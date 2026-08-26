// Safe live-read wave for the generated TypeScript SDK.
// Imports every catalogued GET operation, discovers fixture IDs from list
// calls, asserts decoded response shape, and never prints/persists bodies.

import assert from 'node:assert/strict';
import { counts, freshReport, loadLiveEnvironment, safeFailure, writeReport } from './common-node.mjs';
import { typescriptSnippets } from './snippets-typescript.mjs';

loadLiveEnvironment();
const { default: Cadenya } = await import(new URL('../../gen/typescript/dist/index.js', import.meta.url));
const client = new Cadenya();
const report = freshReport('typescript', 'read wave');
const operations = report.operations;
const ids = { workspaceId: process.env.CADENYA_WORKSPACE_ID };
const result = (id, status, evidence) => { operations[id] = { status, evidence }; };
const completed = (id, detail) => result(id, 'completed', `real api.cadenya.com: ${detail}; no response body persisted`);
const blocked = (id, detail) => result(id, 'blocked', `real API read wave: ${detail}`);
const failed = (id, error) => result(id, 'failed', `real API read failed: ${safeFailure(error)}; no response body persisted`);
const metadataId = (value) => value?.metadata?.id ?? value?.id;
const pageItems = (value) => { assert.ok(Array.isArray(value?.items), 'expected page items array'); return value.items; };
const jsonObject = (value) => assert.ok(value && typeof value === 'object', 'expected decoded object');
const opts = () => ({ signal: AbortSignal.timeout(30_000) });

async function call(id, fn, validate = jsonObject) {
  try { const value = await fn(); validate(value); completed(id, 'successful decoded response'); return value; }
  catch (error) {
    const status = Number(error?.status ?? error?.statusCode);
    if (status === 401 || status === 403) blocked(id, `credential authorization prerequisite (HTTP ${status})`);
    else if (status === 501) blocked(id, 'live endpoint reports not implemented (HTTP 501)');
    else failed(id, error);
    return undefined;
  }
}
async function page(id, fn) { return call(id, fn, pageItems); }
async function first(id, fn, key) {
  const value = await page(id, fn);
  const item = value?.items?.[0];
  if (item && key) ids[key] = metadataId(item);
  return item;
}

await call('AccountService_GetAccount', () => client.accounts.retrieve(opts()));
await call('GlobalAPIKeyService_GetGlobalAPIKey', () => client.apiKeys.retrieveGlobal(opts()));
const apiKey = await first('APIKeyService_ListAPIKeys', () => client.apiKeys.list({ limit: 1 }, opts()), 'apiKeyId');
if (apiKey) await call('APIKeyService_GetAPIKey', () => client.apiKeys.retrieve(ids.apiKeyId, undefined, opts()));
await page('WorkspaceAdminService_ListProfiles', () => client.workspaceAdmin.listProfiles({ limit: 1 }, opts()));
await page('WorkspaceAdminService_ListAccountWorkspaces', () => client.workspaceAdmin.listAccount({ limit: 1 }, opts()));
await call('WorkspaceAdminService_GetWorkspace', () => client.workspaceAdmin.retrieve(undefined, opts()));
await page('WorkspaceAdminService_ListWorkspaceMembers', () => client.workspaceAdmin.listMembers({ limit: 1 }, opts()));
await call('ProfilesService_Whoami', () => client.profiles.whoami(opts()));
await page('WorkspaceService_ListWorkspaces', () => client.workspaces.list({ limit: 1 }, opts()));

const agent = await first('AgentService_ListAgents', () => client.agents.list({ limit: 1 }, opts()), 'agentId');
if (ids.agentId) {
  await call('AgentService_GetAgent', () => client.agents.retrieve(ids.agentId, undefined, opts()));
  await page('AgentService_ListAgentFeedback', () => client.agents.listFeedback(ids.agentId, { limit: 1 }, opts()));
  await page('AgentService_ListAgentWebhookDeliveries', () => client.agents.listWebhookDeliveries(ids.agentId, { limit: 1 }, opts()));
  const schedule = await first('AgentScheduleService_ListAgentSchedules', () => client.agents.schedules.list(ids.agentId, { limit: 1 }, opts()), 'agentScheduleId');
  if (schedule) await call('AgentScheduleService_GetAgentSchedule', () => client.agents.schedules.retrieve(ids.agentId, ids.agentScheduleId, undefined, opts()));
  const variation = await first('AgentVariationService_ListAgentVariations', () => client.agents.variations.list(ids.agentId, { limit: 1 }, opts()), 'variationId');
  if (variation) await call('AgentVariationService_GetAgentVariation', () => client.agents.variations.retrieve(ids.agentId, ids.variationId, undefined, opts()));
}

const providerKey = await first('AIProviderKeyService_ListAIProviderKeys', () => client.aiProviderKeys.list({ limit: 1 }, opts()), 'aiProviderKeyId');
if (providerKey) await call('AIProviderKeyService_GetAIProviderKey', () => client.aiProviderKeys.retrieve(ids.aiProviderKeyId, undefined, opts()));
const layer = await first('MemoryService_ListMemoryLayers', () => client.memoryLayers.list({ limit: 1 }, opts()), 'memoryLayerId');
if (layer) {
  await call('MemoryService_GetMemoryLayer', () => client.memoryLayers.retrieve(ids.memoryLayerId, undefined, opts()));
  const entry = await first('MemoryService_ListMemoryEntries', () => client.memoryLayers.entries.list(ids.memoryLayerId, { limit: 1 }, opts()), 'memoryEntryId');
  if (entry) await call('MemoryService_GetMemoryEntry', () => client.memoryLayers.entries.retrieve(ids.memoryLayerId, ids.memoryEntryId, undefined, opts()));
}
const model = await first('ModelService_ListModels', () => client.models.list({ limit: 1 }, opts()), 'modelId');
if (model) await call('ModelService_GetModel', () => client.models.retrieve(ids.modelId, undefined, opts()));

