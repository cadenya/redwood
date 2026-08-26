// Acceptance gate for the openapi exporter: every emitted sample must be
// SEMANTICALLY valid against the built/installed artifact for its language,
// not merely parseable. Go samples compile against gen/go; TS samples
// typecheck against gen/typescript; Python and Ruby samples execute against
// the installed wheel/gem with the transport stubbed to capture the attempted
// method+path; CLI samples run through the built binary against a loopback
// capture server. Negative controls prove each lane can actually fail.
// Run: node e2e/openapi-samples.mjs
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { provisionPython, provisionRuby } from './conformance/provision.mjs';

const root = new URL('..', import.meta.url).pathname;
const goCache = process.env.GOCACHE ?? join(tmpdir(), 'redwood-openapi-samples-go-cache');
mkdirSync(goCache, { recursive: true });
const goEnv = { ...process.env, GOCACHE: goCache };
// Ruby's stdlib YAML converts the doc to JSON (no npm dependency needed).
const docJson = execFileSync('ruby', ['-ryaml', '-rjson', '-e',
  'puts JSON.generate(YAML.safe_load(File.read(ARGV[0]), aliases: true))',
  join(root, 'gen/openapi/openapi.yml')], { encoding: 'utf8', timeout: 120_000, maxBuffer: 256 * 1024 * 1024 });
const doc = JSON.parse(docJson);

const samples = { typescript: [], go: [], python: [], ruby: [], shell: [] };
for (const [path, item] of Object.entries(doc.paths ?? {})) {
  for (const [method, op] of Object.entries(item)) {
    if (!op || typeof op !== 'object' || !op['x-codeSamples']) continue;
    for (const s of op['x-codeSamples']) {
      samples[s.lang]?.push({
        id: op.operationId,
        source: s.source,
        method: method.toUpperCase(),
        // Path params become a wildcard: values in samples are sampler-chosen.
        pathRegex: '^' + path.replace(/[.*+?^${}()|[\]\\]/g, '\\$&').replace(/\\\{[^}]+\\\}/g, '[^/]+') + '$',
      });
    }
  }
}
const counts = Object.fromEntries(Object.entries(samples).map(([k, v]) => [k, v.length]));
console.log('sample counts:', JSON.stringify(counts));
if (Object.values(counts).some((c) => c !== counts.typescript)) {
  console.error('uneven sample coverage');
  process.exit(1);
}

let failures = 0;

// ---- Go: compile every sample against the generated SDK -------------------
{
  const dir = mkdtempSync(join(tmpdir(), 'redwood-samples-go-'));
  writeFileSync(
    join(dir, 'go.mod'),
    `module sample-probe\n\ngo 1.22\n\nrequire go.cadenya.com/cadenya-go v0.0.0\n\nrequire github.com/tmaxmax/go-sse v0.11.0 // indirect\n\nreplace go.cadenya.com/cadenya-go => ${join(root, 'gen/go')}\n`,
  );
  for (const [i, s] of samples.go.entries()) {
    const pkgDir = join(dir, `s${i}`);
    mkdirSync(pkgDir);
    writeFileSync(join(pkgDir, 'main.go'), s.source);
  }
  execFileSync('go', ['mod', 'tidy'], { cwd: dir, timeout: 300_000, env: goEnv });
  try {
    execFileSync('go', ['build', './...'], { cwd: dir, timeout: 600_000, encoding: 'utf8', env: goEnv });
    console.log(`ok  go: ${samples.go.length}/${samples.go.length} samples compile against gen/go`);
  } catch (err) {
    failures++;
    console.log(`FAIL go samples:\n${(err.stderr ?? '').split('\n').slice(0, 20).join('\n')}`);
  }
}

