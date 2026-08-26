#!/usr/bin/env node

// Build the deterministic Go live-test template catalog from the same
// manifest and generated conformance driver used by the SDK test suite.
//
// This intentionally does not execute the templates. The conformance samples
// contain placeholder IDs and minimal mock-only bodies; the catalog records
// the fixture and safety requirements that must be satisfied before a call is
// promoted to live evidence.

import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';

const here = new URL('./', import.meta.url);
const manifestURL = new URL('../../../gen/manifest/manifest.json', here);
const specURL = new URL('../../../api-spec.yml', here);
const driverURL = new URL('../../../gen/go/conformance/main.go', here);
const outputURL = new URL('./snippets.json', here);

const manifest = JSON.parse(readFileSync(manifestURL, 'utf8'));
const spec = readFileSync(specURL, 'utf8');
const driver = readFileSync(driverURL, 'utf8');

const specOperationIDs = [...spec.matchAll(/^\s*operationId:\s*["']?([^\s"']+)["']?\s*$/gm)]
  .map((match) => match[1]);
const manifestOperationIDs = manifest.operations.map((operation) => operation.id);

assert.equal(new Set(specOperationIDs).size, specOperationIDs.length, 'duplicate operationId in api-spec.yml');
assert.equal(
  new Set(manifestOperationIDs).size,
  manifestOperationIDs.length,
  'duplicate operation id in manifest',
);
assert.deepEqual(
  [...specOperationIDs].sort(),
  [...manifestOperationIDs].sort(),
  'api-spec.yml and generated manifest operation sets differ',
);

// These are the only Go operations for which a checked-in real-API program
// currently provides positive success-path evidence. A 404 probe is useful
// error-mapping evidence, but does not prove the operation's success decode.
const existingLiveEvidence = new Map([
  ['AccountService_GetAccount', 'e2e/live-go/main.go: accounts.retrieve success'],
  ['WorkspaceService_ListWorkspaces', 'e2e/live-go/main.go: workspaces.list success'],
  ['AgentService_ListAgents', 'e2e/live-go/main.go: agents.list success'],
  ['ObjectiveService_ListObjectives', 'e2e/live-go/main.go: objectives.list + pagination success'],
]);

const attemptedEvidence = new Map([
  [
    'ModelService_ListModels',
    'e2e/live-go/main.go calls the endpoint, but treats APIError as a skip; no retained output proves its success branch ran',
  ],
  [
    'ObjectiveService_GetObjective',
    'e2e/live-go/main.go asserted a typed APIError for a deliberately missing objective; the success path was not exercised',
  ],
]);

const nonQualifyingEvidence = new Map([
  [
    'ObjectiveEventStreamsService_StreamObjectiveEvents',
    'an ad-hoc real-API transport probe passed during review, but no checked-in reproducible result identifies its fixture and assertions',
  ],
]);

// JavaScript has no native free-spacing regex flag. Keep these as readable
// strings, then discard whitespace before compiling them.
const privilegedMutationPattern = new RegExp(String.raw`(?:
  RotateChallengeToken|RotateWebhookSigningKey|
  GlobalAPIKeyService_(?:Disable|Enable|Rotate)|
  APIKeyService_(?:Create|Delete|Update|Disable|Enable|Rotate)|
  WorkspaceAdminService_(?:Create|Archive|Update|Add|Remove)|
  AIProviderKeyService_(?:Create|Delete|Update)|
  ModelService_(?:Disable|Enable|Swap)|
  WorkspaceSecretService_(?:Create|Delete|Update)|
  ToolService_(?:Create|Delete|Update)ToolSetSecret|
  WidgetSessionService_DeleteTenant
)`.replace(/\s+/g, ''));

const sensitiveRead = new RegExp(String.raw`^(?:
  AccountService_GetAccount|
  GlobalAPIKeyService_GetGlobalAPIKey|
  AIProviderKeyService_(?:ListAIProviderKeys|GetAIProviderKey)|
  APIKeyService_(?:ListAPIKeys|GetAPIKey)|
  ProfilesService_Whoami|
  WorkspaceAdminService_(?:ListProfiles|ListWorkspaceMembers)|
  WorkspaceSecretService_(?:ListWorkspaceSecrets|GetWorkspaceSecret)|
  ToolService_(?:ListToolSetSecrets|GetToolSetSecret)
)$`.replace(/\s+/g, ''));