const objective = await first('ObjectiveService_ListObjectives', () => client.objectives.list({ limit: 1 }, opts()), 'objectiveId');
if (objective) {
  await call('ObjectiveService_GetObjective', () => client.objectives.retrieve(ids.objectiveId, undefined, opts()));
  await page('ObjectiveService_ListObjectiveContextWindows', () => client.objectives.listContextWindows(ids.objectiveId, { limit: 1 }, opts()));
  await call('ObjectiveService_GetObjectiveDiagnostics', () => client.objectives.retrieveDiagnostics(ids.objectiveId, undefined, opts()));
  await page('ObjectiveService_ListObjectiveEvents', () => client.objectives.listEvents(ids.objectiveId, { limit: 1 }, opts()));
  await page('ObjectiveService_ListObjectiveFeedback', () => client.objectives.listFeedback(ids.objectiveId, { limit: 1 }, opts()));
  const task = await first('ObjectiveService_ListObjectiveTasks', () => client.objectives.listTasks(ids.objectiveId, { limit: 1 }, opts()), 'objectiveTaskId');
  if (task) await call('ObjectiveService_GetObjectiveTask', () => client.objectives.retrieveTask(ids.objectiveId, ids.objectiveTaskId, undefined, opts()));
  else blocked('ObjectiveService_GetObjectiveTask', 'no list-derived objective task fixture');
  const toolCall = await first('ObjectiveService_ListObjectiveToolCalls', () => client.objectives.listToolCalls(ids.objectiveId, { limit: 1 }, opts()), 'toolCallId');
  if (toolCall) await call('ObjectiveService_GetObjectiveToolCall', () => client.objectives.retrieveToolCall(ids.objectiveId, ids.toolCallId, undefined, opts()));
  await page('ObjectiveService_ListObjectiveTools', () => client.objectives.listTools(ids.objectiveId, { limit: 1 }, opts()));
  blocked('ObjectiveEventStreamsService_StreamObjectiveEvents', 'specialized replay fixture runs in a later fresh wave');
}

await call('SearchService_SearchToolsOrToolSets', () => client.toolSearch.searchOrSets({ query: '__redwood_live_matrix_no_match__' }, opts()));
const tenant = await first('TenantService_ListTenants', () => client.tenants.list({ limit: 1 }, opts()), 'tenantId');
if (tenant) {
  await call('TenantService_GetTenant', () => client.tenants.retrieve(ids.tenantId, undefined, opts()));
  await page('TenantService_ListTenantSubjects', () => client.tenants.listSubjects(ids.tenantId, { limit: 1 }, opts()));
}
const toolSet = await first('ToolService_ListToolSets', () => client.toolSets.list({ limit: 1 }, opts()), 'toolSetId');
if (toolSet) {
  await call('ToolService_GetToolSet', () => client.toolSets.retrieve(ids.toolSetId, undefined, opts()));
  await page('ToolService_ListToolSetEvents', () => client.toolSets.listEvents(ids.toolSetId, { limit: 1 }, opts()));
  blocked('ToolService_GetToolSetOpenAPISpec', 'arbitrary discovered tool set is not known to use the OpenAPI adapter');
  await page('ToolService_ListToolSetUsage', () => client.toolSets.listUsage(ids.toolSetId, { limit: 1 }, opts()));
  const toolSetSecret = await first('ToolService_ListToolSetSecrets', () => client.toolSets.secrets.list(ids.toolSetId, { limit: 1 }, opts()), 'toolSetSecretId');
  if (toolSetSecret) await call('ToolService_GetToolSetSecret', () => client.toolSets.secrets.retrieve(ids.toolSetId, ids.toolSetSecretId, undefined, opts()));
  const tool = await first('ToolService_ListTools', () => client.toolSets.tools.list(ids.toolSetId, { limit: 1 }, opts()), 'toolId');
  if (tool) await call('ToolService_GetTool', () => client.toolSets.tools.retrieve(ids.toolSetId, ids.toolId, undefined, opts()));
}
const widgetSession = await first('WidgetSessionService_ListWidgetSessions', () => client.widgetSessions.list({ limit: 1 }, opts()), 'widgetSessionId');
if (widgetSession) await call('WidgetSessionService_GetWidgetSession', () => client.widgetSessions.retrieve(ids.widgetSessionId, undefined, opts()));
const widget = await first('WidgetService_ListWidgets', () => client.widgets.list({ limit: 1 }, opts()), 'widgetId');
if (widget) await call('WidgetService_GetWidget', () => client.widgets.retrieve(ids.widgetId, undefined, opts()));
const workspaceSecret = await first('WorkspaceSecretService_ListWorkspaceSecrets', () => client.workspaceSecrets.list({ limit: 1 }, opts()), 'workspaceSecretId');
if (workspaceSecret) await call('WorkspaceSecretService_GetWorkspaceSecret', () => client.workspaceSecrets.retrieve(ids.workspaceSecretId, undefined, opts()));

for (const [id, record] of Object.entries(typescriptSnippets)) {
  if (record.method !== 'GET' || !operations[id].evidence.includes('not reached')) continue;
  blocked(id, `no fresh read fixture discovered (${record.fixtureKeys.join(', ') || 'fixture unavailable'})`);
}
writeReport(new URL('./results-typescript.json', import.meta.url), report);
const summary = counts(report);
console.log(JSON.stringify(summary));
if (summary.failed) process.exitCode = 1;
