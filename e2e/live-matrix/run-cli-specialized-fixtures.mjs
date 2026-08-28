// Opt-in end-to-end acceptance flow for the generated CLI. It builds a
// temporary binary, uses uniquely owned fixtures, and retains no API bodies.

import assert from 'node:assert/strict';
import { execFile, execFileSync, spawn } from 'node:child_process';
import { mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { promisify } from 'node:util';
import {
  configuredCliBinary,
  counts,
  loadLiveEnvironment,
  readReport,
  resourceId,
  safeFailure,
  writeReport,
} from './common-node.mjs';

if (process.env.CADENYA_LIVE_SPECIALIZED_FIXTURES !== 'cli') {
  console.error('refusing specialized sweep: set CADENYA_LIVE_SPECIALIZED_FIXTURES=cli');
  process.exit(2);
}

loadLiveEnvironment();
const exec = promisify(execFile);
const binary = configuredCliBinary() ?? join(mkdtempSync(join(tmpdir(), 'cadenya-cli-specialized-')), 'cadenya');
if (!process.env.CADENYA_CLI_BINARY) {
  execFileSync('go', ['build', '-o', binary, '.'], {
    cwd: new URL('../../gen/cli', import.meta.url),
    timeout: 300_000,
  });
}
const resultPath = new URL('./results-cli.json', import.meta.url);
const report = readReport(resultPath, 'cli');
const operations = report.operations;
const run = `specialized-cli-${Date.now().toString(36)}`;
const cleanup = [];
const json = JSON.stringify;
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function complete(operationId, detail) {
  operations[operationId] = {
    status: 'completed',
    evidence: `real api.cadenya.com: generated CLI specialized fixture succeeded; ${detail}; no response body or resource ID persisted`,
  };
}

function block(operationId, detail) {
  if (operations[operationId]?.status === 'completed') return;
  operations[operationId] = {
    status: 'blocked',
    evidence: `real api.cadenya.com: generated CLI specialized fixture prerequisite; ${detail}`,
  };
}

function fail(operationId, error) {
  if (operations[operationId]?.status === 'completed') return;
  operations[operationId] = {
    status: 'failed',
    evidence: `real generated CLI specialized fixture failed: ${safeFailure(error)}; no response body or resource ID persisted`,
  };
}

async function command(args, milliseconds = 30_000) {
  const { stdout } = await exec(binary, ['--display', 'json', ...args], {
    env: { ...process.env },
    encoding: 'utf8',
    timeout: milliseconds,
    maxBuffer: 2_000_000,
  });
  return stdout.trim() ? JSON.parse(stdout) : undefined;
}

async function poll(label, callback, milliseconds = 120_000) {
  const deadline = Date.now() + milliseconds;
  let lastError;
  while (Date.now() < deadline) {
    try {
      const value = await callback();
      if (value) return value;
    } catch (error) {
      lastError = error;
    }
    await delay(2_000);
  }
  throw new Error(`${label} timed out${lastError ? ` (${safeFailure(lastError)})` : ''}`);
}

async function createToolSet(suffix, adapter) {
  const args = [
    'tool-sets', 'create',
    '--name', `${run}-${suffix}`,
    '--label', `liveMatrix=${run}`,
    '--description', 'specialized live matrix fixture',
    '--adapter', adapter.type,
  ];
  if (adapter.type === 'openapi') {
    args.push(
      '--openapi', adapter.openapi.type,
      '--openapi-url', adapter.openapi.url,
      '--openapi-base-url', adapter.openapi.baseUrl,
    );
  } else if (adapter.type === 'mcp') {
    args.push(
      '--mcp-url', adapter.mcp.url,
      '--mcp-tool-approvals', adapter.mcp.toolApprovals.type,
      '--mcp-tool-approvals-only-filter', json(adapter.mcp.toolApprovals.only.filters[0]),
      '--mcp-tool-approvals-only-operator', 'and',
    );
  }
  const value = await command(args);
  const id = resourceId(value);
  assert.ok(id, 'created tool set omitted metadata.id');
  cleanup.push(async () => {
    try { await command(['tool-sets', 'archive', id]); } catch {}
    await command(['tool-sets', 'delete', id]);
  });
  return id;
}

async function createObjective(agentId, variationId, suffix, firstUserMessage) {
  const value = await command([
    'objectives', 'create',
    '--agent-id', agentId,
    '--variation-id', variationId,
    '--label', `liveMatrix=${run}`,
    '--label', `case=${suffix}`,
    '--system-prompt-data', '{}',
    '--first-user-message', firstUserMessage,
  ], 120_000);
  const id = resourceId(value);
  assert.ok(id, 'created objective omitted metadata.id');
  cleanup.push(async () => {
    try { await command(['objectives', 'cancel', id, '--reason', 'specialized fixture cleanup']); } catch {}
  });
  complete('ObjectiveService_CreateObjective', 'created and decoded independent owned objectives');
  return id;
}

async function waitForApproval(objectiveId) {
  return poll('tool approval request', async () => {
    const page = await command(['objectives', 'list-tool-calls', objectiveId, '--limit', '20']);
    return page.items?.find((item) => item.status === 'TOOL_CALL_STATUS_WAITING_FOR_APPROVAL');
  });
}

async function waitForObjective(objectiveId, predicate, label) {
  return poll(label, async () => {
    const objective = await command(['objectives', 'retrieve', objectiveId]);
    return predicate(objective) ? objective : undefined;
  });
}

async function streamUntil(objectiveId, checkpoint, expectedType, milliseconds = 120_000) {
  return new Promise((resolve, reject) => {
    const args = ['--display', 'json', 'objectives', 'stream-events', objectiveId];
    if (checkpoint) args.push('--last-event-id', checkpoint);
    const child = spawn(binary, args, { env: { ...process.env }, stdio: ['ignore', 'pipe', 'pipe'] });
    let buffer = '';
    let stderr = '';
    let settled = false;
    const finish = (error, value) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      child.kill('SIGTERM');
      if (error) reject(error); else resolve(value);
    };
    const timer = setTimeout(() => finish(new Error('CLI SSE expected event timed out')), milliseconds);
    child.stderr.on('data', (chunk) => { stderr = `${stderr}${chunk}`.slice(-2_000); });
    child.stdout.on('data', (chunk) => {
      buffer += chunk;
      while (buffer.includes('\n')) {
        const index = buffer.indexOf('\n');
        const line = buffer.slice(0, index).trim();
        buffer = buffer.slice(index + 1);
        if (!line) continue;
        try {
          const event = JSON.parse(line);
          if (!expectedType || event?.data?.type === expectedType) finish(undefined, event);
        } catch (error) {
          finish(new Error(`CLI SSE emitted malformed NDJSON (${safeFailure(error)})`));
        }
      }
    });
    child.on('error', (error) => finish(error));
    child.on('exit', (code, signal) => {
      if (!settled && code !== null) finish(new Error(`CLI SSE exited before expected event (code=${code})`));
      else if (!settled && signal) finish(new Error(`CLI SSE ended before expected event (signal=${signal})`));
      void stderr;
    });
  });
}

