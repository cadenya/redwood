// Zero-network client-option matrix for the TypeScript SDK: explicit blank
// options must throw at construction (never silently fall back to the
// environment), omitted options read the environment, and a blank env var
// counts as unset. Run: node e2e/ts-config-matrix.mjs
import assert from 'node:assert/strict';

const distPath = new URL('../gen/typescript/dist/index.js', import.meta.url).pathname;
const { default: Cadenya } = await import(distPath);

// Ambient values that a silently-falling-back client would pick up.
process.env.CADENYA_API_KEY = 'env-key';
process.env.CADENYA_BASE_URL = 'https://env.example.test';
process.env.CADENYA_WORKSPACE_ID = 'ws_environment';
process.env.CADENYA_WEBHOOK_SECRET = 'ZW52LXNlY3JldC1lbnYtc2VjcmV0LWVudi1zZWNyZXQ=';

const requests = [];
const stubFetch = async (url) => {
  requests.push(String(url));
  return new Response(JSON.stringify({ items: [], nextCursor: null }), {
    status: 200,
    headers: { 'content-type': 'application/json' },
  });
};

const throws = (label, fn) => {
  assert.throws(fn, /blank|Missing/i, `${label}: expected a construction-time error`);
  console.log(`ok  ${label} -> throws`);
};

// --- Explicit blank/whitespace options: rejected at construction. ---
throws('apiKey: ""', () => new Cadenya({ apiKey: '', fetch: stubFetch }));
throws('apiKey: "   "', () => new Cadenya({ apiKey: '   ', fetch: stubFetch }));
throws('baseURL: ""', () => new Cadenya({ apiKey: 'k', baseURL: '', fetch: stubFetch }));
throws('baseURL: "   "', () => new Cadenya({ apiKey: 'k', baseURL: '   ', fetch: stubFetch }));
throws('workspaceId: ""', () => new Cadenya({ apiKey: 'k', workspaceId: '', fetch: stubFetch }));
throws('workspaceId: "   "', () => new Cadenya({ apiKey: 'k', workspaceId: '   ', fetch: stubFetch }));
throws('webhookSecret: ""', () => new Cadenya({ apiKey: 'k', webhookSecret: '', fetch: stubFetch }));

// --- Omitted options read the environment. ---
{
  requests.length = 0;
  const client = new Cadenya({ fetch: stubFetch });
  await client.agents.list();
  assert.match(requests[0], /^https:\/\/env\.example\.test\//, 'omitted baseURL uses env');
  assert.ok(requests[0].includes('ws_environment'), 'omitted workspaceId uses env');
  console.log('ok  omitted options read env:', requests[0]);
}

// --- Explicit valid options override the environment. ---
{
  requests.length = 0;
  const client = new Cadenya({
    apiKey: 'explicit-key',
    baseURL: 'https://explicit.example.test',
    workspaceId: 'ws_explicit',
    fetch: stubFetch,
  });
  await client.agents.list();
  assert.match(requests[0], /^https:\/\/explicit\.example\.test\//, 'explicit baseURL wins');
  assert.ok(requests[0].includes('ws_explicit'), 'explicit workspaceId wins');
  console.log('ok  explicit options override env:', requests[0]);
}

// --- Blank env vars count as unset (defaults apply; no throw). ---
{
  process.env.CADENYA_BASE_URL = '   ';
  delete process.env.CADENYA_WORKSPACE_ID;
  requests.length = 0;
  const client = new Cadenya({ apiKey: 'k', workspaceId: 'ws_x', fetch: stubFetch });
  await client.agents.list();
  assert.match(requests[0], /^https:\/\/api\.cadenya\.com\//, 'blank env baseURL falls to default');
  console.log('ok  blank env baseURL treated as unset:', requests[0]);
}

// --- Missing API key everywhere still fails fast. ---
{
  delete process.env.CADENYA_API_KEY;
  throws('apiKey omitted, no env', () => new Cadenya({ fetch: stubFetch }));
}

console.log('\nts config matrix: all cases passed');
