// Runtime smoke test: runs the generated, compiled TypeScript SDK against a
// local mock of the Cadenya API. Exercises auth, body/query/path splitting,
// cursor pagination (auto-iteration), retry-on-429, error mapping, and SSE.
//
// Usage: node e2e/smoke.mjs [path-to-generated-sdk-dist]

import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';
import { createServer } from 'node:http';

// Hermetic: ignore any real credentials in the invoking shell.
delete process.env.CADENYA_API_KEY;
delete process.env.CADENYA_WORKSPACE_ID;
delete process.env.CADENYA_WEBHOOK_SECRET;
delete process.env.CADENYA_BASE_URL;

const distPath = process.argv[2] ?? new URL('../gen/typescript/dist/index.js', import.meta.url).pathname;
const { default: Cadenya, APIError, WebhookVerificationError } = await import(distPath);

let retryHits = 0;
const requests = [];

const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://localhost');
  requests.push({ method: req.method, path: url.pathname, auth: req.headers.authorization });

  // Page 1 + page 2 of objectives.
  if (url.pathname === '/v1/workspaces/ws_1/objectives') {
    const cursor = url.searchParams.get('cursor');
    res.setHeader('content-type', 'application/json');
    if (!cursor) {
      res.end(JSON.stringify({
        items: [{ id: 'obj_1' }, { id: 'obj_2' }],
        pagination: { nextCursor: 'c2' },
      }));
    } else {
      assert.equal(cursor, 'c2');
      assert.equal(url.searchParams.get('limit'), '2', 'original params carry into refetch');
      res.end(JSON.stringify({ items: [{ id: 'obj_3' }], pagination: {} }));
    }
    return;
  }

  // Create: body must contain spec only (workspaceId travels in the path).
  if (url.pathname === '/v1/workspaces/ws_1/agents' && req.method === 'POST') {
    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      const parsed = JSON.parse(body);
      assert.deepEqual(Object.keys(parsed).sort(), ['metadata', 'spec']);
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ metadata: { name: parsed.metadata.name } }));
    });
    return;
  }

  // Retry: fail twice with 429, then succeed.
  if (url.pathname === '/v1/account') {
    if (retryHits++ < 2) {
      res.statusCode = 429;
      res.setHeader('retry-after', '0');
      res.end(JSON.stringify({ code: 8, message: 'slow down' }));
      return;
    }
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ info: 'ok' }));
    return;
  }

  // SSE: comments, event names, multi-line data, CRLF endings.
  if (url.pathname === '/v1/workspaces/ws_1/objectives/obj_1/events:stream') {
    res.setHeader('content-type', 'text/event-stream');
    res.write(': keep-alive\n\n');
    res.write('event: created\r\ndata: {"eventType":"created",\r\ndata:  "sequence":1}\r\n\r\n');
    res.write('data: {"eventType":"updated","sequence":2}\n\n');
    res.end('data: {"eventType":"done","sequence":3}\n\n');
    return;
  }

  // Everything else: a google.rpc.Status error.
  res.statusCode = 404;
  res.setHeader('content-type', 'application/json');
  res.end(JSON.stringify({ code: 5, message: 'not found', details: [] }));
});

await new Promise((resolve) => server.listen(0, resolve));
const baseURL = `http://127.0.0.1:${server.address().port}`;
const client = new Cadenya({ apiKey: 'test-key', baseURL });

// 1. Pagination: for-await walks both pages transparently.
const page = await client.objectives.list({ workspaceId: 'ws_1', limit: 2 });
const ids = [];
for await (const objective of page) ids.push(objective.id);
assert.deepEqual(ids, ['obj_1', 'obj_2', 'obj_3']);
console.log('pagination      ok  (3 items across 2 pages)');

// 2. Auth header present on every request.
assert.ok(requests.every((r) => r.auth === 'Bearer test-key'));
console.log('bearer auth     ok');

// 3. Body/path splitting on create.
const agent = await client.agents.create({
  workspaceId: 'ws_1',
  metadata: { name: 'smoke' },
  spec: { variationSelectionMode: 'VARIATION_SELECTION_MODE_UNSPECIFIED' },
});
assert.equal(agent.metadata.name, 'smoke');
console.log('create+body     ok  (readOnly workspaceId kept out of body)');

