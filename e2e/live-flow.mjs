// OPT-IN live acceptance flow against the real Cadenya API — the composed
// scenario from the 2026-08-13 live review: bare tool sets + tools, an agent
// with a nested variation, BOTH assignment kinds through the whole-body
// discriminated union, an objective streamed over SSE, tool results supplied
// through the content-block union, and an exact LIVE_FLOW_OK reply.
//
// This MUTATES the workspace (labeled resources, cleaned up in finally;
// objectives leave history because no delete endpoint exists). It therefore
// runs only when explicitly requested:
//
//   source .env.development && CADENYA_LIVE_FLOW=1 node e2e/live-flow.mjs
//
// Known contract quirks honored (tmp/api-contract-feedback.txt): the model
// must be referenced by its canonical `model_…` id, and temperature is not
// sent (rejected by some models).

import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

// Convenience: load `.env.development` (export KEY=value lines) for any
// variable not already set, so the script runs without a manual `source`.
try {
  const envFile = readFileSync(new URL('../.env.development', import.meta.url), 'utf8');
  for (const line of envFile.split('\n')) {
    const m = line.match(/^\s*(?:export\s+)?([A-Z0-9_]+)=(.*)$/);
    if (m && process.env[m[1]] === undefined) {
      process.env[m[1]] = m[2].trim().replace(/^["']|["']$/g, '');
    }
  }
} catch {
  // No env file — rely on the ambient environment.
}

if (process.env.CADENYA_LIVE_FLOW !== '1') {
  console.log('live-flow: refusing to run without CADENYA_LIVE_FLOW=1 (mutates the workspace)');
  process.exit(0);
}
if (!process.env.CADENYA_API_KEY || !process.env.CADENYA_WORKSPACE_ID) {
  console.error('missing CADENYA_API_KEY / CADENYA_WORKSPACE_ID');
  process.exit(1);
}

const distPath = new URL('../gen/typescript/dist/index.js', import.meta.url).pathname;
const { default: Cadenya } = await import(distPath);

const client = new Cadenya();
const RUN = `redwood-live-flow-${Date.now().toString(36)}`;

// Whole-flow deadline: ONE AbortSignal is passed to EVERY SDK call (not
// just the SSE read), so when the watchdog fires, whichever request is in
// flight rejects and run() itself settles — cleanup never overlaps live
// work and there is no orphaned racing promise. Caveat that no client can
// remove: aborting mid-create can leave a server-side resource whose id we
// never received; all resources carry the unique RUN prefix so a manual
// sweep can find them.
const DEADLINE_MS = 180_000;
const controller = new AbortController();
const watchdog = setTimeout(() => controller.abort(), DEADLINE_MS);
const OPTS = { signal: controller.signal };

// Everything created, tracked for unconditional cleanup (reverse order).
// Cleanup fns receive their OWN request options (a separate bounded signal)
// so a tripped flow deadline doesn't also doom the cleanup calls.
const cleanup = [];
const created = (label, fn) => {
  cleanup.push({ label, fn });
};

const step = (label, detail = '') => console.log(`${label.padEnd(20)} ${detail}`);

async function run() {
  // 1. A canonical model id (`model_…`) — the only accepted reference form.
  const models = await client.models.list({ limit: 50 }, OPTS);
  const model = models.items.find((m) => m.metadata?.id);
  assert.ok(model, 'no model available in the workspace');
  step('model', model.metadata.id.slice(0, 18) + '…');

  // 2. Two bare tool sets, one bare tool in each.
  const mkToolSet = async (suffix) => {
    const toolSet = await client.toolSets.create({
      metadata: { name: `${RUN}-set-${suffix}` },
      spec: { description: 'live-flow bare set', adapter: { type: 'bare', bare: {} } },
    }, OPTS);
    const id = toolSet.metadata.id;
    created(`toolSet ${suffix}`, (opts) => client.toolSets.delete(id, undefined, opts));
    return id;
  };
  const mkTool = async (toolSetId, name) => {
    const tool = await client.toolSets.tools.create({
      toolSetId,
      metadata: { name },
      spec: {
        description: `live-flow tool ${name}: echoes its argument`,
        requiresApproval: false,
        parameters: {
          type: 'object',
          properties: { value: { type: 'string' } },
          required: ['value'],
        },
        config: { type: 'bare', bare: {} },
      },
    }, OPTS);
    created(`tool ${name}`, (opts) => client.toolSets.tools.delete(tool.metadata.id, { toolSetId }, opts));
    return tool.metadata.id;
  };
  const setA = await mkToolSet('a');
  const setB = await mkToolSet('b');
  const alphaTool = await mkTool(setA, `${RUN}-alpha`);
  await mkTool(setB, `${RUN}-beta`);
  step('tool sets', 'two bare sets, one tool each');

  // 3. Agent + nested variation.
  const agent = await client.agents.create({
    metadata: { name: `${RUN}-agent` },
    spec: { variationSelectionMode: 'VARIATION_SELECTION_MODE_UNSPECIFIED' },
    defaultVariation: {
      metadata: { name: `${RUN}-v1` },
      spec: {
        systemPromptTemplate:
          'You are a test harness. Follow the user instruction exactly.',
        modelConfig: { modelId: model.metadata.id },
      },
    },
  }, OPTS);
  const agentId = agent.metadata.id;
  created('agent', (opts) => client.agents.delete(agentId, undefined, opts));
  const variations = await client.agents.variations.list({ agentId, limit: 1 }, OPTS);
  const variationId = variations.items[0].metadata.id;
  step('agent', `${agentId.slice(0, 14)}… variation ${variationId.slice(0, 14)}…`);

  // 4. Both assignment kinds through the whole-body discriminated union.
  await client.agents.variations.addAssignment({
    agentId,
    variationId,
    body: { type: 'toolId', toolId: alphaTool },
  }, OPTS);
  await client.agents.variations.addAssignment({
    agentId,
    variationId,
    body: { type: 'toolSetId', toolSetId: setB },
  }, OPTS);
  step('assignments', 'individual tool + whole tool set (whole-body union)');

  // 5. Publish, then an objective pinned to the variation.
  await client.agents.publish(agentId, undefined, OPTS);
  const objective = await client.objectives.create({
    agentId,
    variationId,
    systemPromptData: {},
    firstUserMessage:
      `Call the tool named ${RUN}-alpha with value "alpha" and the tool named ` +
      `${RUN}-beta with value "beta". After both results arrive, reply with ` +
      'exactly LIVE_FLOW_OK and nothing else.',
  }, OPTS);
  const objectiveId = objective.metadata.id;
  created('objective (cancel)', (opts) =>
    client.objectives.cancel(objectiveId, undefined, opts).catch(() => {}),
  );
  step('objective', objectiveId.slice(0, 14) + '…');

  // 6-8. Stream: answer each toolCalled, expect toolResults + LIVE_FLOW_OK.
  const stream = await client.objectives.streamEvents(objectiveId, undefined, OPTS);
  let toolCalls = 0;
  let toolResults = 0;
  let finalReply = null;
  for await (const event of stream) {
    const data = event.data;
    if (data?.type === 'toolCalled') {
      toolCalls++;
      const call = data.toolCalled;
      step('toolCalled', `${call.tool?.tool?.name ?? '?'} (#${toolCalls})`);
      await client.objectives.setToolCallContent(objectiveId, {
        toolCallId: call.toolCallId,
        content: [{ type: 'text', text: { text: `echo ok #${toolCalls}` } }],
      }, OPTS);
    } else if (data?.type === 'toolResult') {
      toolResults++;
      step('toolResult', `#${toolResults}`);
    } else if (data?.type === 'assistantMessage') {
      const content = data.assistantMessage?.content ?? '';
      if (content.trim()) {
        finalReply = content.trim();
        step('assistant', JSON.stringify(finalReply.slice(0, 40)));
        if (finalReply === 'LIVE_FLOW_OK') break;
      }
    } else if (data?.type === 'error') {
      throw new Error(`objective error event: ${JSON.stringify(data).slice(0, 200)}`);
    }
  }

  assert.equal(toolCalls, 2, `expected 2 tool calls, saw ${toolCalls}`);
  assert.equal(toolResults, 2, `expected 2 tool results, saw ${toolResults}`);
  assert.equal(finalReply, 'LIVE_FLOW_OK');

  // 9. Both persisted tool-call records completed.
  const calls = await client.objectives.listToolCalls(objectiveId, { limit: 10 }, OPTS);
  const completed = calls.items.filter(
    (c) => c.executionStatus === 'TOOL_CALL_EXECUTION_STATUS_COMPLETED',
  );
  assert.equal(completed.length, 2, 'expected 2 completed tool-call records');
  step('tool-call records', 'both TOOL_CALL_EXECUTION_STATUS_COMPLETED');
}

let failure = null;
try {
  // No Promise.race: every SDK call carries the abort signal, so run()
  // itself settles when the watchdog fires — cleanup starts only after all
  // flow work has stopped, and no create can land after its snapshot.
  await run();
} catch (err) {
  failure = controller.signal.aborted
    ? new Error(`whole-flow deadline of ${DEADLINE_MS / 1000}s exceeded (${err.message ?? err})`)
    : err;
} finally {
  clearTimeout(watchdog);
  // Unconditional, reverse-order cleanup under its OWN bounded signal (the
  // flow signal may already be aborted). Failures never mask the primary
  // error, but leaked live resources must not exit 0 either.
  const cleanupController = new AbortController();
  const cleanupWatchdog = setTimeout(() => cleanupController.abort(), 60_000);
  let cleanupFailures = 0;
  for (const { label, fn } of cleanup.reverse()) {
    try {
      await fn({ signal: cleanupController.signal });
      step('cleanup', label);
    } catch (err) {
      cleanupFailures++;
      step('cleanup FAILED', `${label}: ${String(err.message ?? err).slice(0, 80)}`);
    }
  }
  clearTimeout(cleanupWatchdog);
  if (failure) {
    console.error(`\nlive flow acceptance FAILED: ${failure.message ?? failure}`);
    process.exitCode = 1;
  } else if (cleanupFailures > 0) {
    console.error(`\nlive flow passed but ${cleanupFailures} cleanup step(s) failed — resources may be leaked`);
    process.exitCode = 1;
  } else {
    console.log('\nlive flow acceptance PASSED');
  }
}
