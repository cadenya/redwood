// Live regression for the protobuf empty-union decode fix: the exact
// read-only operations the live matrix flagged (ListAIProviderKeys and
// GetModel with an embedded settings-less provider key) must decode through
// Go (via the CLI), Python, and Ruby. Read-only; no workspace mutation.
// Run: node e2e/live-decode.mjs   (loads .env.development itself)
import { execFileSync } from 'node:child_process';
import { cpSync, readFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { provisionPython } from './conformance/provision.mjs';

const root = new URL('..', import.meta.url).pathname;

try {
  const envFile = readFileSync(join(root, '.env.development'), 'utf8');
  for (const line of envFile.split('\n')) {
    const m = line.match(/^\s*(?:export\s+)?([A-Z0-9_]+)=(.*)$/);
    if (m && process.env[m[1]] === undefined) {
      process.env[m[1]] = m[2].trim().replace(/^["']|["']$/g, '');
    }
  }
} catch {
  // rely on ambient env
}
if (!process.env.CADENYA_API_KEY || !process.env.CADENYA_WORKSPACE_ID) {
  console.error('missing CADENYA_API_KEY / CADENYA_WORKSPACE_ID');
  process.exit(1);
}

let failed = false;
const step = (label, fn) => {
  try {
    fn();
    console.log(`ok  ${label}`);
  } catch (err) {
    failed = true;
    const detail = `${err.stderr ?? ''}${err.stdout ?? ''}` || String(err);
    console.log(`FAIL ${label}: ${detail.slice(0, 200)}`);
  }
};
const run = (cmd, args, opts = {}) =>
  execFileSync(cmd, args, { encoding: 'utf8', timeout: 120_000, ...opts });

// Go SDK, exercised through the CLI binary. Tidy only a temporary copy so
// this regression never mutates checked-in generated module files.
const goTmp = mkdtempSync(join(tmpdir(), 'redwood-live-decode-'));
const cliCopy = join(goTmp, 'cli');
const goCopy = join(goTmp, 'go');
cpSync(join(root, 'gen/cli'), cliCopy, { recursive: true });
cpSync(join(root, 'gen/go'), goCopy, { recursive: true });
const bin = join(goTmp, 'cadenya');
// No `go mod tidy`: the generated module must build clean as emitted.
run('go', ['build', '-o', bin, '.'], { cwd: cliCopy });

let modelId = '';
step('go/cli: ai-provider-keys list --limit 1', () => {
  run(bin, ['ai-provider-keys', 'list', '--limit', '1']);
});
step('go/cli: models retrieve <live id>', () => {
  const out = run(bin, ['models', 'list', '--limit', '1']);
  modelId = JSON.parse(out).items?.[0]?.metadata?.id ?? '';
  if (!modelId) throw new Error('no model id in response');
  run(bin, ['models', 'retrieve', modelId]);
});

const pyProbe = `
import cadenya
client = cadenya.Cadenya()
client.ai_provider_keys.list(limit=1)
model_id = client.models.list(limit=1).items[0].metadata.id
client.models.retrieve(model_id)
`;
step('python (installed wheel): list + retrieve', () => {
  run(provisionPython(join(root, 'gen/python')), ['-c', pyProbe]);
});

const rbProbe = `
require "cadenya"
client = Cadenya::Client.new
client.ai_provider_keys.list(limit: 1)
model_id = client.models.list(limit: 1).items[0].metadata.id
client.models.retrieve(model_id)
`;
step('ruby: list + retrieve', () => {
  const ruby = process.env.REDWOOD_RUBY || `${process.env.HOME}/.rbenv/shims/ruby`;
  run(ruby, [`-I${join(root, 'gen/ruby/lib')}`, '-e', rbProbe]);
});

if (failed) process.exit(1);
console.log('\nlive decode regression: passed');