// ---- Python: execute against the installed wheel, transport stubbed --------
// The harness patches Core.request/raw to raise a probe carrying the
// attempted (method, path) BEFORE any network I/O, then runs each sample.
// Nonexistent imports/accessors/methods and wrong argument binding all fail;
// a captured probe must match the operation's method and path template.
const PY_HARNESS = `
import json, re, sys, traceback

import cadenya._core as core

class _Probe(Exception):
    def __init__(self, method, path):
        self.method, self.path = method, path

def _request(self, method, path, **kwargs):
    raise _Probe(method, path)

core.Core.request = _request
core.Core.raw = _request

manifest = json.load(open(sys.argv[1]))
bad = []
for entry in manifest:
    source = open(entry["file"]).read()
    try:
        exec(compile(source, entry["file"], "exec"), {"__name__": "__main__"})
        bad.append(f"{entry['id']}: sample completed without reaching the transport")
    except _Probe as probe:
        if probe.method.upper() != entry["method"]:
            bad.append(f"{entry['id']}: attempted {probe.method} != {entry['method']}")
        elif not re.match(entry["pathRegex"], probe.path):
            bad.append(f"{entry['id']}: attempted path {probe.path} !~ {entry['pathRegex']}")
    except Exception:
        bad.append(f"{entry['id']}: {traceback.format_exc(limit=1).splitlines()[-1]}")
print("\\n".join(bad))
sys.exit(1 if bad else 0)
`;
function runPythonLane(entries, label) {
  const dir = mkdtempSync(join(tmpdir(), 'redwood-samples-py-'));
  const manifest = entries.map((s, i) => {
    const file = join(dir, `s${i}.py`);
    writeFileSync(file, s.source);
    return { id: s.id, file, method: s.method, pathRegex: s.pathRegex };
  });
  writeFileSync(join(dir, 'manifest.json'), JSON.stringify(manifest));
  writeFileSync(join(dir, 'harness.py'), PY_HARNESS);
  const python = provisionPython(join(root, 'gen/python'));
  const r = spawnSync(python, [join(dir, 'harness.py'), join(dir, 'manifest.json')], {
    encoding: 'utf8',
    timeout: 300_000,
    env: { ...process.env, CADENYA_API_KEY: 'probe', CADENYA_WORKSPACE_ID: 'sample', PYTHONPATH: '' },
  });
  return { ok: r.status === 0, detail: (r.stdout || '') + (r.stderr || ''), label };
}
{
  const r = runPythonLane(samples.python, 'python');
  if (r.ok) console.log(`ok  python: ${samples.python.length}/${samples.python.length} samples bind against the installed wheel`);
  else { failures++; console.log(`FAIL python samples:\n${r.detail.split('\n').slice(0, 12).join('\n')}`); }
}

// ---- Ruby: execute against the installed gem, transport stubbed ------------
const RB_HARNESS = `
require "json"
require "cadenya"

class Probe < StandardError
  attr_reader :verb, :path
  def initialize(verb, path)
    @verb = verb
    @path = path
    super("probe")
  end
end

Cadenya::Core.class_eval do
  def request(method, path, **_kw)
    raise Probe.new(method.to_s, path)
  end

  def stream_request(method, path, **_kw, &_blk)
    raise Probe.new(method.to_s, path)
  end
end

bad = []
JSON.parse(File.read(ARGV[0])).each do |entry|
  source = File.read(entry["file"])
  begin
    eval(source, TOPLEVEL_BINDING.dup, entry["file"]) # rubocop:disable Security/Eval
    bad << "#{entry['id']}: sample completed without reaching the transport"
  rescue Probe => probe
    if probe.verb.upcase != entry["method"]
      bad << "#{entry['id']}: attempted #{probe.verb} != #{entry['method']}"
    elsif !Regexp.new(entry["pathRegex"]).match?(probe.path)
      bad << "#{entry['id']}: attempted path #{probe.path} !~ #{entry['pathRegex']}"
    end
  rescue StandardError, NameError => e
    bad << "#{entry['id']}: #{e.class}: #{e.message.lines.first&.strip}"
  end
end
puts bad.join("\n")
exit(bad.empty? ? 0 : 1)
`;
function runRubyLane(entries, label) {
  const dir = mkdtempSync(join(tmpdir(), 'redwood-samples-rb-'));
  const manifest = entries.map((s, i) => {
    const file = join(dir, `s${i}.rb`);
    writeFileSync(file, s.source);
    return { id: s.id, file, method: s.method, pathRegex: s.pathRegex };
  });
  writeFileSync(join(dir, 'manifest.json'), JSON.stringify(manifest));
  writeFileSync(join(dir, 'harness.rb'), RB_HARNESS);
  const { ruby, gemHome } = provisionRuby(join(root, 'gen/ruby'));
  const r = spawnSync(ruby, [join(dir, 'harness.rb'), join(dir, 'manifest.json')], {
    encoding: 'utf8',
    timeout: 300_000,
    cwd: dir,
    env: {
      ...process.env, GEM_HOME: gemHome, GEM_PATH: gemHome, RUBYLIB: '',
      CADENYA_API_KEY: 'probe', CADENYA_WORKSPACE_ID: 'sample',
    },
  });
  return { ok: r.status === 0, detail: (r.stdout || '') + (r.stderr || ''), label };
}
{
  const r = runRubyLane(samples.ruby, 'ruby');
  if (r.ok) console.log(`ok  ruby: ${samples.ruby.length}/${samples.ruby.length} samples bind against the installed gem`);
  else { failures++; console.log(`FAIL ruby samples:\n${r.detail.split('\n').slice(0, 12).join('\n')}`); }
}