const permanentSideEffectPattern = new RegExp(String.raw`^(?:
  ObjectiveService_(?:CreateObjective|CreateObjectiveFeedback|CancelObjective|CompactObjective|ContinueObjective)|
  ObjectiveService_(?:ApproveToolCall|DenyToolCall|SetToolCallContent)|
  UploadService_CreateUpload
)$`.replace(/\s+/g, ''));

function extractRunBody(operationID) {
  const marker = `\trun(${JSON.stringify(operationID)}, func() error {\n`;
  const start = driver.indexOf(marker);
  assert.notEqual(start, -1, `missing Go conformance block for ${operationID}`);
  const bodyStart = start + marker.length;
  const end = driver.indexOf('\n\t})', bodyStart);
  assert.notEqual(end, -1, `unterminated Go conformance block for ${operationID}`);
  return driver.slice(bodyStart, end).replace(/^\t\t/gm, '\t').trimEnd();
}

function callOnlySnippet(operation, runBody) {
  const assignment = runBody.match(/^\s*(?:_,|page,|stream,) err := (client\.[^\n]+)$/m);
  const directReturn = runBody.match(/^\s*return (client\.[^\n]+)$/m);
  assert.ok(assignment || directReturn, `cannot find generated Go call for ${operation.id}`);
  const call = assignment?.[1] ?? directReturn[1];

  const warning = [
    '// LIVE TEMPLATE: replace every mock-only "sample" ID/value with the',
    '// recorded owned fixture or a contract-valid value before executing.',
  ];
  if (operation.response.kind === 'sse') {
    return [
      ...warning,
      `stream, err := ${call}`,
      'if err != nil {',
      '\treturn err',
      '}',
      'defer stream.Close()',
      'if !stream.Next() {',
      '\tif err := stream.Err(); err != nil {',
      '\t\treturn err',
      '\t}',
      '\treturn errors.New("objective event stream ended before an event")',
      '}',
      'return nil',
    ].join('\n');
  }
  if (directReturn) return [...warning, `return ${call}`].join('\n');
  return [...warning, `_, err := ${call}`, 'return err'].join('\n');
}

function fixtureDependencies(operation) {
  const dependencies = ['CADENYA_API_KEY'];
  if (operation.path.includes('{workspaceId}')) dependencies.push('CADENYA_WORKSPACE_ID');

  const ids = new Set();
  for (const parameter of operation.pathParams ?? []) {
    if (parameter.name !== 'workspaceId') ids.add(parameter.name);
  }
  if (operation.positional?.name) ids.add(operation.positional.name);

  // Body/query references to other resources are just as important as path
  // IDs. Walk manifest samples rather than trying to maintain 142 hand lists.
  function visit(value, key = '') {
    if (Array.isArray(value)) {
      for (const item of value) visit(item, key);
    } else if (value && typeof value === 'object') {
      for (const [childKey, childValue] of Object.entries(value)) visit(childValue, childKey);
    } else if (typeof value === 'string' && /(?:^id$|Id$)/.test(key)) {
      if (key !== 'workspaceId' && value === 'sample') ids.add(key);
    }
  }
  for (const field of operation.bodyFields ?? []) visit(field.sample, field.name);
  for (const field of operation.queryParams ?? []) visit(field.sample, field.name);
  visit(operation.wholeBody?.sample);

  for (const id of [...ids].sort()) dependencies.push(`fixture.${id}`);
  if ((operation.bodyFields?.length ?? 0) > 0 || operation.wholeBody) {
    dependencies.push('contract-valid request body');
  }
  return dependencies;
}

