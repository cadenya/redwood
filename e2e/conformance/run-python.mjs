// Python SDK conformance runner: starts the mock, executes the generated
// Python conformance driver against it, records per-endpoint results.

import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { copyFileSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { startMock } from './mock.mjs';
import { provisionPython } from './provision.mjs';

const execFileAsync = promisify(execFile);

const manifest = JSON.parse(
  readFileSync(new URL('../../gen/manifest/manifest.json', import.meta.url), 'utf8'),
);
const pyDir = new URL('../../gen/python', import.meta.url).pathname;

// Isolated venv created from the PYTHON base interpreter (or python3) with
// gen/python installed from its own pyproject — cache keyed by interpreter
// version + pyproject, so dependency or runtime changes reprovision.
const python = provisionPython(pyDir);

const { server, baseURL } = await startMock(manifest);

let stdout = '';
let exitCode = 0;
try {
  // Async spawn: a sync spawn would block the event loop and deadlock the
  // in-process mock server (it could never answer the driver's requests).
  // The driver runs COPIED into the venv dir so `import cadenya` resolves
  // the INSTALLED distribution — running beside gen/python would let the
  // source tree shadow the wheel install and mask packaging defects.
  const driver = join(dirname(python), '..', 'conformance.py');
  copyFileSync(join(pyDir, 'conformance.py'), driver);
  let stderr = '';
  ({ stdout, stderr } = await execFileAsync(python, [driver], {
    env: { ...process.env, MOCK_URL: baseURL, PYTHONPATH: '' },
    encoding: 'utf8',
    timeout: 120_000,
  }));
  const loaded = (stderr.match(/cadenya loaded from: (.*)/) ?? [])[1] ?? '';
  const venvRoot = realpathSync(join(dirname(python), '..'));
  if (!realpathSync(loaded).startsWith(venvRoot)) {
    throw new Error(`driver loaded cadenya from ${loaded || '(unknown)'}, not the venv install`);
  }
} catch (err) {
  stdout = (err.stdout ?? '') + (err.stderr ?? '');
  exitCode = err.status ?? err.code ?? 1;
  if (!err.stdout) {
    console.log(`spawn error: ${String(err.message).slice(0, 300)}`);
  }
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
console.log(`python conformance: ${passed}/${results.length} passed (driver exit ${exitCode})`);
for (const f of results.filter((r) => r.status === 'fail')) {
  console.log(`  FAIL ${f.id}: ${f.reason}`);
}
writeFileSync(
  new URL('./results-python.json', import.meta.url),
  JSON.stringify({ target: 'python', total: results.length, passed, results }, null, 2),
);
process.exit(passed === results.length && results.length > 0 ? 0 : 1);