// ---- TypeScript: every sample must typecheck against gen/typescript --------
{
  const dir = mkdtempSync(join(tmpdir(), 'redwood-samples-ts-'));
  for (const [i, s] of samples.typescript.entries()) {
    // Redirect the package import to the generated SDK for a REAL typecheck.
    const source = s.source.replace(
      /from '[^']+';/,
      `from '${join(root, 'gen/typescript/src/index.js')}';`,
    );
    writeFileSync(join(dir, `s${i}.mts`), source);
  }
  writeFileSync(
    join(dir, 'tsconfig.json'),
    JSON.stringify({
      compilerOptions: {
        target: 'ES2022', lib: ['ES2022', 'DOM', 'DOM.AsyncIterable'],
        module: 'NodeNext', moduleResolution: 'NodeNext', strict: true,
        noEmit: true, skipLibCheck: true, allowImportingTsExtensions: false,
      },
    }),
  );
  try {
    execFileSync('npx', ['--prefix', join(root, 'gen/typescript'), 'tsc', '-p', dir], {
      cwd: join(root, 'gen/typescript'),
      timeout: 600_000,
      encoding: 'utf8',
    });
    console.log(`ok  typescript: ${samples.typescript.length}/${samples.typescript.length} samples typecheck against gen/typescript`);
  } catch (err) {
    failures++;
    console.log(`FAIL typescript samples:\n${(err.stdout ?? '').split('\n').slice(0, 20).join('\n')}`);
  }
}

