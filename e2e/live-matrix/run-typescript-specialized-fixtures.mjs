// Opt-in end-to-end acceptance flow for the generated TypeScript SDK.
// Uses uniquely owned fixtures, persists only status evidence, and cleans up.

import assert from 'node:assert/strict';
import {
  counts,
  loadLiveEnvironment,
  readReport,
  resourceId,
  safeFailure,
  writeReport,
} from './common-node.mjs';

if (process.env.CADENYA_LIVE_SPECIALIZED_FIXTURES !== 'typescript') {
  console.error('refusing specialized sweep: set CADENYA_LIVE_SPECIALIZED_FIXTURES=typescript');
  process.exit(2);
}

loadLiveEnvironment();
const { default: Cadenya } = await import('../../gen/typescript/dist/index.js');
const client = new Cadenya();
const resultPath = new URL('./results-typescript.json', import.meta.url);
const report = readReport(resultPath, 'typescript');
const operations = report.operations;
const run = `specialized-ts-${Date.now().toString(36)}`;
const cleanup = [];
const requestOptions = (milliseconds = 30_000) => ({ signal: AbortSignal.timeout(milliseconds) });
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

function complete(operationId, detail) {
  operations[operationId] = {
    status: 'completed',
    evidence: `real api.cadenya.com: generated TypeScript specialized fixture succeeded; ${detail}; no response body or resource ID persisted`,
  };
}

function block(operationId, detail) {
  if (operations[operationId]?.status === 'completed') return;
  operations[operationId] = {
    status: 'blocked',
    evidence: `real api.cadenya.com: generated TypeScript specialized fixture prerequisite; ${detail}`,
  };
}

function fail(operationId, error) {
  if (operations[operationId]?.status === 'completed') return;
  operations[operationId] = {
    status: 'failed',
    evidence: `real generated TypeScript specialized fixture failed: ${safeFailure(error)}; no response body or resource ID persisted`,
  };
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
  const value = await client.toolSets.create({
    metadata: { name: `${run}-${suffix}`, labels: { live_matrix: run } },
    spec: { description: 'specialized live matrix fixture', adapter },
  }, requestOptions());
  const id = resourceId(value);
  assert.ok(id, 'created tool set omitted metadata.id');
  cleanup.push(async () => {
    try { await client.toolSets.archive(id, undefined, requestOptions()); } catch {}
    await client.toolSets.delete(id, undefined, requestOptions());
  });
  return id;
}

async function createObjective(agentId, variationId, suffix, firstUserMessage) {
  const value = await client.objectives.create({
    agentId,
    variationId,
    metadata: { labels: { live_matrix: run, case: suffix } },
    systemPromptData: {},
    firstUserMessage,
  }, requestOptions());
  const id = resourceId(value);
  assert.ok(id, 'created objective omitted metadata.id');
  cleanup.push(async () => {
    try { await client.objectives.cancel(id, { reason: 'specialized fixture cleanup' }, requestOptions()); } catch {}
  });
  complete('ObjectiveService_CreateObjective', 'created and decoded independent owned objectives');
  return id;
}

async function waitForApproval(objectiveId) {
  return poll('tool approval request', async () => {
    const page = await client.objectives.listToolCalls(objectiveId, { limit: 20 }, requestOptions());
    return page.items.find((item) => item.status === 'TOOL_CALL_STATUS_WAITING_FOR_APPROVAL');
  });
}

async function waitForObjective(objectiveId, predicate, label) {
  return poll(label, async () => {
    const objective = await client.objectives.retrieve(objectiveId, undefined, requestOptions());
    return predicate(objective) ? objective : undefined;
  });
}

