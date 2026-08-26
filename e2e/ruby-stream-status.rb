# Loopback verification that the Ruby SDK never yields SSE events from a
# response it ultimately rejects: 1xx/3xx/4xx/5xx stream bodies must produce
# an error and ZERO decoded events, even when the body is SSE-shaped.
# Run: ruby -Igen/ruby/lib e2e/ruby-stream-status.rb
require "socket"
require "json"
require "cadenya"

def serve_once(raw_response)
  server = TCPServer.new("127.0.0.1", 0)
  port = server.addr[1]
  thread = Thread.new do
    sock = server.accept
    sock.readpartial(65_536) # consume the request
    sock.write(raw_response)
    sock.close
    server.close
  end
  [port, thread]
end

def http(status_line, content_type, body)
  "HTTP/1.1 #{status_line}\r\n" \
    "Content-Type: #{content_type}\r\n" \
    "Content-Length: #{body.bytesize}\r\n" \
    "Connection: close\r\n\r\n#{body}"
end

SSE_BODY = "data: {\"unexpected\":true}\n\n"

CASES = [
  ["302 with SSE-shaped body", http("302 Found", "text/event-stream", SSE_BODY), Cadenya::APIResponseError],
  ["404 with JSON error body", http("404 Not Found", "application/json", JSON.dump(code: 5, message: "nope")), Cadenya::APIError],
  ["500 with non-JSON body", http("500 Internal Server Error", "text/html", "<h1>boom</h1>"), Cadenya::APIError],
]

failures = 0

CASES.each do |label, response, expected_error|
  port, thread = serve_once(response)
  client = Cadenya::Client.new(api_key: "k", base_url: "http://127.0.0.1:#{port}", workspace_id: "ws_x")
  events = []
  error = nil
  begin
    client.objectives.stream_events("obj_x").each { |event| events << event }
  rescue expected_error => e
    error = e
  end
  thread.join
  if error && events.empty?
    puts "ok  #{label} -> #{error.class} raised, 0 events yielded"
  else
    failures += 1
    puts "FAIL #{label}: error=#{error.inspect} events=#{events.inspect}"
  end
end

# Control: a genuine 200 SSE stream still yields its events.
port, thread = serve_once(http("200 OK", "text/event-stream", SSE_BODY))
client = Cadenya::Client.new(api_key: "k", base_url: "http://127.0.0.1:#{port}", workspace_id: "ws_x")
events = client.objectives.stream_events("obj_x").to_a
thread.join
if events.length == 1
  puts "ok  200 control -> 1 event yielded"
else
  failures += 1
  puts "FAIL 200 control: #{events.inspect}"
end

# close() from the consuming block: iteration ends cleanly after the current
# event even though the body contains more, and no error escapes.
three_events = "data: {\"n\":1}\n\ndata: {\"n\":2}\n\ndata: {\"n\":3}\n\n"
port, thread = serve_once(http("200 OK", "text/event-stream", three_events))
client = Cadenya::Client.new(api_key: "k", base_url: "http://127.0.0.1:#{port}", workspace_id: "ws_x")
stream = client.objectives.stream_events("obj_x")
seen = []
stream.each do |event|
  seen << event
  stream.close
end
thread.join
if seen.length == 1 && stream.closed?
  puts "ok  close() -> stopped after 1 of 3 events, no error"
else
  failures += 1
  puts "FAIL close(): saw #{seen.length} events, closed=#{stream.closed?}"
end

if failures.zero?
  puts "\nruby stream status matrix: all cases passed"
else
  abort "\nruby stream status matrix: #{failures} failure(s)"
end