// ---- CLI: run every sample through the built binary ------------------------
// A loopback server captures the attempted request; any exit-2 usage error is
// a sample failure, and the captured method+path must match the operation.
// Sample text is tokenized as argv (quotes + line continuations), never run
// through a shell.
function shellSplit(source) {
  const text = source.replace(/\\\n/g, ' ');
  const argv = [];
  let current = '';
  let quote = null;
  let has = false;
  for (const ch of text) {
    if (quote) {
      if (ch === quote) quote = null;
      else current += ch;
      continue;
    }
    if (ch === "'" || ch === '"') { quote = ch; has = true; continue; }
    if (/\s/.test(ch)) {
      if (has || current) argv.push(current);
      current = ''; has = false;
      continue;
    }
    current += ch;
  }
  if (has || current) argv.push(current);
  return argv;
}
// Async spawn: a sync spawn would block the event loop and deadlock the
// in-process capture server (it could never answer the CLI's request).
function runArgv(bin, argv, env) {
  return new Promise((resolve) => {
    const child = spawn(bin, argv, { env, stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = '';
    let stderr = '';
    const timer = setTimeout(() => child.kill('SIGKILL'), 30_000);
    child.stdout.on('data', (d) => { stdout += d; });
    child.stderr.on('data', (d) => { stderr += d; });
    child.on('close', (status) => { clearTimeout(timer); resolve({ status, stdout, stderr }); });
  });
}
async function runCliLane(entries) {
  const bin = join(mkdtempSync(join(tmpdir(), 'redwood-samples-cli-')), 'cadenya');
  execFileSync('go', ['build', '-o', bin, '.'], {
    cwd: join(root, 'gen/cli'), timeout: 600_000, env: goEnv,
  });
  let captured = null;
  const server = createServer((req, res) => {
    captured = { method: req.method, path: req.url.split('?')[0] };
    req.resume();
    // 400 is NOT in the SDK's retryable set — a retryable status here
    // would make every sample pay full retry backoff.
    res.statusCode = 400;
    res.setHeader('content-type', 'application/json');
    res.end('{"code":3,"message":"probe"}');
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const base = `http://127.0.0.1:${server.address().port}`;
  const bad = [];
  try {
    for (const s of entries) {
      const argv = shellSplit(s.source);
      if (argv[0] !== 'cadenya') { bad.push(`${s.id}: missing binary prefix`); continue; }
      captured = null;
      const r = await runArgv(bin, ['--display', 'json', ...argv.slice(1)], {
        ...process.env, CADENYA_API_KEY: 'probe', CADENYA_WORKSPACE_ID: 'sample',
        CADENYA_BASE_URL: base,
      });
      if (r.status === 2) { bad.push(`${s.id}: usage error (exit 2): ${(r.stderr || r.stdout).split('\n')[0]}`); continue; }
      if (!captured) { bad.push(`${s.id}: no request reached the capture server (exit ${r.status})`); continue; }
      if (captured.method !== s.method) { bad.push(`${s.id}: attempted ${captured.method} != ${s.method}`); continue; }
      if (!new RegExp(s.pathRegex).test(captured.path)) bad.push(`${s.id}: attempted path ${captured.path} !~ ${s.pathRegex}`);
    }
  } finally {
    server.close();
  }
  return bad;
}
{
  const bad = await runCliLane(samples.shell);
  if (bad.length === 0) console.log(`ok  cli: ${samples.shell.length}/${samples.shell.length} samples execute through the built binary`);
  else { failures++; console.log(`FAIL cli samples:\n${bad.slice(0, 12).join('\n')}`); }
}

// ---- negative controls: the gate itself must be able to fail ---------------
// Each control corrupts a real sample; a lane that accepts it is broken.
{
  const controls = [];
  const py = samples.python[0];
  controls.push(['python nonexistent method', () =>
    runPythonLane([{ ...py, source: py.source.replace(/client\.(\w+)\.(\w+)\(/, 'client.$1.no_such_method(') }], 'nc').ok]);
  controls.push(['python unknown kwarg', () =>
    runPythonLane([{ ...py, source: py.source.replace(/\(([^)]*)\)\n    print/, '(bogus_kwarg=1, $1)\n    print') }], 'nc').ok]);
  const rb = samples.ruby[0];
  controls.push(['ruby nonexistent method', () =>
    runRubyLane([{ ...rb, source: rb.source.replace(/client\.(\w+)\.(\w+)\(/, 'client.$1.no_such_method(') }], 'nc').ok]);
  const sh = samples.shell.find((s) => s.source.includes('--'));
  controls.push(['cli unknown flag', async () =>
    (await runCliLane([{ ...sh, source: sh.source.replace('cadenya ', 'cadenya ').trimEnd() + " --no-such-flag 'x'" }])).length === 0]);
  controls.push(['cli nonexistent command', async () =>
    (await runCliLane([{ ...sh, source: sh.source.replace(/^cadenya (\S+)/, 'cadenya no-such-resource') }])).length === 0]);
  let brokenControls = 0;
  for (const [name, fn] of controls) {
    const passed = await fn();
    if (passed) { brokenControls++; console.log(`FAIL negative control not caught: ${name}`); }
  }
  if (brokenControls) failures++;
  else console.log(`ok  negative controls: ${controls.length}/${controls.length} corrupted samples rejected`);
}

if (failures) {
  console.error(`\nopenapi sample gate: ${failures} language(s) failed`);
  process.exit(1);
}
console.log('\nopenapi sample gate: all languages passed');
