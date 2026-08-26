// TypeScript live-test snippets for every operation in the generated manifest.
//
// This file does not call the API. It is a deterministic source for the live
// matrix and its runner. A snippet is runnable after the runner supplies:
//
//   client         a generated Cadenya client
//   fixtures.ids   real IDs named by `fixtureKeys`
//   fixtures.inputs[operationId]
//                  a valid, run-scoped input object for body/required-query
//                  fields (only required when named by `fixtureKeys`)
//   requestOptions bounded AbortSignal etc.; never automatic mutation retries
//
// Inputs are intentionally not synthesized from manifest `sample` values:
// those values prove wire conformance, not that a request is valid or safe
// against a live workspace.

import { readFileSync } from 'node:fs';

const manifest = JSON.parse(
  readFileSync(new URL('../../gen/manifest/manifest.json', import.meta.url), 'utf8'),
);

const camel = (value) => value.replace(/_([a-z0-9])/g, (_, char) => char.toUpperCase());

const entityForGenericId = (op) => {
  const marker = op.path.lastIndexOf('/{id}');
  if (marker < 0) return 'id';
  const segment = op.path.slice(0, marker).split('/').at(-1);
  return {
    agents: 'agentId',
    schedules: 'agentScheduleId',
    variations: 'variationId',
    ai_provider_keys: 'aiProviderKeyId',
    api_keys: 'apiKeyId',
    memory_layers: 'memoryLayerId',
    entries: 'memoryEntryId',
    models: 'modelId',
    objectives: 'objectiveId',
    tasks: 'objectiveTaskId',
    tenants: 'tenantId',
    tool_sets: 'toolSetId',
    secrets: op.path.includes('/tool_sets/') ? 'toolSetSecretId' : 'workspaceSecretId',
    tools: 'toolId',
    uploads: 'uploadId',
    widget_sessions: 'widgetSessionId',
    widgets: 'widgetId',
    workspaces: 'workspaceId',
  }[segment] ?? `${camel(segment.replace(/s$/, ''))}Id`;
};

const idKey = (op, name) => (name === 'id' ? entityForGenericId(op) : name);
const idExpression = (op, name) => `fixtures.ids.${idKey(op, name)}`;

const hasInput = (op) =>
  Boolean(op.wholeBody) ||
  op.bodyFields.length > 0 ||
  op.queryParams.some((param) => param.required && !param.name.endsWith('Id'));

function fixtureKeys(op) {
  const keys = new Set();
  for (const positional of op.positionals ?? []) keys.add(`ids.${idKey(op, positional.name)}`);
  for (const param of op.pathParams) keys.add(`ids.${idKey(op, param.name)}`);
  for (const param of [...op.queryParams, ...op.bodyFields].filter((item) => item.required)) {
    if (param.name.endsWith('Id')) keys.add(`ids.${idKey(op, param.name)}`);
  }
  if (hasInput(op)) keys.add(`inputs.${op.id}`);
  return [...keys];
}

const fixtureProducer = {
  'ids.profileId': 'ProfilesService_Whoami',
  'ids.agentId': 'AgentService_CreateAgent',
  'ids.agentScheduleId': 'AgentScheduleService_CreateAgentSchedule',
  'ids.variationId': 'AgentVariationService_CreateAgentVariation',
  'ids.assignmentId': 'AgentVariationService_AddAgentVariationAssignment',
  'ids.aiProviderKeyId': 'AIProviderKeyService_CreateAIProviderKey',
  'ids.apiKeyId': 'APIKeyService_CreateAPIKey',
  'ids.memoryLayerId': 'MemoryService_CreateMemoryLayer',
  'ids.memoryLayerAssignmentId': 'AgentVariationService_AddAgentVariationMemoryLayer',
  'ids.memoryEntryId': 'MemoryService_CreateMemoryEntry',
  'ids.modelId': 'ModelService_ListModels',
  'ids.objectiveId': 'ObjectiveService_CreateObjective',
  'ids.objectiveTaskId': 'ObjectiveService_ListObjectiveTasks',
  'ids.toolCallId': 'ObjectiveService_ListObjectiveToolCalls',
  'ids.tenantId': 'TenantService_ListTenants',
  'ids.toolSetId': 'ToolService_CreateToolSet',
  'ids.toolSetSecretId': 'ToolService_CreateToolSetSecret',
  'ids.toolId': 'ToolService_CreateTool',
  'ids.uploadId': 'UploadService_CreateUpload',
  'ids.widgetSessionId': 'WidgetSessionService_CreateWidgetSession',
  'ids.widgetId': 'WidgetService_CreateWidget',
  'ids.workspaceSecretId': 'WorkspaceSecretService_CreateWorkspaceSecret',
};

const dependencies = (op) => [...new Set(
  fixtureKeys(op).map((key) => fixtureProducer[key]).filter((id) => id && id !== op.id),
)];

