// TypeScript SDK conformance driver: calls every operation in the manifest
// against the conformance mock and reports per-endpoint pass/fail.
//
// Usage: node e2e/conformance/ts-driver.mjs

import { readFileSync, writeFileSync } from 'node:fs';
import { startMock } from './mock.mjs';

delete process.env.CADENYA_API_KEY;
delete process.env.CADENYA_WORKSPACE_ID;

const manifest = JSON.parse(
  readFileSync(new URL('../../gen/manifest/manifest.json', import.meta.url), 'utf8'),
);
const { default: Cadenya } = await import('../../gen/typescript/dist/index.js');

const { server, baseURL } = await startMock(manifest);
const client = new Cadenya({ apiKey: 'conformance-key', baseURL });

const toCamel = (s) => s.replace(/_([a-z0-9])/g, (_, c) => c.toUpperCase());

const setNested = (target, path, value) => {
  const segments = path.split('.');
  let owner = target;
  for (const segment of segments.slice(0, -1)) {
    const current = owner[segment];
    if (current !== undefined && (current === null || typeof current !== 'object')) {
      throw new Error(`query parameter ${path} collides at ${segment}`);
    }
    owner = owner[segment] ??= {};
  }
  owner[segments.at(-1)] = value;
};

const results = [];
for (const op of manifest.operations) {
  const label = `${op.resource}.${op.method}`;
  try {
    // op.resource is a dotted accessor path for nested resources.
    const resource = op.resource.split('.').reduce((owner, key) => owner?.[toCamel(key)], client);
    if (!resource) throw new Error(`missing resource ${op.resource} on client`);
    const fn = resource[toCamel(op.method)];
    if (typeof fn !== 'function') throw new Error(`missing method ${toCamel(op.method)}`);

    const params = {};
    for (const p of [...op.pathParams, ...op.bodyFields]) {
      params[p.name] = p.sample;
    }
    for (const p of op.queryParams) setNested(params, p.name, p.sample);
    if (op.wholeBody) params.body = op.wholeBody.sample;
    const args = [];
    for (const pos of op.positionals ?? []) args.push(pos.sample);
    if (Object.keys(params).length > 0) args.push(params);

    const value = await fn.call(resource, ...args);

    if (op.response.kind === 'sse') {
      let count = 0;
      for await (const _event of value) count++;
      if (count !== 2) throw new Error(`expected 2 SSE events, got ${count}`);
    } else if (op.response.kind === 'paginated') {
      if (!Array.isArray(value.items)) throw new Error('page has no items array');
      // Auto-iterate: the mock serves two pages and rejects the second when
      // any non-cursor query param drifts from the first request.
      let total = 0;
      for await (const _item of value) total++;
      if (total !== 2) throw new Error(`expected 2 items across pages, got ${total}`);
    }
    results.push({ id: op.id, label, status: 'pass' });
  } catch (err) {
    results.push({ id: op.id, label, status: 'fail', reason: String(err.message ?? err).slice(0, 160) });
  }
}
server.close();

const passed = results.filter((r) => r.status === 'pass');
const failed = results.filter((r) => r.status === 'fail');
console.log(`typescript conformance: ${passed.length}/${results.length} passed`);
for (const f of failed) console.log(`  FAIL ${f.label} (${f.id}): ${f.reason}`);

writeFileSync(
  new URL('./results-typescript.json', import.meta.url),
  JSON.stringify({ target: 'typescript', total: results.length, passed: passed.length, results }, null, 2),
);
process.exit(failed.length > 0 ? 1 : 0);
