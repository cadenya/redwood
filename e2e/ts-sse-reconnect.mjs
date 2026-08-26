// Durable gate: TypeScript SSE auto-reconnect. A MID-STREAM transport drop
// resumes from the last received event id (EventSource semantics); a clean
// EOF, close(), and reconnect:false never reconnect. Runs against the built
// dist. Run: node e2e/ts-sse-reconnect.mjs
import { createServer } from 'node:http';

const { default: Cadenya } = await import('../gen/typescript/dist/index.js');

const failures = [];

function sseServer(handler) {
  const state = { connections: [], server: undefined };
  state.server = createServer((req, res) => {
    state.connections.push({ lastEventId: req.headers['last-event-id'] });
    handler(state.connections.length, req, res);
  });
  return new Promise((resolve) => {
    state.server.listen(0, '127.0.0.1', () => resolve(state));
  });
}

function client(state, extra = {}) {
  return new Cadenya({
    apiKey: 'probe',
    workspaceId: 'w',
    baseURL: `http://127.0.0.1:${state.server.address().port}`,
    ...extra,
  });
}

const EVENT = (id) => `id: ${id}\nretry: 10\ndata: {"kind":"probe"}\n\n`;

// --- 1) Drop after the first event: reconnect resumes with Last-Event-ID.
{
  const state = await sseServer((n, req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    if (n === 1) {
      res.write(EVENT('e1'));
      // Mid-stream transport drop, not a clean end.
      setTimeout(() => res.socket.destroy(), 30);
    } else {
      res.write(EVENT('e2'));
      res.end();
    }
  });
  const stream = await client(state).objectives.streamEvents('obj-1');
  const seen = [];
  for await (const event of stream) seen.push(event);
  if (seen.length !== 2) failures.push(`reconnect: expected 2 events, got ${seen.length}`);
  if (state.connections.length !== 2) failures.push(`reconnect: expected 2 connections, got ${state.connections.length}`);
  if (state.connections[1]?.lastEventId !== 'e1') {
    failures.push(`reconnect: second request Last-Event-ID was ${state.connections[1]?.lastEventId}, want e1`);
  }
  if (stream.lastEventId !== 'e2') failures.push(`reconnect: checkpoint ${stream.lastEventId}, want e2`);
  state.server.close();
  console.error(`case 1 done`);
}

// --- 2) Clean EOF never reconnects, even with reconnect available.
{
  const state = await sseServer((n, req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write(EVENT('only'));
    res.end();
  });
  const stream = await client(state).objectives.streamEvents('obj-1');
  const seen = [];
  for await (const event of stream) seen.push(event);
  await new Promise((r) => setTimeout(r, 100));
  if (seen.length !== 1) failures.push(`clean EOF: expected 1 event, got ${seen.length}`);
  if (state.connections.length !== 1) failures.push(`clean EOF reconnected: ${state.connections.length} connections`);
  state.server.close();
  console.error(`case 2 done`);
}

// --- 3) reconnect: false surfaces the drop as APIConnectionError.
{
  const state = await sseServer((n, req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write(EVENT('e1'));
    setTimeout(() => res.socket.destroy(), 30);
  });
  const stream = await client(state).objectives.streamEvents('obj-1', {}, { reconnect: false });
  let error;
  try {
    for await (const _event of stream) {
      // consume
    }
  } catch (err) {
    error = err;
  }
  if (error?.name !== 'APIConnectionError') {
    failures.push(`reconnect:false: expected APIConnectionError, got ${error?.name ?? 'no error'}`);
  }
  if (state.connections.length !== 1) failures.push(`reconnect:false made ${state.connections.length} connections`);
  state.server.close();
  console.error(`case 3 done`);
}

// --- 4) close() during the stream never reconnects.
{
  const state = await sseServer((n, req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    res.write(EVENT('e1'));
    // Keep the connection open; the client closes it.
  });
  const stream = await client(state).objectives.streamEvents('obj-1');
  const seen = [];
  for await (const event of stream) {
    seen.push(event);
    await stream.close();
  }
  await new Promise((r) => setTimeout(r, 150));
  if (seen.length !== 1) failures.push(`close: expected 1 event, got ${seen.length}`);
  if (state.connections.length !== 1) failures.push(`close() reconnected: ${state.connections.length} connections`);
  state.server.close();
  console.error(`case 4 done`);
}

// --- 5) The reconnect budget is bounded: connections that die without
// delivering a single chunk never reset the counter, so a permanently
// broken server surfaces APIConnectionError after MAX attempts.
{
  const state = await sseServer((n, req, res) => {
    res.writeHead(200, { 'content-type': 'text/event-stream' });
    if (n === 1) {
      res.write(EVENT('e1')); // retry: 10 keeps the backoff fast
      setTimeout(() => res.socket.destroy(), 10);
    } else {
      // Headers only, then die: no chunk ever arrives on reconnects.
      setTimeout(() => res.socket.destroy(), 5);
    }
  });
  const stream = await client(state).objectives.streamEvents('obj-1');
  let error;
  const seen = [];
  try {
    for await (const event of stream) seen.push(event);
  } catch (err) {
    error = err;
  }
  if (seen.length !== 1) failures.push(`bounded: expected 1 event, got ${seen.length}`);
  if (error?.name !== 'APIConnectionError') {
    failures.push(`bounded: expected APIConnectionError after budget, got ${error?.name ?? 'no error'}`);
  }
  // Between 2 and 6 server-side connections: transport handshake failures
  // (dead pooled sockets) also consume budget without reaching the server.
  if (state.connections.length < 2 || state.connections.length > 6) {
    failures.push(`bounded: expected 2-6 connections, got ${state.connections.length}`);
  }
  state.server.close();
  console.error(`case 5 done`);
}

if (failures.length) {
  console.log(failures.join('\n'));
  console.error('ts sse reconnect gate: FAILED');
  process.exit(1);
}
console.log('ts sse reconnect gate: all cases passed');
