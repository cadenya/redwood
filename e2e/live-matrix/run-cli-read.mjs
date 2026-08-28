// Safe real-API GET wave for the generated CLI. Uses the same endpoint and
// fixture sequence as the TypeScript wave, invokes each generated command
// directly against the real API, and asserts JSON shape.
// Response bodies and secrets are never persisted or printed.

import assert from 'node:assert/strict';
import { execFile, execFileSync } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import { configuredCliBinary, counts, freshReport, loadLiveEnvironment, manifest, safeFailure, writeReport } from './common-node.mjs';
import { cliInputArgs, cliSnippets } from './snippets-cli.mjs';

const exec = promisify(execFile);
loadLiveEnvironment();
const binary = configuredCliBinary() ?? join(mkdtempSync(join(tmpdir(), 'cadenya-cli-read-')), 'cadenya');
if (!process.env.CADENYA_CLI_BINARY) {
  execFileSync('go', ['build', '-o', binary, '.'], { cwd: new URL('../../gen/cli', import.meta.url), timeout: 300_000 });
}
const report = freshReport('cli', 'read wave');
const operations = report.operations;
const ids = { workspaceId: process.env.CADENYA_WORKSPACE_ID };
const kebab = (s) => s.replace(/_/g, '-').replace(/([a-z0-9])([A-Z])/g, '$1-$2').toLowerCase();
const genericId = (op) => {
  const marker = op.path.lastIndexOf('/{id}');
  const segment = op.path.slice(0, marker).split('/').at(-1);
  return {
    agents: 'agentId', schedules: 'agentScheduleId', variations: 'variationId',
    ai_provider_keys: 'aiProviderKeyId', api_keys: 'apiKeyId', memory_layers: 'memoryLayerId',
    entries: 'memoryEntryId', models: 'modelId', objectives: 'objectiveId',
    tasks: 'objectiveTaskId', tenants: 'tenantId', tool_sets: 'toolSetId',
    secrets: op.path.includes('/tool_sets/') ? 'toolSetSecretId' : 'workspaceSecretId',
    tools: 'toolId', uploads: 'uploadId', widget_sessions: 'widgetSessionId',
    widgets: 'widgetId', workspaces: 'workspaceId',
  }[segment];
};
const valueId = (v) => v?.metadata?.id ?? v?.id;
const completed = (id) => operations[id] = { status: 'completed', evidence: 'real api.cadenya.com: CLI exit 0 and decoded expected JSON shape; no response body persisted' };
const failed = (id, e) => operations[id] = { status: 'failed', evidence: `real API CLI read failed: ${safeFailure(e)}; no response body persisted` };
const blocked = (id, why) => operations[id] = { status: 'blocked', evidence: `real API CLI read wave: ${why}` };

function argv(op) {
  const args = [...op.resource.split('.').map(kebab), kebab(op.method)];
  args.push('--display', 'json');
  for (const positional of op.positionals ?? []) {
    args.push(ids[positional.name === 'id' ? genericId(op) : positional.name]);
  }
  for (const p of op.pathParams) args.push(`--${kebab(p.name)}`, ids[p.name]);
  const inputs = {};
  if (op.id === 'SearchService_SearchToolsOrToolSets') inputs.query = '__redwood_live_matrix_no_match__';
  args.push(...cliInputArgs(op.id, inputs));
  if (op.queryParams.some((p) => p.name === 'limit')) args.push('--limit', '1');
  return args;
}

async function run(op) {
  const missing = cliSnippets[op.id].fixtureKeys.filter((k) => k.startsWith('ids.') && !ids[k.slice(4)]);
  if (missing.length) return blocked(op.id, `no safe live fixture discovered (${missing.join(', ')})`);
  try {
    const { stdout } = await exec(binary, argv(op), {
      env: { ...process.env }, encoding: 'utf8', timeout: 30_000, maxBuffer: 2_000_000,
    });
    const parsed = JSON.parse(stdout);
    if (op.pagination) assert.ok(Array.isArray(parsed?.items), 'expected page items array');
    else assert.ok(parsed && typeof parsed === 'object', 'expected decoded JSON object');
    completed(op.id);
    const first = op.pagination ? parsed.items[0] : parsed;
    const key = op.id.replace(/^.*_(?:List|Get)/, '');
    // Fixture acquisition is explicit below; this fallback never logs values.
    return { parsed, first, key };
  } catch (e) {
    const diagnostic = String(e?.stderr ?? '');
    if (/\b(?:401|403)\b/.test(diagnostic)) blocked(op.id, 'credential authorization prerequisite');
    else if (/\b501\b|not implemented/i.test(diagnostic)) blocked(op.id, 'live endpoint reports not implemented');
    else failed(op.id, e);
    return undefined;
  }
}

