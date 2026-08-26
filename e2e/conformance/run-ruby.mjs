// Ruby SDK conformance runner: starts the mock, executes the generated Ruby
// conformance driver against it, records per-endpoint results.

import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { copyFileSync, readFileSync, realpathSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { startMock } from './mock.mjs';
import { provisionRuby } from './provision.mjs';

const execFileAsync = promisify(execFile);

const manifest = JSON.parse(
  readFileSync(new URL('../../gen/manifest/manifest.json', import.meta.url), 'utf8'),
);
const rubyDir = new URL('../../gen/ruby', import.meta.url).pathname;

// Isolated GEM_HOME with the generated gem installed BY the selected Ruby's
// own RubyGems, from the gemspec (this gate catches default-gem regressions
// like base64). Cache keyed by ruby version + gemspec.
const { ruby, gemHome } = provisionRuby(rubyDir);

const { server, baseURL } = await startMock(manifest);

let stdout = '';
let exitCode = 0;
let stderr = '';
try {
  // Async spawn: a sync spawn would block the event loop and deadlock the
  // in-process mock server (it could never answer the driver's requests).
  // The driver runs COPIED into the gem home with a scrubbed load path so
  // `require "cadenya"` resolves the INSTALLED gem — running beside
  // gen/ruby would let the source tree shadow packaging defects.
  const driver = join(gemHome, 'conformance.rb');
  copyFileSync(join(rubyDir, 'conformance.rb'), driver);
  ({ stdout, stderr } = await execFileAsync(ruby, [driver], {
    cwd: gemHome,
    env: {
      ...process.env,
      MOCK_URL: baseURL,
      GEM_HOME: gemHome,
      GEM_PATH: gemHome,
      RUBYLIB: '',
    },
    encoding: 'utf8',
    timeout: 120_000,
  }));
  const loaded = (stderr.match(/cadenya loaded from: (.*)/) ?? [])[1] ?? '';
  if (!realpathSync(loaded).startsWith(realpathSync(gemHome))) {
    throw new Error(`driver loaded cadenya from ${loaded || '(unknown)'}, not the installed gem`);
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
console.log(`ruby conformance: ${passed}/${results.length} passed (driver exit ${exitCode})`);
for (const f of results.filter((r) => r.status === 'fail')) {
  console.log(`  FAIL ${f.id}: ${f.reason}`);
}
writeFileSync(
  new URL('./results-ruby.json', import.meta.url),
  JSON.stringify({ target: 'ruby', total: results.length, passed, results }, null, 2),
);
process.exit(passed === results.length && results.length > 0 ? 0 : 1);