async function replayOneEvent(objectiveId, lastEventId, expectedType, milliseconds = 120_000) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), milliseconds);
  let stream;
  try {
    stream = await client.objectives.streamEvents(
      objectiveId,
      undefined,
      { signal: controller.signal, lastEventId },
    );
    for await (const envelope of stream.events()) {
      const event = envelope.data;
      if (!expectedType || event?.data?.type === expectedType) return event;
    }
    throw new Error('SSE ended before expected persisted event');
  } finally {
    clearTimeout(timer);
    await stream?.close();
  }
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
    const response = await client.toolSets.retrieveOpenApiSpec(petstoreId, undefined, requestOptions());
    if (!response.spec) return undefined;
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
    const page = await client.toolSets.tools.list(fakerId, { limit: 20 }, requestOptions());
    return page.items.length >= 3 ? page.items : undefined;
  });
  const byName = new Map(fakerTools.map((tool) => [tool.spec.llmToolName, tool]));
  assert.deepEqual(new Set(byName.keys()), new Set(['GenerateCurseWord', 'GenerateFake', 'GetFakerOptions']));
  assert.equal(byName.get('GenerateCurseWord')?.spec.requiresApproval, true);
  assert.equal(byName.get('GenerateFake')?.spec.requiresApproval, false);
  assert.equal(byName.get('GetFakerOptions')?.spec.requiresApproval, false);

  const bareId = await createToolSet('bare-content', { type: 'bare', bare: {} });
  const bareTool = await client.toolSets.tools.create(bareId, {
    metadata: { name: `${run}-provide-content` },
    spec: {
      description: 'Request externally supplied live-test content.',
      requiresApproval: true,
      parameters: { type: 'object', properties: {} },
      config: { type: 'bare', bare: {} },
    },
  }, requestOptions());
  const bareToolId = resourceId(bareTool);
  assert.ok(bareToolId, 'bare tool omitted metadata.id');
  cleanup.push(() => client.toolSets.tools.delete(bareId, bareToolId, undefined, requestOptions()));

  const models = await client.models.list({ limit: 50 }, requestOptions());
  const modelId = resourceId(models.items.find((item) => resourceId(item)));
  assert.ok(modelId, 'workspace has no readable model fixture');
  const agent = await client.agents.create({
    metadata: { name: `${run}-agent`, labels: { live_matrix: run } },
    spec: { variationSelectionMode: 'VARIATION_SELECTION_MODE_UNSPECIFIED' },
    defaultVariation: {
      metadata: { name: `${run}-variation`, labels: { live_matrix: run } },
      spec: {
        systemPromptTemplate: 'You are an integration-test agent. Follow explicit tool-use instructions.',
        modelConfig: { modelId },
        constraints: { maxToolCalls: 2, inactivityTimeout: '300s' },
      },
    },
  }, requestOptions());
  const agentId = resourceId(agent);
  assert.ok(agentId, 'created agent omitted metadata.id');
  cleanup.push(() => client.agents.delete(agentId, undefined, requestOptions()));
  const variations = await client.agents.variations.list(agentId, { limit: 10 }, requestOptions());
  const variationId = resourceId(variations.items[0]);
  assert.ok(variationId, 'default variation was not returned');
  const fakerAssignment = await client.agents.variations.addAssignment(
    agentId,
    variationId,
    { body: { type: 'toolSetId', toolSetId: fakerId } },
    requestOptions(),
  );
  assert.ok(fakerAssignment.id, 'faker assignment omitted row id');
  cleanup.push(() => client.agents.variations.removeAssignment(agentId, variationId, fakerAssignment.id, undefined, requestOptions()));
  const bareAssignment = await client.agents.variations.addAssignment(
    agentId,
    variationId,
    { body: { type: 'toolId', toolId: bareToolId } },
    requestOptions(),
  );
  assert.ok(bareAssignment.id, 'bare assignment omitted row id');
  cleanup.push(() => client.agents.variations.removeAssignment(agentId, variationId, bareAssignment.id, undefined, requestOptions()));
  await client.agents.publish(agentId, undefined, requestOptions());

  const curseInstruction = 'Generate a curse word using faker. You must call GenerateCurseWord exactly once; do not answer without using it.';
  const approveId = await createObjective(agentId, variationId, 'approve', curseInstruction);
  const initialEvents = await client.objectives.listEvents(approveId, { limit: 100, sortOrder: 'asc' }, requestOptions());
  const checkpoint = resourceId(initialEvents.items.find((event) => resourceId(event)));
  const approvalEvent = await replayOneEvent(approveId, checkpoint, 'toolApprovalRequested');
  assert.ok(approvalEvent?.data?.toolApprovalRequested?.toolCallId, 'approval SSE event omitted toolCallId');
  complete('ObjectiveEventStreamsService_StreamObjectiveEvents', 'SSE decoded a persisted approval event from a Last-Event-ID checkpoint');
  const approveCall = await waitForApproval(approveId);
  const approveCallId = resourceId(approveCall);
  assert.ok(approveCallId, 'waiting approval call omitted metadata.id');
  await delay(2_000);
  const durableApprove = await client.objectives.retrieveToolCall(approveId, approveCallId, undefined, requestOptions());
  assert.equal(durableApprove.status, 'TOOL_CALL_STATUS_WAITING_FOR_APPROVAL');
  await client.objectives.approveToolCall(approveId, approveCallId, undefined, requestOptions());
  await poll('approved MCP execution', async () => {
    const call = await client.objectives.retrieveToolCall(approveId, approveCallId, undefined, requestOptions());
    return call.executionStatus === 'TOOL_CALL_EXECUTION_STATUS_COMPLETED' ? call : undefined;
  });
  complete('ObjectiveService_ApproveToolCall', 'approved a durably paused GenerateCurseWord call and observed MCP execution complete');
  complete('ObjectiveService_GetObjectiveToolCall', 'retrieved the approved call after MCP execution completed');
  await waitForObjective(approveId, (objective) => objective.state === 'STATE_WAITING', 'post-tool waiting state');

  await client.objectives.createFeedback(approveId, {
    metadata: { labels: { live_matrix: run } },
    data: { score: 1, comment: 'specialized TypeScript live fixture' },
  }, requestOptions());
  complete('ObjectiveService_CreateObjectiveFeedback', 'submitted feedback on the completed MCP interaction');
  const continued = await client.objectives.continue(approveId, {
    message: 'Reply exactly CONTINUE_OK.',
    enqueue: false,
  }, requestOptions());
  assert.ok(resourceId(continued), 'continue response omitted event metadata.id');
  complete('ObjectiveService_ContinueObjective', 'continued a WAITING objective and decoded its persisted event');
  await waitForObjective(approveId, (objective) => objective.state === 'STATE_WAITING', 'continued waiting state');
  try {
    const compacted = await client.objectives.compact(approveId, {
      compactionConfig: { summarization: { instructions: 'Summarize the integration-test conversation accurately.' } },
    }, requestOptions(120_000));
    assert.ok(compacted && typeof compacted === 'object');
    complete('ObjectiveService_CompactObjective', 'compacted the continued MCP objective and decoded the response');
  } catch (error) {
    const status = Number(error?.status ?? error?.statusCode);
    if (status >= 400 && status < 500) block('ObjectiveService_CompactObjective', `owned objective did not satisfy server compaction prerequisites (HTTP ${status})`);
    else throw error;
  }

  const denyId = await createObjective(agentId, variationId, 'deny', curseInstruction);
  const denyCall = await waitForApproval(denyId);
  const denyCallId = resourceId(denyCall);
  assert.ok(denyCallId, 'deny call omitted metadata.id');
  await client.objectives.denyToolCall(denyId, denyCallId, { memo: 'specialized denial path' }, requestOptions());
  await poll('denied call persistence', async () => {
    const call = await client.objectives.retrieveToolCall(denyId, denyCallId, undefined, requestOptions());
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
  await client.objectives.approveToolCall(contentId, contentCallId, undefined, requestOptions());
  await poll('bare tool waiting for content', async () => {
    const call = await client.objectives.retrieveToolCall(contentId, contentCallId, undefined, requestOptions());
    return call.executionStatus === 'TOOL_CALL_EXECUTION_STATUS_WAITING_FOR_CONTENT' ? call : undefined;
  });
  const contentResult = await client.objectives.setToolCallContent(contentId, contentCallId, {
    content: [{ type: 'text', text: { text: 'BARE_CONTENT_OK' } }],
  }, requestOptions());
  assert.equal(resourceId(contentResult), contentCallId);
  complete('ObjectiveService_SetToolCallContent', 'supplied content to an independently approved bare tool call');

  const cancelId = await createObjective(
    agentId,
    variationId,
    'cancel',
    'Call GenerateCurseWord exactly once now and wait for approval.',
  );
  await waitForObjective(cancelId, (objective) => objective.state === 'STATE_RUNNING', 'cancel objective running state');
  await client.objectives.cancel(cancelId, { reason: 'specialized cancel path' }, requestOptions());
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
  console.error(`specialized TypeScript harness failed: ${safeFailure(fatal)}`);
  process.exitCode = 1;
}
