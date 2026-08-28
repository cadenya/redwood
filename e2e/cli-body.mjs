// Body-assembly round trip for the generated CLI, offline: for every
// operation with a body, the flattened flags must assemble a body (printed
// by --dry-run) that survives a full round trip through -f — the same
// document fed back as a file must assemble byte-identically — and the
// documents pass must agree with the flags pass on every path both set.
//
// Exercises: flag → path mapping, union stamping, enum short forms inside
// documents, unknown-field stripping, and the update-mask derivation,
// without a network.
//
// Usage: node e2e/cli-body.mjs   (builds gen/cli)

import { execFile } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';

import { documentArgv, flattenedArgv, kebab } from './conformance/cli-argv.mjs';

const execFileAsync = promisify(execFile);
const root = new URL('..', import.meta.url).pathname;
const manifest = JSON.parse(readFileSync(join(root, 'gen/manifest/manifest.json'), 'utf8'));
const cliDir = join(root, 'gen/cli');
const work = mkdtempSync(join(tmpdir(), 'redwood-cli-body-'));
const binary = join(work, 'cadenya');
await execFileAsync('go', ['build', '-o', binary, '.'], { cwd: cliDir });

const env = { ...process.env, CADENYA_API_KEY: 'offline', CADENYA_WORKSPACE_ID: 'ws_offline' };

async function dryRun(argv) {
  const { stdout } = await execFileAsync(binary, [...argv, '--dry-run', '--display', 'json'], {
    env,
    encoding: 'utf8',
    timeout: 30_000,
  });
  return JSON.parse(stdout);
}

// Every leaf of `subset` must appear with the same value in `whole`.
function contains(whole, subset, path = '') {
  if (subset && typeof subset === 'object' && !Array.isArray(subset)) {
    if (!whole || typeof whole !== 'object') return `${path || '.'}: missing object`;
    for (const [k, v] of Object.entries(subset)) {
      const reason = contains(whole[k], v, path ? `${path}.${k}` : k);
      if (reason) return reason;
    }
    return null;
  }
  return JSON.stringify(whole) === JSON.stringify(subset) ? null : `${path}: ${JSON.stringify(whole)} != ${JSON.stringify(subset)}`;
}

let checked = 0;
const failures = [];
for (const op of manifest.operations) {
  if (!op.cli) continue;
  const base = [...op.resource.split('.').map(kebab), kebab(op.method)];
  for (const pos of op.positionals ?? []) base.push(String(pos.sample));
  for (const p of op.pathParams ?? []) base.push(`--${kebab(p.name)}=${p.sample}`);
  for (const p of op.queryParams ?? []) {
    if (p.required) base.push(`--${kebab(p.name)}=${p.sample}`);
  }
  try {
    const flags = await dryRun([...base, ...flattenedArgv(op.cli)]);
    const file = join(work, `${op.id}.json`);
    writeFileSync(file, JSON.stringify(flags));
    const roundTrip = await dryRun([...base, '-f', file]);
    if (JSON.stringify(roundTrip) !== JSON.stringify(flags)) {
      throw new Error(`-f round trip differs:\n  flags: ${JSON.stringify(flags)}\n  file:  ${JSON.stringify(roundTrip)}`);
    }
    if (!op.wholeBody) {
      const docs = await dryRun([...base, ...documentArgv(op.bodyFields)]);
      const drift = contains(flags, docs);
      if (drift) throw new Error(`documents pass disagrees with flags pass at ${drift}`);
    }
    checked += 1;
  } catch (err) {
    failures.push(`${op.id}: ${err.message} ${err.stderr ?? ''}`.replace(/\s+/g, ' ').slice(0, 400));
  }
}
console.log(`cli body round trip: ${checked}/${checked + failures.length} operations ok`);
for (const f of failures) console.log(`  FAIL ${f}`);
process.exit(failures.length === 0 ? 0 : 1);
