// Go SDK conformance runner: starts the mock, executes the generated Go
// conformance driver against it, records per-endpoint results.

import { execFile, execFileSync } from 'node:child_process';
import { promisify } from 'node:util';
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startMock } from './mock.mjs';

const execFileAsync = promisify(execFile);

const manifest = JSON.parse(
  readFileSync(new URL('../../gen/manifest/manifest.json', import.meta.url), 'utf8'),
);
const goDir = new URL('../../gen/go', import.meta.url).pathname;
// Resolve module deps (go-sse) and build first so compile time (slow on a
// cold cache) can't eat the run timeout.
// No `go mod tidy`: generated modules must build clean as emitted — a tidy
// here would mask an incomplete go.mod/go.sum (generation gate).
const driverBin = join(mkdtempSync(join(tmpdir(), 'redwood-go-conf-')), 'driver');
execFileSync('go', ['build', '-o', driverBin, './conformance'], {
  cwd: goDir,
  encoding: 'utf8',
  timeout: 300_000,
});

const { server, baseURL } = await startMock(manifest);

let stdout = '';
let exitCode = 0;
try {
  // Async spawn: a sync spawn would block the event loop and deadlock the
  // in-process mock server (it could never answer the driver's requests).
  ({ stdout } = await execFileAsync(driverBin, [], {
    cwd: goDir,
    env: { ...process.env, MOCK_URL: baseURL },
    encoding: 'utf8',
    timeout: 120_000,
  }));
} catch (err) {
  stdout = (err.stdout ?? '') + (err.stderr ?? '');
  exitCode = err.status ?? 1;
  console.log(`spawn error: code=${err.code} status=${err.status} signal=${err.signal} msg=${String(err.message).slice(0, 200)}`);
}
server.close();

const results = [];
for (const line of stdout.trim().split('\n')) {
  const pass = line.match(/^PASS (\S+)$/);
  const fail = line.match(/^FAIL (\S+): (.*)$/);
  if (pass) results.push({ id: pass[1], status: 'pass' });
  else if (fail) results.push({ id: fail[1], status: 'fail', reason: fail[2].slice(0, 160) });
}
if (results.length === 0) {
  console.log('--- raw driver output (no PASS/FAIL lines parsed) ---');
  console.log(stdout.slice(0, 1500));
}
const passed = results.filter((r) => r.status === 'pass').length;
console.log(`go conformance: ${passed}/${results.length} passed (driver exit ${exitCode})`);
for (const f of results.filter((r) => r.status === 'fail')) {
  console.log(`  FAIL ${f.id}: ${f.reason}`);
}
writeFileSync(
  new URL('./results-go.json', import.meta.url),
  JSON.stringify({ target: 'go', total: results.length, passed, results }, null, 2),
);
process.exit(passed === results.length && results.length > 0 ? 0 : 1);
