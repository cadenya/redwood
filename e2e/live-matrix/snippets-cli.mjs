// CLI live-test snippets for every operation in the generated manifest.
//
// This file is descriptive and executable by a future matrix runner; importing
// it never invokes the CLI. The runner should spawn the generated binary with
// the exported argv rather than evaluate a shell string. That keeps API keys,
// secrets, JSON documents, and user-controlled fixture values out of shell
// parsing and process output.

import { readFileSync } from 'node:fs';

const manifest = JSON.parse(
  readFileSync(new URL('../../gen/manifest/manifest.json', import.meta.url), 'utf8'),
);

const kebab = (value) => value
  .replace(/_/g, '-')
  .replace(/([a-z0-9])([A-Z])/g, '$1-$2')
  .toLowerCase();
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
const fixtureRef = (op, name) => `fixtures.ids.${idKey(op, name)}`;

function includedInputFields(op) {
  const fields = [...op.queryParams, ...op.bodyFields].filter((field) => field.required);
  if (op.wholeBody) fields.push({ name: 'body', required: true });

  // PATCH/update requests frequently declare every field optional. A live
  // scenario still supplies one deliberately chosen valid field rather than
  // claiming an empty patch exercises useful behavior.
  if (fields.length === 0 && op.bodyFields.length > 0) fields.push(op.bodyFields[0]);
  return fields;
}

function fixtureKeys(op) {
  const keys = new Set();
  for (const positional of op.positionals ?? []) keys.add(`ids.${idKey(op, positional.name)}`);
  for (const param of op.pathParams) keys.add(`ids.${idKey(op, param.name)}`);
  for (const field of includedInputFields(op)) {
    keys.add(field.name.endsWith('Id')
      ? `ids.${idKey(op, field.name)}`
      : `inputs.${op.id}.${field.name}`);
  }
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

function argvExpressions(op) {
  const argv = op.resource.split('.').map((part) => JSON.stringify(kebab(part)));
  argv.push(JSON.stringify(kebab(op.method)));
  for (const positional of op.positionals ?? []) argv.push(fixtureRef(op, positional.name));
  for (const param of op.pathParams) {
    argv.push(JSON.stringify(`--${kebab(param.name)}`), fixtureRef(op, param.name));
  }
  const inputFields = includedInputFields(op).filter((field) => !field.name.endsWith('Id'));
  for (const field of includedInputFields(op).filter((item) => item.name.endsWith('Id'))) {
    argv.push(JSON.stringify(`--${kebab(field.name)}`), fixtureRef(op, field.name));
  }
  if (inputFields.length > 0) {
    argv.push(`...cliInputArgs(${JSON.stringify(op.id)}, fixtures.inputs.${op.id})`);
  }
  return argv;
}

/**
 * Serialize a scenario's selected fields exactly as the generated CLI expects.
 * Values are returned as argv entries, never as a shell command: arrays become
 * repeated flags, JSON values stay intact, and booleans use --flag=true/false.
 */
export function cliInputArgs(operationId, input) {
  const op = manifest.operations.find((candidate) => candidate.id === operationId);
  if (!op) throw new Error(`unknown operationId: ${operationId}`);
  const argv = [];
  for (const field of includedInputFields(op).filter((item) => !item.name.endsWith('Id'))) {
    if (input?.[field.name] === undefined) {
      throw new Error(`missing fixtures.inputs.${operationId}.${field.name}`);
    }
    // A discriminated-union body is driven through its arm flag: the fixture
    // body's tag picks the arm, and the CLI stamps the tag itself.
    const arm = field.name === 'body' && op.wholeBody?.choices
      ? op.wholeBody.choices.find((c) => c.tag === input.body?.type)
      : undefined;
    if (field.name === 'body' && op.wholeBody?.choices && !arm) {
      throw new Error(`fixtures.inputs.${operationId}.body.type must name a union arm`);
    }
    const flag = `--${kebab(arm ? arm.name : field.name)}`;
    const raw = arm ? input.body[arm.name] ?? input.body : input[field.name];
    const values = Array.isArray(raw) ? raw : [raw];
    for (const value of values) {
      const encoded = typeof value === 'object' && value !== null ? JSON.stringify(value) : String(value);
      if (typeof value === 'boolean') argv.push(`${flag}=${encoded}`);
      else argv.push(flag, encoded);
    }
  }
  return argv;
}

function snippet(op) {
  const args = argvExpressions(op);
  return `await runCli([\n  ${args.join(',\n  ')},\n], { redactOutput: ${
    safety(op).credentialSensitive ? 'true' : 'false'
  } });`;
}

export const cliSnippets = Object.fromEntries(
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

if (Object.keys(cliSnippets).length !== manifest.operations.length) {
  throw new Error('CLI live snippets do not cover every manifest operation');
}

export default cliSnippets;
