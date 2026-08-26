# frozen_string_literal: true

# The hand-written HTTP core: Faraday transport, retries with backoff and
# Retry-After, error mapping to the API's google.rpc Status payload, and
# client-level default params.

require "cgi"
require "faraday"
require "json"
require "net/http"
require "time"
require "uri"

module RedwoodModule
  # A non-2xx response carrying the API's rpc Status payload.
  class APIError < StandardError
    attr_reader :status_code, :code, :details

    def initialize(status_code, code: nil, message: nil, details: nil)
      super(message || "HTTP #{status_code}")
      @status_code = status_code
      @code = code
      @details = details
    end
  end

  # The request never produced an HTTP response.
  class APIConnectionError < StandardError; end

  # The server answered, but not with the JSON the API contract promises
  # (e.g. an unfollowed redirect or a non-JSON success body).
  class APIResponseError < StandardError
    attr_reader :status_code, :body

    def initialize(status_code, message, body: nil)
      super(message)
      @status_code = status_code
      @body = body
    end
  end

  module Util
    module_function

    def path_escape(value)
      CGI.escape(value.to_s).gsub("+", "%20")
    end

    # Encode one path segment, rejecting blank values at the boundary — an
    # empty segment silently rewrites the route (/parents//children).
    def path_param(name, value)
      text = value.to_s
      raise ArgumentError, "missing required path parameter #{name}" if text.strip.empty?

      path_escape(text)
    end

    # RFC 3339 with caller-supplied fractional seconds preserved (trailing
    # fractional zeros trimmed): plain iso8601 would silently truncate to
    # whole seconds.
    def timestamp(value)
      value.iso8601(9).sub(/\.(\d*?)0+(?=Z|[+-])/) { Regexp.last_match(1).empty? ? "" : ".#{Regexp.last_match(1)}" }
    end

    # Recursively prepare a body value for JSON: Time -> RFC 3339.
    def deep_jsonify(value)
      case value
      when Time then timestamp(value)
      when Hash then value.to_h { |k, v| [k, deep_jsonify(v)] }
      when Array then value.map { |v| deep_jsonify(v) }
      else value
      end
    end

    # Unwrap decoded value objects (and containers of them) into plain
    # snake-keyed hashes so responses round-trip into request params.
    def plain(value)
      case value
      when Hash then value.to_h { |k, v| [k, plain(v)] }
      when Array then value.map { |v| plain(v) }
      else
        value.class.name.to_s.start_with?("#{name.split("::").first}::Types") ? value.to_h : value
      end
    end

    def query_value(value)
      case value
      when true, false then value.to_s
      when Time then timestamp(value)
      else value.to_s
      end
    end
  end

  class Core
    # Presence sentinel: nil can be a legitimate JSON body (null), and false
    # is a legitimate boolean body -- truthiness cannot mean "no body".
    UNSET = Object.new.freeze

    RETRYABLE_STATUS = [408, 409, 429, 500, 502, 503, 504].freeze

    # Automatic retries apply only to idempotent methods: a POST/PATCH that
    # succeeds server-side but loses its response would be executed twice.
    IDEMPOTENT_METHODS = %i[get head put delete].freeze

    def initialize(base_url:, auth_header:, max_retries:, defaults:, user_agent:, connection: nil, stream_transport: nil)
      @auth_header = auth_header
      # Finite bounded integer: negatives, floats, huge values, Infinity,
      # NaN, and non-numerics all normalize (to_i alone raises
      # FloatDomainError on non-finite floats).
      @max_retries =
        if max_retries.is_a?(Numeric) && (!max_retries.is_a?(Float) || max_retries.finite?)
          max_retries.to_i.clamp(0, 10)
        else
          0
        end
      @defaults = defaults
      @user_agent = user_agent
      # No connection-wide timeout: it would also cap SSE stream lifetime.
      # Ordinary requests get a per-request timeout in perform — but only on
      # the connection Core created; a caller-supplied connection's timeout
      # policy (including for streams) is authoritative and never touched.
      # Validate the STRUCTURE once: a query/fragment/userinfo base would
      # silently corrupt routing, and Faraday drops a base path prefix for
      # root-relative request paths -- so the prefix is captured here and
      # prepended to every operation path instead. Absolute http(s) with a
      # host is required.
      parsed = begin
        URI.parse(base_url)
      rescue URI::InvalidURIError
        raise ArgumentError, "base_url #{base_url.inspect} is not a valid URL"
      end
      unless parsed.is_a?(URI::HTTP) && !parsed.host.to_s.empty?
        raise ArgumentError, "base_url #{base_url.inspect} must be an absolute http(s) URL with a host"
      end
      if parsed.userinfo || parsed.query || parsed.fragment
        raise ArgumentError, "base_url #{base_url.inspect} must not carry userinfo, query, or fragment"
      end
      @path_prefix = parsed.path.chomp("/")
      origin = "#{parsed.scheme}://#{parsed.host}#{parsed.port == parsed.default_port ? "" : ":#{parsed.port}"}"
      @origin = origin
      @owns_conn = connection.nil?
      @stream_transport = stream_transport
      @conn = connection || Faraday.new(url: origin) do |f|
        f.request :url_encoded
      end
    end

    def resolve_default(wire_name, env_var, value)
      # Presence and validity are separate: an EXPLICITLY supplied blank is
      # a configuration error and must never fall back to ambient client/
      # environment state (that could silently target another tenant/scope).
      unless value.nil?
        trimmed = value.to_s.strip
        raise ArgumentError, "#{wire_name} must not be blank" if trimmed.empty?

        return trimmed
      end

      resolved = @defaults[wire_name].to_s.strip
      resolved = ENV.fetch(env_var, "").strip if resolved.empty?
      if resolved.empty?
        raise ArgumentError, "missing #{wire_name}: pass it, set it on the client, or set #{env_var}"
      end
      resolved
    end

    # Per-call transport controls. A strict allow-list so a misspelled API
    # parameter can never be silently swallowed as an "option".
    REQUEST_OPTION_KEYS = %i[headers timeout max_retries reconnect].freeze

    def self.validate_request_options(opts)
      return nil if opts.nil?
      raise ArgumentError, "request_options must be a Hash" unless opts.is_a?(Hash)

      unknown = opts.keys - REQUEST_OPTION_KEYS
      unless unknown.empty?
        raise ArgumentError,
              "unknown request_options key(s): #{unknown.map(&:inspect).join(', ')} (supported: #{REQUEST_OPTION_KEYS.map(&:inspect).join(', ')})"
      end
      if opts.key?(:headers)
        h = opts[:headers]
        raise ArgumentError, "request_options[:headers] must be a Hash of String => String" unless h.is_a?(Hash) && h.all? { |k, v| k.is_a?(String) && v.is_a?(String) }
      end
      if opts.key?(:timeout)
        t = opts[:timeout]
        unless t.is_a?(Numeric) && !t.is_a?(Complex) && (!t.is_a?(Float) || t.finite?) && t.positive?
          raise ArgumentError, "request_options[:timeout] must be a positive finite number of seconds"
        end
      end
      if opts.key?(:reconnect) && ![true, false].include?(opts[:reconnect])
        raise ArgumentError, "request_options[:reconnect] must be true or false"
      end
      if opts.key?(:max_retries)
        r = opts[:max_retries]
        raise ArgumentError, "request_options[:max_retries] must be an Integer >= 0" unless r.is_a?(Integer) && r >= 0
      end
      opts
    end

    def request(method, path, query: nil, body: UNSET, expects_body: true, request_options: nil)
      request_options = Core.validate_request_options(request_options)
      response = send_with_retries(method, path, query, body, extra_headers: nil, stream: nil, request_options: request_options)
      raise_for_status(response.status, response.body)
      raw = response.body
      # Branch on the GENERATED expectation, not the HTTP status: a void
      # method accepts 204/empty, but an output-bearing method requires a
      # JSON document -- empty/null would fabricate a resource (or an empty
      # page) outside the declared contract.
      return nil unless expects_body

      if response.status == 204 || raw.nil? || raw.strip.empty?
        raise APIResponseError.new(
          response.status,
          "HTTP #{response.status} with an empty body where a JSON response was expected"
        )
      end
      parsed = begin
        JSON.parse(raw)
      rescue JSON::ParserError
        raise APIResponseError.new(
          response.status,
          "response body is not valid JSON",
          body: raw.to_s[0, 2000]
        )
      end
      if parsed.nil?
        raise APIResponseError.new(
          response.status,
          "HTTP #{response.status} with a JSON null body where a JSON response was expected"
        )
      end
      parsed
    end

    # Cancellation handle for a streaming request. `cancel!` from any thread
    # tears the transport down, which unblocks a read parked on a silent
    # socket — polling a flag between chunks cannot do that.
    class CancelHandle
      def initialize
        @mutex = Mutex.new
        @cancelled = false
        @teardown = nil
      end

      def attach(&teardown)
        fire = @mutex.synchronize do
          @teardown = teardown
          @cancelled
        end
        safe_call(teardown) if fire
        nil
      end

      def cancel!
        teardown = @mutex.synchronize do
          @cancelled = true
          @teardown
        end
        safe_call(teardown)
        nil
      end

      def cancelled?
        @mutex.synchronize { @cancelled }
      end

      private

      def safe_call(teardown)
        teardown&.call
      rescue StandardError, IOError
        nil
      end
    end

    # Streaming request for SSE: yields body chunks as they arrive. Chunks
    # reach the caller's parser ONLY for 2xx responses — anything else
    # (1xx/3xx included) is buffered as a bounded diagnostic body and raised,
    # so application code never observes events from a response the SDK
    # ultimately rejects. `headers` may carry Last-Event-ID for resumption.
    MAX_ERROR_BODY = 65_536

    # Deadline for the bounded diagnostic read of a non-2xx stream body
    # (seconds): together with MAX_ERROR_BODY it bounds TIME and MEMORY.
    STREAM_ERROR_BODY_SECONDS = 10

    # The default SSE transport: interruptible Net::HTTP. Implements the
    # injectable stream-transport interface — `stream` yields body chunks
    # for ANY status and returns [status, diagnostic_error_body_or_nil];
    # the cancel handle's teardown must unblock a parked read.
    class DefaultStreamTransport
      def stream(method:, uri:, headers:, body:, cancel: nil, open_timeout: 60, &on_chunk)
        http = Net::HTTP.new(uri.host, uri.port)
        http.use_ssl = uri.scheme == "https"
        # A per-request timeout bounds only the OPEN phase for streams: a
        # deadline on a healthy quiet SSE body would be a lifetime limit.
        http.open_timeout = open_timeout
        # The HANDSHAKE (response headers) is bounded — a half-open server
        # must not hang the request forever. Only the BODY read is unbounded:
        # a healthy SSE stream may stay silent indefinitely.
        http.read_timeout = open_timeout

        req = Net::HTTP.const_get(method.to_s.capitalize).new(uri.request_uri)
        headers.each { |k, v| req[k] = v }
        req.body = body if body

        status = nil
        error_body = nil
        begin
          http.start
          # Registered only once the connection exists; a close that already
          # happened fires the teardown immediately inside attach.
          cancel&.attach do
            http.finish
          rescue IOError
            nil
          end
          http.request(req) do |response|
            status = response.code.to_i
            if (200...300).cover?(status)
              http.read_timeout = nil
              response.read_body(&on_chunk)
            else
              # Diagnostic body read bounded in BYTES and TIME: the stream's
              # read timeout was disabled before the status was known, so a
              # server that stalls after failure headers must not hang this.
              http.read_timeout = STREAM_ERROR_BODY_SECONDS
              error_body = +""
              begin
                response.read_body do |chunk|
                  if error_body.bytesize < MAX_ERROR_BODY
                    error_body << chunk.byteslice(0, MAX_ERROR_BODY - error_body.bytesize)
                  end
                  break if error_body.bytesize >= MAX_ERROR_BODY
                end
              rescue Net::ReadTimeout, IOError, EOFError
                # Partial diagnostics still name the failure.
              end
            end
          end
        ensure
          begin
            http.finish if http.started?
          rescue IOError
            nil
          end
        end
        [status, error_body]
      end
    end

    def stream_request(method, path, query: nil, body: UNSET, headers: nil, request_options: nil, cancel: nil, &on_chunk)
      request_options = Core.validate_request_options(request_options)
      # Caller-owned transports are NEVER silently bypassed: Faraday's
      # adapter interface exposes no mid-read cancellation handle, so a
      # client built on connection: must configure an explicit
      # stream_transport: for SSE (or omit connection:).
      transport =
        if @stream_transport
          @stream_transport
        elsif !@owns_conn
          raise ArgumentError,
                "streaming with a caller-supplied connection: requires an explicit stream_transport: " \
                "(the connection's adapter cannot expose mid-read cancellation); " \
                "pass stream_transport: alongside connection:, or omit connection:"
        else
          DefaultStreamTransport.new
        end

      path = @path_prefix + path unless @path_prefix.empty?
      uri = URI.parse(@origin + path)
      if query
        pairs = []
        query.reject { |_k, v| v.nil? }.each do |k, v|
          (v.is_a?(Array) ? v : [v]).each { |item| pairs << [k.to_s, Util.query_value(item)] }
        end
        uri.query = URI.encode_www_form(pairs) unless pairs.empty?
      end

      request_headers = headers(true)
      merge_ci = lambda do |extra|
        extra.each do |k, v|
          request_headers.delete_if { |existing, _| existing.casecmp?(k) }
          request_headers[k] = v
        end
      end
      merge_ci.call(request_options[:headers]) if request_options && request_options[:headers]
      merge_ci.call(headers) if headers

      encoded_body = nil
      unless body.equal?(UNSET)
        request_headers["Content-Type"] = "application/json"
        encoded_body = begin
          JSON.generate(Util.deep_jsonify(body))
        rescue JSON::GeneratorError => e
          raise ArgumentError, "request body is not JSON-serializable: #{e.message}"
        end
      end

      status, error_body = begin
        transport.stream(
          method: method,
          uri: uri,
          headers: request_headers,
          body: encoded_body,
          cancel: cancel,
          open_timeout: (request_options && request_options[:timeout]) || 60,
          &on_chunk
        )
      rescue APIError, APIResponseError
        raise
      rescue StandardError, IOError, EOFError => e
        # A DELIBERATE close tore the socket down: normal termination, never
        # a transport error. Anything else keeps the stable error family.
        return nil if cancel&.cancelled?

        raise APIConnectionError, e.message
      end
      return nil if cancel&.cancelled?
      raise APIConnectionError, "stream produced no HTTP status" if status.nil?

      raise_for_status(status, error_body || "") unless (200...300).cover?(status)
      nil
    end

    private

    def headers(stream)
      h = {
        "User-Agent" => @user_agent,
        "Accept" => stream ? "text/event-stream" : "application/json"
      }
      h[@auth_header[0]] = @auth_header[1] unless @auth_header[0].to_s.empty?
      h
    end

    def send_with_retries(method, path, query, body, extra_headers:, stream:, request_options: nil)
      # Base path prefix support: prepend so a base of .../prefix routes to
      # /prefix/v1/... instead of silently dropping the prefix.
      path = @path_prefix + path unless @path_prefix.empty?
      # Streams never auto-retry: chunks may already have reached the
      # caller's parser, and replaying from byte zero would double-deliver
      # events. Resume explicitly with last_event_id instead.
      # Streams NEVER auto-retry, even with an explicit override: delivered
      # chunks cannot be un-delivered. Elsewhere an explicit per-request
      # value is the caller opting this exact call in/out — it overrides the
      # idempotency default, mutations included (bodies are fixed strings,
      # replay is safe).
      max_retries = if stream
                      0
                    elsif request_options&.key?(:max_retries)
                      request_options[:max_retries].clamp(0, 10)
                    elsif IDEMPOTENT_METHODS.include?(method.to_sym)
                      @max_retries
                    else
                      0
                    end
      attempt = 0
      loop do
        begin
          response = perform(method, path, query, body, extra_headers, stream, request_options)
        rescue Faraday::Error => e
          raise APIConnectionError, e.message if attempt >= max_retries

          sleep(backoff_seconds(attempt, nil))
          attempt += 1
          next
        end
        if RETRYABLE_STATUS.include?(response.status) && attempt < max_retries
          sleep(backoff_seconds(attempt, response.headers["retry-after"]))
          attempt += 1
          next
        end
        return response
      end
    end

    def perform(method, path, query, body, extra_headers, stream, request_options = nil)
      request_headers = headers(!stream.nil?)
      # Precedence: generated defaults < auth < per-request option headers <
      # semantic extra_headers (Last-Event-ID resume state stays
      # authoritative). Replacement is case-insensitive as HTTP requires.
      merge_ci = lambda do |extra|
        extra.each do |k, v|
          request_headers.delete_if { |existing, _| existing.casecmp?(k) }
          request_headers[k] = v
        end
      end
      merge_ci.call(request_options[:headers]) if request_options && request_options[:headers]
      merge_ci.call(extra_headers) if extra_headers
      @conn.run_request(method, path, nil, request_headers) do |req|
        # FlatParamsEncoder is wire-format correctness (repeated params must
        # not grow [] suffixes), so it applies to every request. Timeouts are
        # policy: applied only to the connection Core owns — streams get a
        # bounded open timeout but no whole-request deadline.
        req.options.params_encoder = Faraday::FlatParamsEncoder
        if @owns_conn
          if stream
            req.options.open_timeout = 60
          else
            req.options.timeout = 60
          end
        end
        # An EXPLICIT per-request timeout overrides even a caller-owned
        # connection's policy for this one call. For streams it bounds only
        # the open phase: a deadline on a healthy SSE body would turn a
        # transport control into a lifetime limit.
        if request_options && request_options[:timeout]
          if stream
            req.options.open_timeout = request_options[:timeout]
          else
            req.options.timeout = request_options[:timeout]
          end
        end
        if query
          clean = query.reject { |_k, v| v.nil? }
          req.params.update(clean.transform_values { |v| v.is_a?(Array) ? v.map { |i| Util.query_value(i) } : Util.query_value(v) })
        end
        unless body.equal?(UNSET)
          # Serialize the body EXACTLY: objects, arrays, scalars, booleans,
          # and explicit null are all legitimate whole bodies. Field omission
          # for flattened object bodies happens in the generated resource
          # methods, never here.
          req.headers["Content-Type"] = "application/json"
          req.body = begin
            JSON.generate(Util.deep_jsonify(body))
          rescue JSON::GeneratorError => e
            raise ArgumentError, "request body is not JSON-serializable: #{e.message}"
          end
        end
        req.options.on_data = proc { |chunk, _size, env| stream.call(chunk, env) } if stream
      end
    end

    def raise_for_status(status, raw_body)
      return if status >= 200 && status < 300

      if status < 400
        # An unfollowed redirect is a protocol surprise, not an API error.
        raise APIResponseError.new(
          status,
          "unexpected non-2xx response (HTTP #{status})",
          body: raw_body.to_s[0, 2000]
        )
      end

      parsed = begin
        JSON.parse(raw_body.to_s)
      rescue JSON::ParserError
        {}
      end
      parsed = {} unless parsed.is_a?(Hash)
      raise APIError.new(
        status,
        code: parsed["code"],
        message: parsed["message"],
        details: parsed["details"]
      )
    end

    def backoff_seconds(attempt, retry_after)
      if retry_after
        # Retry-After is either delta-seconds or an HTTP-date.
        seconds = Float(retry_after, exception: false)
        return [seconds, 60.0].min if seconds && seconds >= 0

        begin
          delta = Time.httpdate(retry_after) - Time.now
          return [delta, 60.0].min if delta.positive?
        rescue ArgumentError
          nil
        end
      end
      base = [0.5 * (2**attempt), 8.0].min
      base * (0.5 + rand / 2)
    end
  end
end
