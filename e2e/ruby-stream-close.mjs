// Durable Gate-4 regression: Ruby Stream#close must interrupt a read parked
// on a silent socket, and caller-owned transports must never be silently
// bypassed for SSE. Runs against the INSTALLED gem (conformance gem home).
// Run: node e2e/ruby-stream-close.mjs
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { provisionRuby } from './conformance/provision.mjs';

const root = new URL('..', import.meta.url).pathname;
const { ruby, gemHome } = provisionRuby(join(root, 'gen/ruby'));

const PROBE = String.raw`
require "socket"
require "cadenya"

failures = []

# --- 1) Silent-socket close: 200 SSE headers, no body byte, held open. -----
server = TCPServer.new("127.0.0.1", 0)
port = server.addr[1]
server_saw = Queue.new
accepted = Queue.new
srv = Thread.new do
  conn = server.accept
  loop { break if conn.readpartial(4096).include?("\r\n\r\n") }
  conn.write("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
  conn.flush
  accepted << true
  begin
    conn.readpartial(1)
    server_saw << :data
  rescue EOFError, Errno::ECONNRESET
    server_saw << :closed
  end
end

client = Cadenya::Client.new(api_key: "probe", workspace_id: "w", base_url: "http://127.0.0.1:#{port}")
stream = client.objectives.stream_events("obj-1")
events = []
entered = Queue.new
consumer = Thread.new do
  entered << true
  stream.each { |e| events << e }
  :done
end
entered.pop
accepted.pop # server sent headers; the consumer's read is (about to be) parked
sleep 0.5    # allow the consumer to actually block on the silent socket
failures << "consumer not alive/blocked before close" unless consumer.alive?

t0 = Process.clock_gettime(Process::CLOCK_MONOTONIC)
stream.close
close_ms = (Process.clock_gettime(Process::CLOCK_MONOTONIC) - t0) * 1000
failures << "close blocked #{close_ms.round}ms" if close_ms > 1000

if consumer.join(3).nil?
  failures << "consumer did not unblock within 3s of close"
  consumer.kill
elsif consumer.value != :done
  failures << "close surfaced an error: #{consumer.value.inspect}"
end
failures << "event yielded on silent stream" unless events.empty?
failures << "server never observed socket closure" unless server_saw.pop == :closed
stream.close # repeated close: idempotent, no error
srv.join(1)

# --- 2) Reset WITHOUT close keeps the stable transport error. --------------
server2 = TCPServer.new("127.0.0.1", 0)
srv2 = Thread.new do
  conn = server2.accept
  loop { break if conn.readpartial(4096).include?("\r\n\r\n") }
  conn.write("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
  conn.flush
  sleep 0.2
  conn.close
end
client2 = Cadenya::Client.new(api_key: "probe", workspace_id: "w", base_url: "http://127.0.0.1:#{server2.addr[1]}")
begin
  client2.objectives.stream_events("obj-1", request_options: { reconnect: false }).each { |_e| }
  failures << "reset did not raise"
rescue Cadenya::APIConnectionError
  # expected
rescue StandardError => e
  failures << "reset raised #{e.class}, not APIConnectionError"
end
srv2.join(1)

# --- 3) close before iteration opens no connection. ------------------------
server3 = TCPServer.new("127.0.0.1", 0)
opened = false
Thread.new { server3.accept; opened = true }
client3 = Cadenya::Client.new(api_key: "probe", workspace_id: "w", base_url: "http://127.0.0.1:#{server3.addr[1]}")
s3 = client3.objectives.stream_events("obj-1")
s3.close
s3.each { |_e| }
sleep 0.2
failures << "close-before-iteration opened a connection" if opened

# --- 4) Caller-supplied connection: SSE must NOT silently reroute. ---------
conn4 = Faraday.new(url: "http://127.0.0.1:9")
client4 = Cadenya::Client.new(api_key: "probe", workspace_id: "w",
                              base_url: "http://127.0.0.1:9", connection: conn4)
begin
  client4.objectives.stream_events("obj-1").each { |_e| }
  failures << "connection: without stream_transport: did not raise"
rescue ArgumentError => e
  failures << "wrong message: #{e.message}" unless e.message.include?("stream_transport")
rescue StandardError => e
  failures << "raised #{e.class}, not the configuration ArgumentError"
end

# --- 5) Injected stream transport is used; no implicit Net::HTTP opened. ---
recorded = []
fake = Object.new
fake.define_singleton_method(:stream) do |method:, uri:, headers:, body:, cancel: nil, open_timeout: 60, &on_chunk|
  recorded << { method: method, path: uri.path, has_cancel: !cancel.nil? }
  on_chunk.call("data: {\"objectiveEvent\":null}\n\n")
  [200, nil]
end
tcp_probe = TCPServer.new("127.0.0.1", 0)
implicit = false
Thread.new { tcp_probe.accept; implicit = true }
client5 = Cadenya::Client.new(api_key: "probe", workspace_id: "w",
                              base_url: "http://127.0.0.1:#{tcp_probe.addr[1]}",
                              connection: Faraday.new(url: "http://127.0.0.1:9"),
                              stream_transport: fake)
count = 0
client5.objectives.stream_events("obj-1").each { |_e| count += 1 }
sleep 0.2
failures << "injected transport not used" if recorded.empty?
failures << "no cancel handle passed to injected transport" unless recorded.first&.fetch(:has_cancel)
failures << "implicit connection opened despite injected transport" if implicit
failures << "expected 1 event via injected transport, got #{count}" unless count == 1

# --- 6) Finite multi-event single chunk, close after FIRST event. ----------
# All three events arrive in one chunk; close inside the consumer block must
# prevent the already-buffered second event from ever being decoded.
server6 = TCPServer.new("127.0.0.1", 0)
srv6 = Thread.new do
  conn = server6.accept
  loop { break if conn.readpartial(4096).include?("\r\n\r\n") }
  events3 = (1..3).map { |i| "data: {\"objectiveEvent\":null}\n\n" }.join
  body = events3
  conn.write("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: #{body.bytesize}\r\n\r\n#{body}")
  conn.flush
  conn.close
end
client6 = Cadenya::Client.new(api_key: "probe", workspace_id: "w", base_url: "http://127.0.0.1:#{server6.addr[1]}")
stream6 = client6.objectives.stream_events("obj-1")
seen = 0
stream6.each do |_e|
  seen += 1
  stream6.close
end
failures << "close after first event leaked #{seen - 1} more event(s)" unless seen == 1
srv6.join(1)

# --- 7) Auto-reconnect: a mid-stream drop resumes with Last-Event-ID.
server7 = TCPServer.new("127.0.0.1", 0)
resume_headers = Queue.new
srv7 = Thread.new do
  2.times do |i|
    conn = server7.accept
    request = +""
    loop do
      request << conn.readpartial(4096)
      break if request.include?("\r\n\r\n")
    end
    resume_headers << request[/^Last-Event-ID: (.*)$/i, 1]&.strip
    conn.write("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n")
    body = i.zero? ? "id: e1\nretry: 10\ndata: {\"objectiveEvent\":null}\n\n" : "id: e2\ndata: {\"objectiveEvent\":null}\n\n"
    chunk = "#{body.bytesize.to_s(16)}\r\n#{body}\r\n"
    conn.write(chunk)
    conn.flush
    if i.zero?
      sleep 0.1
      conn.close # mid-stream drop, chunked stream unterminated
    else
      conn.write("0\r\n\r\n")
      conn.close
    end
  end
end
client7 = Cadenya::Client.new(api_key: "probe", workspace_id: "w", base_url: "http://127.0.0.1:#{server7.addr[1]}")
stream7 = client7.objectives.stream_events("obj-1")
events7 = stream7.each_event.to_a
failures << "resume: expected 2 events, got #{events7.length}" unless events7.length == 2
first_resume = resume_headers.pop
second_resume = resume_headers.pop
failures << "resume: first request carried Last-Event-ID #{first_resume.inspect}" unless first_resume.nil?
failures << "resume: second request Last-Event-ID was #{second_resume.inspect}, want e1" unless second_resume == "e1"
failures << "resume: checkpoint #{stream7.last_event_id.inspect}, want e2" unless stream7.last_event_id == "e2"
srv7.join(2)

if failures.empty?
  puts "ruby stream close gate: all cases passed"
else
  puts failures
  exit 1
end
`;

const dir = mkdtempSync(join(tmpdir(), 'redwood-rb-stream-close-'));
const probe = join(dir, 'probe.rb');
writeFileSync(probe, PROBE);
const r = spawnSync(ruby, [probe], {
  encoding: 'utf8',
  timeout: 120_000,
  cwd: dir,
  env: { ...process.env, GEM_HOME: gemHome, GEM_PATH: gemHome, RUBYLIB: '' },
});
process.stdout.write(r.stdout ?? '');
process.stderr.write(r.stderr ?? '');
process.exit(r.status ?? 1);