function classify(operation) {
  const id = operation.id;
  if (privilegedMutationPattern.test(id)) {
    return {
      executionGate: 'manual-only',
      safety: 'privileged-or-secret mutation',
      lifecycle: 'operator-approved; capture and restore state where possible',
    };
  }
  if (sensitiveRead.test(id)) {
    return {
      executionGate: 'manual-sensitive-read',
      safety: 'read-only but response may contain secret or account material',
      lifecycle: 'do not print or persist the response body',
    };
  }
  if (permanentSideEffectPattern.test(id)) {
    return {
      executionGate: 'workflow-fixture-only',
      safety: 'persistent workflow side effect',
      lifecycle: 'use a uniquely labeled acceptance fixture; history may not be deletable',
    };
  }
  if (id.startsWith('AgentScheduleService_') && operation.httpMethod !== 'GET') {
    return {
      executionGate: 'workflow-fixture-only',
      safety: 'scheduled workflow side effect',
      lifecycle: 'use a disabled/future-dated owned schedule; clean it up before it can dispatch',
    };
  }
  if (operation.response.kind === 'sse') {
    return {
      executionGate: 'workflow-fixture-only',
      safety: 'long-lived read tied to an objective fixture',
      lifecycle: 'use a bounded context; close the stream; preserve LastEventID for resume testing',
    };
  }
  if (operation.httpMethod === 'GET') {
    return {
      executionGate: 'safe-read',
      safety: 'read-only',
      lifecycle: 'existing fixture only; do not log sensitive response fields',
    };
  }
  if (operation.httpMethod === 'DELETE') {
    return {
      executionGate: 'owned-fixture-only',
      safety: 'destructive',
      lifecycle: 'delete only a fixture created and recorded by this matrix run',
    };
  }
  return {
    executionGate: 'owned-fixture-only',
    safety: 'state-changing',
    lifecycle: 'mutate only an owned fixture; register compensating cleanup before the call',
  };
}

const operations = {};
for (const operation of manifest.operations) {
  const runBody = extractRunBody(operation.id);
  const completedEvidence = existingLiveEvidence.get(operation.id) ?? null;
  const partialEvidence = attemptedEvidence.get(operation.id) ?? null;
  operations[operation.id] = {
    operationId: operation.id,
    httpMethod: operation.httpMethod,
    path: operation.path,
    resource: operation.resource,
    method: operation.method,
    liveStatus: completedEvidence ? 'completed' : partialEvidence ? 'attempted' : 'not-tested',
    evidence: completedEvidence ?? partialEvidence,
    nonQualifyingEvidence: nonQualifyingEvidence.get(operation.id) ?? null,
    ...classify(operation),
    dependencies: fixtureDependencies(operation),
    snippet: callOnlySnippet(operation, runBody),
  };
}

assert.equal(Object.keys(operations).length, manifest.operations.length);

const catalog = {
  schemaVersion: 1,
  sdk: 'go',
  generatedFrom: [
    'api-spec.yml operationId inventory',
    'gen/manifest/manifest.json request/response metadata',
    'gen/go/conformance/main.go exact generated SDK invocations',
  ],
  statusPolicy: {
    completed:
      'A checked-in program called the real api.cadenya.com success path with the generated Go SDK and asserted/decoded its response.',
    attempted:
      'A checked-in program made a real call, but its assertion did not prove the operation success path (for example, a deliberate 404 or accepted APIError skip).',
    'not-tested':
      'Mock conformance may pass, but no qualifying real-API success-path evidence is recorded.',
  },
  templatePolicy: [
    'Templates are deliberately not executable with their mock-only sample values.',
    'Use CADENYA_API_KEY and CADENYA_WORKSPACE_ID from the environment; never print them.',
    'Replace resource identifiers with fixtures owned by the current matrix run.',
    'Every call must receive a bounded context; every SSE stream must be closed.',
    'Register cleanup before each state-changing request and run cleanup in reverse order under a separate bounded context.',
    'A non-2xx APIError is a failed live test, not evidence that the SDK operation works.',
  ],
  sharedHelpers: `func decode[T any](raw string) *T {
\tvar value T
\tif err := json.Unmarshal([]byte(raw), &value); err != nil {
\t\tpanic(err)
\t}
\treturn &value
}`,
  operationCount: manifest.operations.length,
  operations,
};

const rendered = `${JSON.stringify(catalog, null, 2)}\n`;
if (process.argv.includes('--check')) {
  assert.equal(readFileSync(outputURL, 'utf8'), rendered, 'snippets.json is stale; run generate.mjs');
  console.log(`go live-matrix catalog: ${catalog.operationCount}/${manifest.operations.length} operations current`);
} else {
  writeFileSync(outputURL, rendered);
  console.log(`wrote ${catalog.operationCount} Go live-test templates to ${outputURL.pathname}`);
}