let fatal;
try {
  const petstoreId = await createToolSet('petstore', {
    type: 'openapi',
    openapi: {
      type: 'url',
      url: 'https://petstore3.swagger.io/api/v3/openapi.json',
      baseUrl: 'https://petstore3.swagger.io/api/v3',
    },
  });
  const petstoreSpec = await poll('Petstore OpenAPI ingestion', async () => {
    const response = await command(['tool-sets', 'retrieve-open-api-spec', petstoreId]);
    if (!response?.spec) return undefined;
    const parsed = JSON.parse(response.spec);
    return parsed?.openapi && Object.keys(parsed.paths ?? {}).length >= 10 ? parsed : undefined;
  });
  assert.ok(Object.keys(petstoreSpec.paths).length >= 10);
  complete('ToolService_GetToolSetOpenAPISpec', 'URL adapter returned and decoded a consumed Petstore document with at least ten paths');

  const fakerId = await createToolSet('faker-mcp', {
    type: 'mcp',
    mcp: {
      url: 'https://free.cadenya.com/faker-mcp',
      toolApprovals: {
        type: 'only',
        only: {
          operator: 'OPERATOR_AND',
          filters: [{
            attribute: 'ATTRIBUTE_NAME',
            matcher: { type: 'contains', contains: 'Curse', caseSensitive: false },
          }],
        },
      },
    },
  });
  const fakerTools = await poll('faker MCP tool sync', async () => {
    const page = await command(['tool-sets', 'tools', 'list', fakerId, '--limit', '20']);
    return page.items?.length >= 3 ? page.items : undefined;
  });
  const byName = new Map(fakerTools.map((tool) => [tool.spec.llmToolName, tool]));
  assert.deepEqual(new Set(byName.keys()), new Set(['GenerateCurseWord', 'GenerateFake', 'GetFakerOptions']));
  assert.equal(byName.get('GenerateCurseWord')?.spec.requiresApproval, true);
  assert.equal(byName.get('GenerateFake')?.spec.requiresApproval, false);
  assert.equal(byName.get('GetFakerOptions')?.spec.requiresApproval, false);

  const bareId = await createToolSet('bare-content', { type: 'bare', bare: {} });
  const bareTool = await command([
    'tool-sets', 'tools', 'create', bareId,
    '--name', `${run}-provide-content`,
    '--description', 'Request externally supplied live-test content.',
    '--requires-approval=true',
    '--parameter', 'type=object',
    '--config', 'bare',
  ]);
  const bareToolId = resourceId(bareTool);
  assert.ok(bareToolId, 'bare tool omitted metadata.id');
  cleanup.push(() => command(['tool-sets', 'tools', 'delete', bareId, bareToolId]));

  const models = await command(['models', 'list', '--limit', '50']);
  const modelId = resourceId(models.items?.find((item) => resourceId(item)));
  assert.ok(modelId, 'workspace has no readable model fixture');
  const agent = await command([
    'agents', 'create',
    '--name', `${run}-agent`,
    '--label', `liveMatrix=${run}`,
    '--variation-selection-mode', 'random',
    '--default-variation-name', `${run}-variation`,
    '--default-variation-label', `liveMatrix=${run}`,
    '--default-variation-system-prompt-template', 'You are an integration-test agent. Follow explicit tool-use instructions.',
    '--default-variation-model-id', modelId,
    '--default-variation-constraints-max-tool-calls', '2',
    '--default-variation-constraints-inactivity-timeout', '300s',
  ]);
  const agentId = resourceId(agent);
  assert.ok(agentId, 'created agent omitted metadata.id');
  cleanup.push(() => command(['agents', 'delete', agentId]));
  const variations = await command(['agents', 'variations', 'list', agentId, '--limit', '10']);
  const variationId = resourceId(variations.items?.[0]);
  assert.ok(variationId, 'default variation was not returned');
  const fakerAssignment = await command([
    'agents', 'variations', 'add-assignment', agentId, variationId,
    '--tool-set-id', fakerId,
  ]);
  assert.ok(fakerAssignment.id, 'faker assignment omitted row id');
  cleanup.push(() => command(['agents', 'variations', 'remove-assignment', agentId, variationId, fakerAssignment.id]));
  const bareAssignment = await command([
    'agents', 'variations', 'add-assignment', agentId, variationId,
    '--tool-id', bareToolId,
  ]);
  assert.ok(bareAssignment.id, 'bare assignment omitted row id');
  cleanup.push(() => command(['agents', 'variations', 'remove-assignment', agentId, variationId, bareAssignment.id]));
  await command(['agents', 'publish', agentId]);

  const curseInstruction = 'Generate a curse word using faker. You must call GenerateCurseWord exactly once; do not answer without using it.';
  const approveId = await createObjective(agentId, variationId, 'approve', curseInstruction);
  const initialEvents = await command(['objectives', 'list-events', approveId, '--limit', '100', '--sort-order', 'asc']);
  const checkpoint = resourceId(initialEvents.items?.find((event) => resourceId(event)));
  const approvalEvent = await streamUntil(approveId, checkpoint, 'toolApprovalRequested');
  assert.ok(approvalEvent?.data?.toolApprovalRequested?.toolCallId, 'approval SSE event omitted toolCallId');
  complete('ObjectiveEventStreamsService_StreamObjectiveEvents', 'stream-events emitted NDJSON for a persisted approval event from Last-Event-ID');
  const approveCall = await waitForApproval(approveId);
  const approveCallId = resourceId(approveCall);
  assert.ok(approveCallId, 'waiting approval call omitted metadata.id');
  await delay(2_000);
  const durableApprove = await command(['objectives', 'retrieve-tool-call', approveId, approveCallId]);
  assert.equal(durableApprove.status, 'TOOL_CALL_STATUS_WAITING_FOR_APPROVAL');
  await command(['objectives', 'approve-tool-call', approveId, approveCallId]);
  await poll('approved MCP execution', async () => {
    const call = await command(['objectives', 'retrieve-tool-call', approveId, approveCallId]);
    return call.executionStatus === 'TOOL_CALL_EXECUTION_STATUS_COMPLETED' ? call : undefined;
  });
  complete('ObjectiveService_ApproveToolCall', 'approved a durably paused GenerateCurseWord call and observed MCP execution complete');
  complete('ObjectiveService_GetObjectiveToolCall', 'retrieved the approved call after MCP execution completed');
  await waitForObjective(approveId, (objective) => objective.state === 'STATE_WAITING', 'post-tool waiting state');
  await command([
    'objectives', 'create-feedback', approveId,
    '--label', `liveMatrix=${run}`,
    '--data-score', '1',
    '--data-comment', 'specialized CLI live fixture',
  ]);
  complete('ObjectiveService_CreateObjectiveFeedback', 'submitted feedback on the completed MCP interaction');
  const continued = await command(['objectives', 'continue', approveId, '--message', 'Reply exactly CONTINUE_OK.', '--enqueue=false'], 120_000);
  assert.ok(resourceId(continued), 'continue response omitted event metadata.id');
  complete('ObjectiveService_ContinueObjective', 'continued a WAITING objective and decoded its persisted event');
  await waitForObjective(approveId, (objective) => objective.state === 'STATE_WAITING', 'continued waiting state');
  try {
    const compacted = await command([
      'objectives', 'compact', approveId,
      '--compaction-config-summarization-instructions', 'Summarize the integration-test conversation accurately.',
    ], 120_000);
    assert.ok(compacted && typeof compacted === 'object');
    complete('ObjectiveService_CompactObjective', 'compacted the continued MCP objective and decoded the response');
  } catch (error) {
    const diagnostic = String(error?.stderr ?? '');
    const match = diagnostic.match(/\b(4\d\d)\b/);
    if (match) block('ObjectiveService_CompactObjective', `owned objective did not satisfy server compaction prerequisites (HTTP ${match[1]})`);
    else throw error;
  }

  const denyId = await createObjective(agentId, variationId, 'deny', curseInstruction);
  const denyCall = await waitForApproval(denyId);
  const denyCallId = resourceId(denyCall);
  assert.ok(denyCallId, 'deny call omitted metadata.id');
  await command(['objectives', 'deny-tool-call', denyId, denyCallId, '--memo', 'specialized denial path']);
  await poll('denied call persistence', async () => {
    const call = await command(['objectives', 'retrieve-tool-call', denyId, denyCallId]);
    return call.status === 'TOOL_CALL_STATUS_DENIED' ? call : undefined;
  });
  complete('ObjectiveService_DenyToolCall', 'denied an independent durably paused GenerateCurseWord call');

  const contentId = await createObjective(
    agentId,
    variationId,
    'content',
    `Call the tool named ${run}-provide-content exactly once. Do not answer without calling it.`,
  );
  const contentCall = await waitForApproval(contentId);
  const contentCallId = resourceId(contentCall);
  assert.ok(contentCallId, 'content call omitted metadata.id');
  await command(['objectives', 'approve-tool-call', contentId, contentCallId]);
  await poll('bare tool waiting for content', async () => {
    const call = await command(['objectives', 'retrieve-tool-call', contentId, contentCallId]);
    return call.executionStatus === 'TOOL_CALL_EXECUTION_STATUS_WAITING_FOR_CONTENT' ? call : undefined;
  });
  const contentResult = await command([
    'objectives', 'set-tool-call-content', contentId, contentCallId,
    '--content', json({ type: 'text', text: { text: 'BARE_CONTENT_OK' } }),
  ]);
  assert.equal(resourceId(contentResult), contentCallId);
  complete('ObjectiveService_SetToolCallContent', 'supplied content to an independently approved bare tool call');

  const cancelId = await createObjective(
    agentId,
    variationId,
    'cancel',
    'Call GenerateCurseWord exactly once now and wait for approval.',
  );
  await waitForObjective(cancelId, (objective) => objective.state === 'STATE_RUNNING', 'cancel objective running state');
  await command(['objectives', 'cancel', cancelId, '--reason', 'specialized cancel path']);
  await waitForObjective(cancelId, (objective) => objective.state === 'STATE_CANCELLED', 'cancelled objective state');
  complete('ObjectiveService_CancelObjective', 'cancelled a separate objective observed in RUNNING state');
} catch (error) {
  fatal = error;
  for (const operationId of [
    'ToolService_GetToolSetOpenAPISpec',
    'ObjectiveService_CreateObjective',
    'ObjectiveEventStreamsService_StreamObjectiveEvents',
    'ObjectiveService_CreateObjectiveFeedback',
    'ObjectiveService_ApproveToolCall',
    'ObjectiveService_DenyToolCall',
    'ObjectiveService_SetToolCallContent',
    'ObjectiveService_CancelObjective',
    'ObjectiveService_CompactObjective',
    'ObjectiveService_ContinueObjective',
  ]) fail(operationId, error);
} finally {
  let cleanupFailures = 0;
  for (const dispose of cleanup.reverse()) {
    try { await dispose(); } catch { cleanupFailures += 1; }
  }
  writeReport(resultPath, report);
  console.log(JSON.stringify({ ...counts(report), cleanupFailures }));
}

if (fatal) {
  console.error(`specialized CLI harness failed: ${safeFailure(fatal)}`);
  process.exitCode = 1;
}
