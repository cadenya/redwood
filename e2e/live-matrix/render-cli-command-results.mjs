import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';

import { manifest } from './common-node.mjs';

const binary = process.env.CADENYA_CLI_BINARY;
assert.ok(binary?.startsWith('/'), 'CADENYA_CLI_BINARY must be an absolute path');

const results = JSON.parse(
  readFileSync(new URL('./results-cli.json', import.meta.url), 'utf8'),
);
const usageLines = readFileSync(new URL('../../gen/cli/api.md', import.meta.url), 'utf8')
  .split('\n')
  .filter((line) => line.startsWith('cadenya '));
assert.equal(usageLines.length, manifest.operations.length);

const kebab = (value) => value
  .replace(/_/g, '-')
  .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
  .toLowerCase();
const csv = (value) => `"${String(value ?? '').replaceAll('"', '""')}"`;

const rows = manifest.operations.map((operation, index) => {
  const path = [...operation.resource.split('.').map(kebab), kebab(operation.method)].join(' ');
  const expected = `cadenya ${path}`;
  const grammar = usageLines[index];
  assert.ok(
    grammar.startsWith(expected),
    `CLI grammar/manifest order mismatch for ${operation.id}: ${grammar}`,
  );
  const result = results.operations[operation.id];
  assert.ok(result, `missing CLI result for ${operation.id}`);
  return {
    scope: 'api',
    operationId: operation.id,
    command: `${binary} ${path}`,
    argumentForm: `${binary}${grammar.slice('cadenya'.length)}`,
    status: result.status,
    evidence: result.evidence,
    record: operation.id === 'UploadService_CreateUpload'
      ? 'upload_01M12ZKWFCHSZTXZ54P867BNWN (live-cli-evidence-20260828)'
      : '',
  };
});

const local = (command, argumentForm, status, evidence) => rows.push({
  scope: 'local', operationId: '', command: `${binary} ${command}`,
  argumentForm: `${binary} ${argumentForm}`, status, evidence, record: '',
});
local('--help', '--help', 'completed', 'absolute binary rendered root command help');
local('--version', '--version', 'completed', 'absolute binary reported CLI 1.0.0 / API 1.0');
local('schema', 'schema [command path]', 'completed', 'listed all schemas and rendered agents create schema');
local('auth login', 'auth login --no-browser', 'blocked', 'real device flow initialized; interactive browser completion intentionally not available');
local('auth status', '--profile live-test auth status', 'completed', 'isolated temporary-home status succeeded');
local('auth logout', '--profile live-test auth logout', 'completed', 'isolated temporary-home logout succeeded');
local('config set', '--profile live-test config set workspace-id <workspace-id>', 'completed', 'isolated temporary-home value stored');
local('config get', '--profile live-test config get workspace-id', 'completed', 'isolated temporary-home value read and matched');
local('config list', '--profile live-test config list', 'completed', 'isolated temporary-home effective configuration listed');
local('config unset', '--profile live-test config unset workspace-id', 'completed', 'isolated temporary-home value removed');
local('whoami', '--display json whoami', 'completed', 'root alias reached api.cadenya.com and decoded profile JSON');

const headers = ['scope', 'operation_id', 'command', 'argument_form', 'status', 'evidence', 'live_record'];
const body = rows.map((row) => [
  row.scope,
  row.operationId,
  row.command,
  row.argumentForm,
  row.status,
  row.evidence,
  row.record,
].map(csv).join(','));
const output = new URL('./cli-command-live-results.csv', import.meta.url);
writeFileSync(output, `${headers.map(csv).join(',')}\n${body.join('\n')}\n`);

const counts = Object.groupBy(rows, (row) => row.status);
console.log(JSON.stringify(Object.fromEntries(
  Object.entries(counts).map(([status, items]) => [status, items.length]),
)));
