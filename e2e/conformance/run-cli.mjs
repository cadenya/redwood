// CLI conformance runner: builds the generated CLI once, then drives it
// per-operation against the mock, synthesizing argv from the manifest.
// Every body-taking operation runs twice — once with each top-level field
// as a document, once through the flattened typed flags — and both must
// reach the mock as a valid request.

import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { readFileSync, writeFileSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { startMock } from './mock.mjs';
import { documentArgv, flattenedArgv, kebab } from './cli-argv.mjs';

const execFileAsync = promisify(execFile);

const manifest = JSON.parse(
  readFileSync(new URL('../../gen/manifest/manifest.json', import.meta.url), 'utf8'),
);
const cliDir = new URL('../../gen/cli', import.meta.url).pathname;

// Build once; compile time must not eat the per-op timeout.
const binary = join(mkdtempSync(join(tmpdir(), 'redwood-cli-conf-')), 'cadenya');

async function runPass(baseURL, op, label, argv) {
  let stdout;
  try {
    ({ stdout } = await execFileAsync(binary, argv, {
      env: { ...process.env, CADENYA_API_KEY: 'conformance-key', CADENYA_BASE_URL: baseURL },
      encoding: 'utf8',
      timeout: 30_000,
    }));
  } catch (err) {
    err.message = `[${label}] ${err.message}`;
    throw err;
  }
  if (op.pagination) {
    const parsed = JSON.parse(stdout);
    if (!Array.isArray(parsed.items) || parsed.items.length !== 1) {
      throw new Error(`[${label}] expected 1 item, got ${parsed.items?.length}`);
    }
  } else if (op.response.kind === 'sse') {
    // Streams are NDJSON: every non-empty stdout line must parse as an
    // independent JSON document (no indented multi-line values, no
    // status text), one line per event served by the mock.
    const lines = stdout.split('\n').filter((l) => l.trim() !== '');
    if (lines.length !== 2) {
      throw new Error(`[${label}] expected 2 NDJSON lines, got ${lines.length}`);
    }
    for (const line of lines) JSON.parse(line);
  } else if (op.response.kind === 'json') {
    JSON.parse(stdout);
  }
}

async function main() {
  // No `go mod tidy`: generated modules must build clean as emitted — a
  // tidy here would mask an incomplete go.mod/go.sum (generation gate).
  await execFileAsync('go', ['build', '-o', binary, '.'], {
    cwd: cliDir,
    timeout: 300_000,
  });

  const { server, baseURL } = await startMock(manifest);
  const results = [];

  for (const op of manifest.operations) {
    // op.resource is a dotted accessor path; each segment is a subcommand.
    // Conformance asserts wire behavior, not rendering: force JSON output
    // regardless of the config's display default (streams accept it too).
    const base = ['--display', 'json', ...op.resource.split('.').map(kebab), kebab(op.method)];
    for (const pos of op.positionals ?? []) base.push(String(pos.sample));
    const paramArgv = [];
    const addParam = (wireName, sample) => {
      const flag = kebab(wireName);
      if (Array.isArray(sample)) {
        for (const item of sample) {
          paramArgv.push(`--${flag}=${typeof item === 'string' ? item : JSON.stringify(item)}`);
        }
      } else if (typeof sample === 'object' && sample !== null) {
        paramArgv.push(`--${flag}=${JSON.stringify(sample)}`);
      } else {
        paramArgv.push(`--${flag}=${sample}`);
      }
    };
    for (const p of op.pathParams ?? []) addParam(p.name, p.sample);
    for (const p of op.queryParams ?? []) addParam(p.name, p.sample);

    const passes = [];
    if (op.cli) {
      // Flattened surface: typed flags for the first arm of every union.
      passes.push({ label: 'flags', argv: [...base, ...paramArgv, ...flattenedArgv(op.cli)] });
      if (!op.wholeBody) {
        // Documents: each top-level field as one value on its flag.
        passes.push({ label: 'documents', argv: [...base, ...paramArgv, ...documentArgv(op.bodyFields)] });
      }
    } else if (op.wholeBody) {
      passes.push({ label: 'body', argv: [...base, ...paramArgv, `--body=${JSON.stringify(op.wholeBody.sample)}`] });
    } else {
      passes.push({ label: 'params', argv: [...base, ...paramArgv] });
    }

    try {
      for (const pass of passes) await runPass(baseURL, op, pass.label, pass.argv);
      results.push({ id: op.id, status: 'pass' });
    } catch (err) {
      const reason = `${err.message} ${err.stderr ?? ''}`.replace(/\s+/g, ' ').slice(0, 160);
      results.push({ id: op.id, status: 'fail', reason });
    }
  }
  server.close();

  const passed = results.filter((r) => r.status === 'pass').length;
  console.log(`cli conformance: ${passed}/${results.length} passed`);
  for (const f of results.filter((r) => r.status === 'fail')) {
    console.log(`  FAIL ${f.id}: ${f.reason}`);
  }
  writeFileSync(
    new URL('./results-cli.json', import.meta.url),
    JSON.stringify({ target: 'cli', total: results.length, passed, results }, null, 2),
  );
  process.exit(passed === results.length && results.length > 0 ? 0 : 1);
}

await main();
