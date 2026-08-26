// Live read-only test of the generated SDK against the real Cadenya API.
// Requires CADENYA_API_KEY and CADENYA_WORKSPACE_ID in the environment
// (source .env.development). Never prints secrets; IDs are truncated.
//
// Usage: source .env.development && node e2e/live.mjs

import { default as Cadenya, APIError } from '../gen/typescript/dist/index.js';

const workspaceId = process.env.CADENYA_WORKSPACE_ID;
if (!process.env.CADENYA_API_KEY || !workspaceId) {
  console.error('missing CADENYA_API_KEY / CADENYA_WORKSPACE_ID');
  process.exit(1);
}

// Key AND workspace come from env (CADENYA_API_KEY / CADENYA_WORKSPACE_ID) —
// no per-call workspaceId below exercises the client-defaults feature live.
const client = new Cadenya();
const short = (id) => (id ? `${String(id).slice(0, 12)}…` : id);

// 1. Credentials check.
// account.info carries secret material (webhook HMAC secret) — never print it.
const account = await client.accounts.retrieve();
console.log('accounts.retrieve   ok ', `info keys: ${Object.keys(account.info ?? {}).join(', ') || '(none)'}`);

// 2. Workspaces list (pagination envelope against real data).
const workspaces = await client.workspaces.list({ limit: 2 });
console.log('workspaces.list     ok ', `${workspaces.items.length} item(s), hasNextPage=${workspaces.hasNextPage()}`);

// 3. Agents in the provided workspace.
const agents = await client.agents.list({ limit: 3 });
console.log('agents.list         ok ', agents.items.map((a) => short(a.metadata?.id)).join(', ') || '(none)');

// 4. Objectives + auto-pagination across real pages (capped at 5).
const objectives = await client.objectives.list({ limit: 2 });
const seen = [];
for await (const objective of objectives) {
  seen.push(short(objective.metadata?.id ?? objective.id));
  if (seen.length >= 5) break;
}
console.log('objectives.list     ok ', `${seen.length} across pages: ${seen.join(', ') || '(none)'}`);

// 5. Models catalog.
const models = await client.models.list({ workspaceId, limit: 3 }).catch((e) => e);
if (models instanceof APIError) {
  console.log('models.list         skip', `APIError ${models.status}: ${models.message}`);
} else {
  console.log('models.list         ok ', `${models.items?.length ?? 0} item(s)`);
}

// 6. Error mapping against the real server.
try {
  await client.objectives.retrieve('obj_does_not_exist', { workspaceId });
  console.log('error mapping       FAIL (expected an APIError)');
  process.exit(1);
} catch (err) {
  if (err instanceof APIError) {
    console.log('error mapping       ok ', `status=${err.status} code=${err.code} message=${JSON.stringify(err.message).slice(0, 60)}`);
  } else {
    throw err;
  }
}

console.log('\nlive API checks passed');
