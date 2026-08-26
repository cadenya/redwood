# frozen_string_literal: true

# Server-sent events over Faraday. Parses the wire format incrementally —
# multi-line data, event names, comments, CRLF. Each event's `data` is
# JSON-decoded and passed through the stream's decoder.

require "json"

module RedwoodModule
  ServerSentEvent = Struct.new(:event, :data, :id)

  class Stream
    include Enumerable

    # Internal control-flow signal for close; never escapes this class.
    Closed = Class.new(StandardError)
    private_constant :Closed

    # The resume checkpoint: seeded from the id this stream was resumed with,
    # then updated by `id:` fields (persistent per the SSE spec). Pass it as
    # `last_event_id:` to resume after a disconnect.
    attr_reader :last_event_id

    # `perform` runs the HTTP request: it receives a Core::CancelHandle to
    # register transport teardown on, and calls its block with each chunk.
    def initialize(decoder, last_event_id: nil, skip_events: [], auto_reconnect: true, &perform)
      @decoder = decoder
      @perform = perform
      @last_event_id = last_event_id
      # Housekeeping event names (the `event:` field) skipped without
      # decoding; their `id:` fields still advance the resume checkpoint.
      @skip_events = skip_events.to_a.freeze
      # Auto-reconnect (EventSource semantics): a MID-STREAM transport drop
      # re-runs @perform from the resume checkpoint. Clean EOF, close, and
      # budget exhaustion never reconnect.
      @auto_reconnect = auto_reconnect
      @retry_hint_ms = nil
      @reconnect_attempts = 0
      @closed = false
      @consumed = false
      @lifecycle = Mutex.new
      @cancel = Core::CancelHandle.new
    end

    # Stop the stream deterministically — safe to call from the consuming
    # block or another thread, INCLUDING while the consumer is blocked on a
    # silent socket: the cancel handle tears the transport down, which
    # unblocks the read immediately. Idempotent; deliberate close is normal
    # termination, never a transport error.
    def close
      handle = @lifecycle.synchronize do
        @closed = true
        @cancel
      end
      handle.cancel!
      nil
    end

    def closed?
      @closed
    end

    # Bounded: 5 consecutive attempts per outage, 500ms*2^n capped 10s (the
    # server's `retry:` hint overrides). Sliced sleeps keep close() prompt.
    MAX_RECONNECTS = 5

    private def reconnect_backoff
      return false unless @auto_reconnect && !@closed
      return false if @reconnect_attempts >= MAX_RECONNECTS

      delay = @retry_hint_ms ? @retry_hint_ms / 1000.0 : [0.5 * (2**@reconnect_attempts), 10.0].min
      @reconnect_attempts += 1
      waited = 0.0
      while waited < delay
        return false if @closed

        step = [0.1, delay - waited].min
        sleep(step)
        waited += step
      end
      return false if @closed

      # A fresh handle per connection: close() must tear down the CURRENT
      # transport, never a dead one from before the drop.
      @lifecycle.synchronize do
        return false if @closed

        @cancel = Core::CancelHandle.new
      end
      true
    end

    def each
      return enum_for(:each) unless block_given?

      each_event { |event| yield event.data }
      # Ruby collection iterators return the receiver in block form.
      self
    end

    def each_event
      return enum_for(:each_event) unless block_given?

      # A Stream wraps ONE HTTP request. Re-enumerating would silently reopen
      # the connection with the ORIGINAL resume header (stale checkpoint) and
      # duplicate events; reconnection is explicit — construct a new stream
      # with `last_event_id: old.last_event_id`. A stream closed before
      # iteration yields nothing and never opens a connection.
      @lifecycle.synchronize do
        return if @closed && !@consumed
        raise IOError, "stream already consumed — reconnect with a new stream using last_event_id: #{@last_event_id.inspect}" if @consumed

        @consumed = true
      end

      buffer = +""
      data_lines = []
      event_name = nil

      flush = proc do
        # Authoritative close check BEFORE decoding: several complete events
        # can arrive in one chunk, and no buffered event may be decoded or
        # yielded once close (from the consumer block or elsewhere) returns.
        raise Closed if @closed

        unless data_lines.empty? || @skip_events.include?(event_name)
          payload = JSON.parse(data_lines.join("\n"))
          # Per the SSE spec the last-event-ID buffer persists across events
          # until another `id:` field changes it (an empty one resets it).
          yield ServerSentEvent.new(event_name, @decoder.call(payload), @last_event_id)
        end
        data_lines = []
        event_name = nil
        raise Closed if @closed
      end

      handle_line = proc do |line|
        if line.empty?
          flush.call
        elsif !line.start_with?(":")
          field, _, value = line.partition(":")
          value = value.delete_prefix(" ")
          case field
          when "data" then data_lines << value
          when "event" then event_name = value
          when "id"
            # Ids containing U+0000 are ignored per the event-stream
            # algorithm; an empty id resets the buffer.
            @last_event_id = value.empty? ? nil : value unless value.include?("\0")
          when "retry"
            # Reconnection-delay hint, honored during auto-reconnect.
            @retry_hint_ms = [value.to_i, 60_000].min if /\A[0-9]+\z/.match?(value)
          end
        end
      end

      # WHATWG event streams terminate lines with LF, CRLF, OR bare CR; a CR
      # at a chunk boundary must wait for the next chunk to see whether an
      # LF follows (CRLF is one terminator, never two).
      next_line = proc do |at_eof|
        index = buffer.index(/[\r\n]/)
        if index.nil?
          nil
        elsif buffer[index] == "\n"
          line = buffer[0...index]
          buffer.slice!(0..index)
          line
        elsif index == buffer.length - 1 && !at_eof
          nil # possible CRLF split across chunks
        else
          line = buffer[0...index]
          consume = buffer[index + 1] == "\n" ? index + 1 : index
          buffer.slice!(0..consume)
          line
        end
      end

      loop do
        begin
          @perform.call(@cancel, @last_event_id) do |chunk|
            # Bytes flowing again: the reconnect budget is per-outage.
            @reconnect_attempts = 0
            raise Closed if @closed
            buffer << chunk
            while (line = next_line.call(false))
              handle_line.call(line)
            end
          end
        rescue Closed
          return
        rescue APIConnectionError
          # Only a MID-STREAM transport drop (or a failed reconnect
          # handshake, which raises the same family and consumes budget)
          # reconnects; HTTP-level failures propagate untouched.
          raise unless reconnect_backoff

          buffer = +""
          data_lines = []
          event_name = nil
          next
        end
        break # clean EOF: API streams may legitimately end
      end
      # A deliberate close may surface as a NORMAL transport return (the
      # cancel handle maps teardown to clean termination); the leftover
      # buffer must still never be processed.
      return if @closed

      while (line = next_line.call(true))
        handle_line.call(line)
      end
      handle_line.call(buffer) unless buffer.empty?
      flush.call
    rescue Closed
      nil
    end
  end
end