const byId = Object.fromEntries(manifest.operations.map((op) => [op.id, op]));
async function capture(id, key) { const r = await run(byId[id]); if (r?.first && key) ids[key] = valueId(r.first); return r; }

for (const id of ['AccountService_GetAccount','GlobalAPIKeyService_GetGlobalAPIKey']) await run(byId[id]);
await capture('APIKeyService_ListAPIKeys','apiKeyId'); await run(byId.APIKeyService_GetAPIKey);
for (const id of ['WorkspaceAdminService_ListProfiles','WorkspaceAdminService_ListAccountWorkspaces','WorkspaceAdminService_GetWorkspace','WorkspaceAdminService_ListWorkspaceMembers']) await run(byId[id]);
for (const id of ['ProfilesService_Whoami','WorkspaceService_ListWorkspaces']) await run(byId[id]);
await capture('AgentService_ListAgents','agentId');
for (const id of ['AgentService_GetAgent','AgentService_ListAgentFeedback','AgentService_ListAgentWebhookDeliveries']) await run(byId[id]);
await capture('AgentScheduleService_ListAgentSchedules','agentScheduleId'); await run(byId.AgentScheduleService_GetAgentSchedule);
await capture('AgentVariationService_ListAgentVariations','variationId'); await run(byId.AgentVariationService_GetAgentVariation);
await capture('AIProviderKeyService_ListAIProviderKeys','aiProviderKeyId'); await run(byId.AIProviderKeyService_GetAIProviderKey);
await capture('MemoryService_ListMemoryLayers','memoryLayerId'); await run(byId.MemoryService_GetMemoryLayer);
await capture('MemoryService_ListMemoryEntries','memoryEntryId'); await run(byId.MemoryService_GetMemoryEntry);
await capture('ModelService_ListModels','modelId'); await run(byId.ModelService_GetModel);
await capture('ObjectiveService_ListObjectives','objectiveId');
for (const id of ['ObjectiveService_GetObjective','ObjectiveService_ListObjectiveContextWindows','ObjectiveService_GetObjectiveDiagnostics','ObjectiveService_ListObjectiveEvents','ObjectiveService_ListObjectiveFeedback']) await run(byId[id]);
await capture('ObjectiveService_ListObjectiveTasks','objectiveTaskId'); await run(byId.ObjectiveService_GetObjectiveTask);
await capture('ObjectiveService_ListObjectiveToolCalls','toolCallId'); await run(byId.ObjectiveService_GetObjectiveToolCall);
await run(byId.ObjectiveService_ListObjectiveTools);
blocked('ObjectiveEventStreamsService_StreamObjectiveEvents','requires bounded replay/live objective scenario');
await run(byId.SearchService_SearchToolsOrToolSets);
await capture('TenantService_ListTenants','tenantId'); await run(byId.TenantService_GetTenant); await run(byId.TenantService_ListTenantSubjects);
await capture('ToolService_ListToolSets','toolSetId');
for (const id of ['ToolService_GetToolSet','ToolService_ListToolSetEvents','ToolService_ListToolSetUsage']) await run(byId[id]);
blocked('ToolService_GetToolSetOpenAPISpec','arbitrary discovered tool set is not known to use the OpenAPI adapter');
await capture('ToolService_ListToolSetSecrets','toolSetSecretId'); await run(byId.ToolService_GetToolSetSecret);
await capture('ToolService_ListTools','toolId'); await run(byId.ToolService_GetTool);
await capture('WidgetSessionService_ListWidgetSessions','widgetSessionId'); await run(byId.WidgetSessionService_GetWidgetSession);
await capture('WidgetService_ListWidgets','widgetId'); await run(byId.WidgetService_GetWidget);
await capture('WorkspaceSecretService_ListWorkspaceSecrets','workspaceSecretId'); await run(byId.WorkspaceSecretService_GetWorkspaceSecret);
for (const [id, record] of Object.entries(cliSnippets)) if (record.method === 'GET' && operations[id].evidence.includes('not reached')) blocked(id, `no fresh read fixture discovered (${record.fixtureKeys.join(', ') || 'fixture unavailable'})`);
writeReport(new URL('./results-cli.json', import.meta.url), report);
const summary = counts(report);
console.log(JSON.stringify(summary));
if (summary.failed) process.exitCode=1;
