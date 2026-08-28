import assert from 'node:assert/strict';
import { accessSync, constants, readFileSync, writeFileSync } from 'node:fs';
import { isAbsolute } from 'node:path';

export const manifest = JSON.parse(
  readFileSync(new URL('../../gen/manifest/manifest.json', import.meta.url), 'utf8'),
);

export function loadLiveEnvironment() {
  const rootEnv = readFileSync(new URL('../../.env.development', import.meta.url), 'utf8');
  for (const line of rootEnv.split('\n')) {
    const match = line.match(/^\s*(?:export\s+)?([A-Z0-9_]+)=(.*)$/);
    if (!match || process.env[match[1]] !== undefined) continue;
    process.env[match[1]] = match[2].trim().replace(/^['"]|['"]$/g, '');
  }
  if (!process.env.CADENYA_API_KEY) {
    const token = readFileSync(new URL('../../tmp/token.jwt', import.meta.url), 'utf8').trim();
    assert.ok(token && !/\s/.test(token), 'tmp/token.jwt must contain one nonblank token');
    process.env.CADENYA_API_KEY = token;
  }
  assert.ok(process.env.CADENYA_WORKSPACE_ID, 'CADENYA_WORKSPACE_ID missing from root .env.development');
}

export function configuredCliBinary() {
  const binary = process.env.CADENYA_CLI_BINARY;
  if (!binary) return undefined;
  assert.ok(isAbsolute(binary), 'CADENYA_CLI_BINARY must be an absolute path');
  accessSync(binary, constants.X_OK);
  return binary;
}

export function freshReport(sdk, wave) {
  const operations = Object.fromEntries(manifest.operations.map(({ id }) => [id, {
    status: 'blocked',
    evidence: `fresh ${sdk} ${wave}: operation not reached by an authorized wave`,
  }]));
  return { schemaVersion: 1, sdk, executedAt: new Date().toISOString(), operations };
}

export function readReport(url, sdk) {
  const report = JSON.parse(readFileSync(url, 'utf8'));
  assert.equal(report.schemaVersion, 1);
  assert.equal(report.sdk, sdk);
  return report;
}

export function writeReport(url, report) {
  const expected = new Set(manifest.operations.map(({ id }) => id));
  const actual = Object.keys(report.operations);
  assert.equal(actual.length, expected.size, `result must contain exactly ${expected.size} operations`);
  for (const id of actual) assert.ok(expected.has(id), `unknown result operation ${id}`);
  report.executedAt = new Date().toISOString();
  writeFileSync(url, `${JSON.stringify(report, null, 2)}\n`);
}

export function safeFailure(error) {
  const name = String(error?.name || error?.constructor?.name || 'Error').replace(/[^A-Za-z0-9_.-]/g, '');
  const status = Number(error?.status ?? error?.statusCode);
  const code = Number(error?.code);
  const details = [`type=${name || 'Error'}`];
  if (Number.isFinite(status)) details.push(`http=${status}`);
  if (Number.isFinite(code)) details.push(`code=${code}`);
  return details.join(', ');
}

export function counts(report) {
  return Object.values(report.operations).reduce((acc, item) => {
    acc[item.status] = (acc[item.status] ?? 0) + 1;
    return acc;
  }, {});
}

export const resourceId = (value) => value?.metadata?.id ?? value?.id;