function safety(op) {
  const credentialSensitive = /(?:APIKey|ProviderKey|Secret|SigningKey|ChallengeToken)/.test(op.id)
    || op.id === 'AccountService_GetAccount';
  const destructive = op.httpMethod === 'DELETE'
    || /_(?:Archive|Disable|Revoke|Remove|Deny|Cancel)/.test(op.id);
  const mutating = op.httpMethod !== 'GET';
  const accountCritical = /GlobalAPIKey|RotateChallengeToken|RotateWebhookSigningKey|ArchiveWorkspace/.test(op.id);
  return {
    riskClass: accountCritical
      ? 'account_critical'
      : op.httpMethod === 'GET'
        ? 'read_only'
        : op.httpMethod === 'DELETE'
          ? 'destructive_test_resource'
          : 'mutating_test_resource',
    classification: destructive ? 'destructive' : mutating ? 'mutating' : 'read-only',
    credentialSensitive,
    explicitOptInRequired: mutating,
    cleanupOperations: cleanupOperations[op.id] ?? [],
  };
}

const cleanupOperations = {
  APIKeyService_CreateAPIKey: ['APIKeyService_DeleteAPIKey'],
  WorkspaceAdminService_CreateWorkspace: ['WorkspaceAdminService_ArchiveWorkspace'],
  AgentService_CreateAgent: ['AgentService_DeleteAgent'],
  AgentScheduleService_CreateAgentSchedule: ['AgentScheduleService_DeleteAgentSchedule'],
  AgentVariationService_CreateAgentVariation: ['AgentVariationService_DeleteAgentVariation'],
  AgentVariationService_AddAgentVariationAssignment: ['AgentVariationService_RemoveAgentVariationAssignment'],
  AgentVariationService_AddAgentVariationMemoryLayer: ['AgentVariationService_RemoveAgentVariationMemoryLayer'],
  AIProviderKeyService_CreateAIProviderKey: ['AIProviderKeyService_DeleteAIProviderKey'],
  MemoryService_CreateMemoryLayer: ['MemoryService_DeleteMemoryLayer'],
  MemoryService_CreateMemoryEntry: ['MemoryService_DeleteMemoryEntry'],
  ObjectiveService_CreateObjective: ['ObjectiveService_CancelObjective'],
  ToolService_CreateToolSet: ['ToolService_DeleteToolSet'],
  ToolService_CreateToolSetSecret: ['ToolService_DeleteToolSetSecret'],
  ToolService_CreateTool: ['ToolService_DeleteTool'],
  WidgetSessionService_CreateWidgetSession: ['WidgetSessionService_DeleteWidgetSession'],
  WidgetService_CreateWidget: ['WidgetService_DeleteWidget'],
  WorkspaceSecretService_CreateWorkspaceSecret: ['WorkspaceSecretService_DeleteWorkspaceSecret'],
};

function paramsExpression(op) {
  const entries = [];
  if (hasInput(op)) entries.push(`...fixtures.inputs.${op.id}`);

  // Put resource identifiers after the scenario input so an input file can
  // never accidentally redirect a test to an unrelated live resource.
  for (const param of op.pathParams) {
    entries.push(`${param.name}: ${idExpression(op, param.name)}`);
  }
  for (const param of [...op.queryParams, ...op.bodyFields].filter(
    (item) => item.required && item.name.endsWith('Id'),
  )) {
    entries.push(`${param.name}: ${idExpression(op, param.name)}`);
  }
  if (op.wholeBody) entries.push(`body: fixtures.inputs.${op.id}.body`);
  return entries.length ? `{ ${entries.join(', ')} }` : null;
}

function invocation(op) {
  const accessor = op.resource.split('.').map(camel).join('.');
  const args = [];
  for (const positional of op.positionals ?? []) args.push(idExpression(op, positional.name));
  const params = paramsExpression(op);
  if (params) args.push(params);
  args.push('requestOptions');
  return `client.${accessor}.${camel(op.method)}(${args.join(', ')})`;
}

function snippet(op) {
  const call = invocation(op);
  if (op.response.kind !== 'sse') return `await ${call};`;
  return [
    `const stream = await ${call};`,
    'for await (const event of stream) {',
    '  assertObjectiveEvent(event);',
    '  break; // the runner aborts its bounded stream after proving one event',
    '}',
  ].join('\n');
}

export const typescriptSnippets = Object.fromEntries(
  manifest.operations.map((op) => [
    op.id,
    {
      operationId: op.id,
      method: op.httpMethod,
      path: op.path,
      snippet: snippet(op),
      fixtureKeys: fixtureKeys(op),
      dependencies: dependencies(op),
      safety: safety(op),
    },
  ]),
);

if (Object.keys(typescriptSnippets).length !== manifest.operations.length) {
  throw new Error('TypeScript live snippets do not cover every manifest operation');
}

export default typescriptSnippets;
