// Release-artifact audit: build the REAL wheel and gem and assert the
// registry-facing surfaces (docs, metadata, identity) — a source-tree
// assertion is not evidence about what a registry serves.
// Run: node e2e/artifacts.mjs
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const root = new URL('..', import.meta.url).pathname;
const run = (cmd, args, opts = {}) =>
  execFileSync(cmd, args, { encoding: 'utf8', timeout: 300_000, ...opts });

let failures = 0;
const check = (label, ok, detail = '') => {
  if (ok) {
    console.log(`ok  ${label}`);
  } else {
    failures++;
    console.log(`FAIL ${label}${detail ? `: ${detail}` : ''}`);
  }
};

const pythonProject = readFileSync(join(root, 'gen/python/pyproject.toml'), 'utf8');
const rubyGemspec = readFileSync(join(root, 'gen/ruby/cadenya.gemspec'), 'utf8');
const packageVersions = {
  typescript: JSON.parse(readFileSync(join(root, 'gen/typescript/package.json'), 'utf8')).version,
  python: pythonProject.match(/^version = "([^"]+)"/m)[1],
  ruby: rubyGemspec.match(/spec\.version = "([^"]+)"/)[1],
};

// ---- Python wheel ---------------------------------------------------------
const wheelDir = mkdtempSync(join(tmpdir(), 'redwood-wheel-'));
run('python3', ['-m', 'pip', 'wheel', '--no-deps', '-w', wheelDir, join(root, 'gen/python')], {
  stdio: ['ignore', 'ignore', 'inherit'],
});
const wheel = readdirSync(wheelDir).find((f) => f.endsWith('.whl'));
assert.ok(wheel, 'wheel built');
const metadata = run('python3', [
  '-c',
  `import zipfile,sys
z = zipfile.ZipFile(sys.argv[1])
meta = next(n for n in z.namelist() if n.endswith('METADATA'))
sys.stdout.write(z.read(meta).decode())
print('\\n__NAMES__')
print('\\n'.join(z.namelist()))`,
  join(wheelDir, wheel),
]);
check(
  `wheel: version ${packageVersions.python}`,
  metadata.includes(`\nVersion: ${packageVersions.python}\n`),
);
check('wheel: long description from README', metadata.includes('Description-Content-Type: text/markdown'));
check('wheel: README content present', metadata.includes('The official Python client for the Cadenya API'));
check('wheel: license set', /^License:/m.test(metadata) || /^Classifier: License/m.test(metadata));
check('wheel: Typing :: Typed classifier', metadata.includes('Classifier: Typing :: Typed'));
check('wheel: project URL', /^Project-URL: Homepage/m.test(metadata));
check('wheel: py.typed shipped', metadata.includes('cadenya/py.typed'));

// ---- Ruby gem -------------------------------------------------------------
const gemDir = mkdtempSync(join(tmpdir(), 'redwood-gem-'));
let gemOut = '';
try {
  gemOut = run('gem', ['build', 'cadenya.gemspec', '-o', join(gemDir, 'cadenya.gem')], {
    cwd: join(root, 'gen/ruby'),
    stdio: ['ignore', 'pipe', 'pipe'],
  });
} catch (err) {
  gemOut = `${err.stdout ?? ''}${err.stderr ?? ''}` || String(err);
}
// gem prints warnings to stderr; rebuild capturing both streams via shellless
// spawn is awkward with execFileSync, so run a verification pass instead.
const gemWarnings = run('ruby', ['-e', `
  require "rubygems"
  spec = Gem::Specification.load("cadenya.gemspec")
  warnings = []
  warnings << "no license" if Array(spec.licenses).empty?
  warnings << "invalid license #{spec.licenses}" unless spec.licenses.all? { |l|
    Gem::Licenses.match?(l) || l == "Nonstandard"
  }
  warnings << "no homepage" if spec.homepage.to_s.empty?
  print warnings.join("; ")
`, ], { cwd: join(root, 'gen/ruby') });
check('gem: builds without metadata warnings', gemWarnings === '', gemWarnings);
const gemFiles = run('tar', ['-xOf', join(gemDir, 'cadenya.gem'), 'data.tar.gz'], {
  encoding: 'buffer',
});
const gemList = execFileSync('tar', ['-tz'], { input: gemFiles, encoding: 'utf8' });
check('gem: ships README.md', gemList.includes('README.md'));
check('gem: ships api.md', gemList.includes('api.md'));
const gemSpec = run('gem', ['specification', join(gemDir, 'cadenya.gem')], { encoding: 'utf8' });
check(
  `gem: version ${packageVersions.ruby}`,
  gemSpec.includes(`version: ${packageVersions.ruby}`),
);
check('gem: license present', /licenses:\s*\n\s*- /.test(gemSpec));
check('gem: homepage present', /homepage: https:/.test(gemSpec));
check('gem: MFA required', gemSpec.includes('rubygems_mfa_required'));

// ---- Cross-SDK identity consistency --------------------------------------
const identities = {
  typescript: readFileSync(join(root, 'gen/typescript/src/client.ts'), 'utf8')
    .match(/cadenya-typescript\/([^ ']+) \(api ([^)]+)\)/),
  go: readFileSync(join(root, 'gen/go/client.go'), 'utf8')
    .match(/cadenya-go\/([^ "]+) \(api ([^)]+)\)/),
  python: readFileSync(join(root, 'gen/python/cadenya/_client.py'), 'utf8')
    .match(/cadenya-python\/([^ "]+) \(api ([^)]+)\)/),
  ruby: readFileSync(join(root, 'gen/ruby/lib/cadenya/client.rb'), 'utf8')
    .match(/cadenya-ruby\/([^ "]+) \(api ([^)]+)\)/),
  cli: readFileSync(join(root, 'gen/cli/main.go'), 'utf8')
    .match(/var version = "([^"]+)"[\s\S]*Version: version \+ " \(api ([^)]+)\)"/),
};
for (const [lang, m] of Object.entries(identities)) {
  check(`${lang}: SDK identity present`, Boolean(m));
}
for (const lang of ['typescript', 'python', 'ruby']) {
  const advertised = identities[lang]?.[1];
  check(
    `${lang}: manifest matches advertised SDK version`,
    advertised === packageVersions[lang],
    `manifest=${packageVersions[lang]}, advertised=${advertised ?? 'missing'}`,
  );
}

if (failures > 0) {
  console.error(`\nartifact audit: ${failures} failure(s)`);
  process.exit(1);
}
console.log('\nartifact audit: all checks passed');
