// Recovery cleanup for interrupted TypeScript specialized runs. It touches
// only resources whose owned live-matrix marker starts with "specialized-ts-".

import { loadLiveEnvironment, resourceId } from './common-node.mjs';

if (process.env.CADENYA_LIVE_SPECIALIZED_CLEANUP !== 'typescript') {
  console.error('refusing cleanup: set CADENYA_LIVE_SPECIALIZED_CLEANUP=typescript');
  process.exit(2);
}

loadLiveEnvironment();
const { default: Cadenya } = await import('../../gen/typescript/dist/index.js');
const client = new Cadenya();
const options = () => ({ signal: AbortSignal.timeout(30_000) });
const owned = (value) => {
  const labels = value?.metadata?.labels ?? {};
  return Object.values(labels).some((label) => String(label).startsWith('specialized-ts-'))
    || String(value?.metadata?.name ?? '').startsWith('specialized-ts-');
};
let cleaned = 0;
let failed = 0;

const objectives = await client.objectives.list({ limit: 100, sortOrder: 'desc' }, options());
for (const objective of objectives.items.filter(owned)) {
  const id = resourceId(objective);
  if (!id) continue;
  try { await client.objectives.cancel(id, { reason: 'interrupted specialized fixture cleanup' }, options()); cleaned += 1; }
  catch { /* Terminal objectives need no cleanup. */ }
}

const agents = await client.agents.list({ limit: 100, prefix: 'specialized-ts-' }, options());
for (const agent of agents.items.filter(owned)) {
  const agentId = resourceId(agent);
  if (!agentId) continue;
  try {
    const variations = await client.agents.variations.list(agentId, { limit: 100, includeInfo: true }, options());
    for (const variation of variations.items) {
      const variationId = resourceId(variation);
      if (!variationId) continue;
      for (const assignment of variation.info?.assignments ?? []) {
        if (assignment.id) {
          try { await client.agents.variations.removeAssignment(agentId, variationId, assignment.id, undefined, options()); } catch {}
        }
      }
    }
    await client.agents.delete(agentId, undefined, options());
    cleaned += 1;
  } catch { failed += 1; }
}

const sets = await client.toolSets.list({ limit: 100, prefix: 'specialized-ts-' }, options());
for (const set of sets.items.filter(owned)) {
  const setId = resourceId(set);
  if (!setId) continue;
  try {
    const tools = await client.toolSets.tools.list(setId, { limit: 100 }, options());
    for (const tool of tools.items) {
      const toolId = resourceId(tool);
      if (toolId) {
        try { await client.toolSets.tools.delete(setId, toolId, undefined, options()); } catch {}
      }
    }
    try { await client.toolSets.archive(setId, undefined, options()); } catch {}
    await client.toolSets.delete(setId, undefined, options());
    cleaned += 1;
  } catch { failed += 1; }
}

console.log(JSON.stringify({ cleaned, failed }));
if (failed) process.exitCode = 1;