// 4. Retries: two 429s then success. Retries default to 0, so the retry
// client opts in explicitly (GET is idempotent, so the client-level setting
// applies without a per-request override).
const retryClient = new Cadenya({ apiKey: 'test-key', baseURL, maxRetries: 2 });
const account = await retryClient.accounts.retrieve();
assert.equal(account.info, 'ok');
assert.equal(retryHits, 3);
console.log('retry-on-429    ok  (2 retries then success)');

// 5. Error mapping.
await assert.rejects(
  () => client.objectives.retrieve('missing', { workspaceId: 'ws_1' }),
  (err) => err instanceof APIError && err.status === 404 && err.code === 5 && err.message === 'not found',
);
console.log('error mapping   ok  (APIError carries rpc Status)');

// 6. SSE parsing: comments skipped, multi-line data joined, CRLF handled.
const stream = await client.objectives.streamEvents('obj_1', { workspaceId: 'ws_1' });
const events = [];
for await (const event of stream) events.push(event);
assert.deepEqual(
  events.map((e) => e.sequence),
  [1, 2, 3],
);
assert.equal(events[0].eventType, 'created');
console.log('sse streaming   ok  (multi-line data, comments, CRLF)');

// 7. Client-level workspaceId default: no params at the call site.
const defaultedClient = new Cadenya({ apiKey: 'test-key', baseURL, workspaceId: 'ws_1' });
const viaDefault = await defaultedClient.objectives.list({ limit: 2 });
assert.equal(viaDefault.items.length, 2);
console.log('client defaults ok  (workspaceId from constructor, none at call site)');

// 8. Missing workspaceId everywhere -> clear error naming the env var.
const bareClient = new Cadenya({ apiKey: 'test-key', baseURL });
await assert.rejects(
  () => bareClient.objectives.list({ limit: 2 }),
  (err) => err.message.includes("Missing 'workspaceId'") && err.message.includes('CADENYA_WORKSPACE_ID'),
);
console.log('missing default ok  (helpful error names CADENYA_WORKSPACE_ID)');

// 9. Webhooks: Standard Webhooks signing -> typed unwrap; tampering rejected.
const secretBytes = Buffer.from('super-secret-signing-key');
const whClient = new Cadenya({
  apiKey: 'test-key',
  baseURL,
  webhookSecret: `whsec_${secretBytes.toString('base64')}`,
});
const webhookPayload = JSON.stringify({
  type: 'objective_event.user_message',
  timestamp: new Date().toISOString(),
  data: { agent: { id: 'agent_1' }, agentVariation: { id: 'av_1' }, objective: { id: 'obj_1' }, objectiveEvent: { data: 'hi' } },
});
const msgId = 'msg_2u4Kq';
const ts = String(Math.floor(Date.now() / 1000));
const signature = createHmac('sha256', secretBytes).update(`${msgId}.${ts}.${webhookPayload}`).digest('base64');
const headers = {
  'webhook-id': msgId,
  'webhook-timestamp': ts,
  'webhook-signature': `v2,bogus v1,${signature}`,
};
const event = await whClient.webhooks.unwrap(webhookPayload, headers);
assert.equal(event.type, 'objective_event.user_message');
assert.equal(event.data.objectiveEvent.data, 'hi');
console.log('webhook unwrap  ok  (typed event, multi-signature header)');

await assert.rejects(
  () => whClient.webhooks.unwrap(webhookPayload.replace('hi', 'evil'), headers),
  (err) => err instanceof WebhookVerificationError,
);
await assert.rejects(
  () => whClient.webhooks.unwrap(webhookPayload, { ...headers, 'webhook-timestamp': String(Math.floor(Date.now() / 1000) - 3600) }),
  (err) => err instanceof WebhookVerificationError && err.message.includes('tolerance'),
);
console.log('webhook reject  ok  (tampered payload, stale timestamp)');

server.close();
console.log('\nall smoke tests passed');
