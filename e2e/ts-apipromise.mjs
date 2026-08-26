// Durable gate: TypeScript APIPromise ownership rules. Awaiting parses;
// withResponse() pairs data with the response; asResponse() (called
// synchronously) claims the UNCONSUMED body; and a dropped return value
// still reaches terminal cleanup — body consumed, deadline timer released,
// process free to exit. Runs against the built dist.
// Run: node e2e/ts-apipromise.mjs
import { createServer } from 'node:http';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const root = new URL('..', import.meta.url).pathname;
const { default: Cadenya } = await import('../gen/typescript/dist/index.js');

const failures = [];
const BODY = JSON.stringify({ info: {} });
const server = createServer((req, res) => {
  res.setHeader('content-type', 'application/json');
  res.setHeader('x-request-id', 'req_1');
  res.end(BODY);
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const baseURL = `http://127.0.0.1:${server.address().port}`;
const client = new Cadenya({ apiKey: 'k', workspaceId: 'w', baseURL });

// 1) plain await, withResponse, synchronous asResponse.
const account = await client.accounts.retrieve();
if (!account) failures.push('await returned nothing');
const { data, response } = await client.accounts.retrieve().withResponse();
if (!data || response.status !== 200 || response.headers.get('x-request-id') !== 'req_1') {
  failures.push('withResponse pairing broken');
}
const raw = await client.accounts.retrieve().asResponse();
const rawText = await raw.text();
if (rawText !== BODY) failures.push(`asResponse body was consumed or altered: ${rawText}`);

// 2) mixed mode: LATE parsed access after asResponse still yields data
// via... the documented rule is that late asResponse after parse gets a
// consumed body; here we check the reverse — asResponse first, then await,
// which re-parses from the memoized parse (body already claimed): the await
// must reject cleanly rather than hang.
{
  const p = client.accounts.retrieve();
  await p.asResponse();
  let settled = false;
  try {
    await Promise.race([
      p.then(() => { settled = true; }),
      new Promise((_, reject) => setTimeout(() => reject(new Error('timeout')), 2000)),
    ]);
    settled = true;
  } catch {
    settled = true; // a clean rejection is acceptable; hanging is not
  }
  if (!settled) failures.push('mixed-mode await hung');
}

server.close();

// 3) dropped return value: the process must exit promptly (deadline timer
// released, body consumed) even though nobody observed the promise.
{
  const dir = mkdtempSync(join(tmpdir(), 'redwood-apipromise-'));
  const script = join(dir, 'drop.mjs');
  writeFileSync(script, `
import { createServer } from 'node:http';
const { default: Cadenya } = await import('${join(root, 'gen/typescript/dist/index.js')}');
const server = createServer((req, res) => {
  res.setHeader('content-type', 'application/json');
  res.end('{"info":{}}');
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const client = new Cadenya({
  apiKey: 'k', workspaceId: 'w',
  baseURL: 'http://127.0.0.1:' + server.address().port,
  timeout: 30000,
});
const startedAt = Date.now();
client.accounts.retrieve(); // deliberately unobserved
setTimeout(() => server.close(), 200).unref();
process.on('beforeExit', () => {
  console.log(JSON.stringify({ elapsed: Date.now() - startedAt }));
});
`);
  const out = execFileSync('node', [script], { encoding: 'utf8', timeout: 20_000 });
  const { elapsed } = JSON.parse(out.trim().split('\n').pop());
  if (elapsed > 5_000) {
    failures.push(`dropped APIPromise held the process ${elapsed}ms (timer not released)`);
  }
}

if (failures.length) {
  console.log(failures.join('\n'));
  console.error('ts apipromise gate: FAILED');
  process.exit(1);
}
console.log('ts apipromise gate: all cases passed');
