// Public generated clients: verify the JSON body and transport parameters separately.
import assert from 'node:assert/strict';
import UnionFixture from '../gen/fixtures/typescript-union-body/dist/index.js';

const requests = [];
const fetch = async (input, init) => {
  requests.push({ url: new URL(input), method: init.method, body: JSON.parse(init.body) });
  return new Response(null, { status: 204 });
};
const client = new UnionFixture({ workspaceId: 'ws_default', fetch });
for (const method of ['add', 'remove']) {
  for (const variant of [
    { type: 'toolId', toolId: 'tool_789' },
    { type: 'toolSetId', toolSetId: 'toolset_789', scope: 'all' },
  ]) {
    await client.assignment[method](variant);
    assert.equal(requests.at(-1).url.pathname, '/v1/workspaces/ws_default/assignments');
    assert.deepEqual(requests.at(-1).body, variant);
    const params = Object.freeze({ ...variant, workspaceId: 'ws/override', filters: { mode: 'active' }, dryRun: false });
    await client.assignment[method](params);
    const request = requests.at(-1);
    assert.equal(request.method, method === 'add' ? 'POST' : 'DELETE');
    assert.equal(request.url.pathname, '/v1/workspaces/ws%2Foverride/assignments');
    assert.equal(request.url.searchParams.get('filters.mode'), 'active');
    assert.equal(request.url.searchParams.get('dryRun'), 'false');
    assert.deepEqual(request.body, variant);
    assert.equal(params.workspaceId, 'ws/override');
  }
}
await client.submission.submit({ type: 'toolId', toolId: 'tool_789' });
assert.deepEqual(requests.at(-1).body, { type: 'toolId', toolId: 'tool_789' });
await client.batch.submit({ body: ['one', 'two'] });
assert.deepEqual(requests.at(-1).body, ['one', 'two']);

const nestedClient = new UnionFixture({ workspaceId: 'ws_default', fetch: async (input, init) => {
  await fetch(input, init);
  return Response.json({ id: 'assignment_123' });
} });
for (const field of ['toolId', 'toolSetId', 'subAgentId']) {
  for (const workspaceId of [undefined, 'ws_override']) {
    const params = Object.freeze({ type: field, [field]: 'target_789', workspaceId, agentId: 'a_leak', variationId: 'v_leak' });
    await nestedClient.agentVariations.addAssignment('agent_123', 'agentvar_456', params);
    assert.equal(requests.at(-1).url.pathname, `/v1/workspaces/${workspaceId ?? 'ws_default'}/agents/agent_123/variations/agentvar_456/assignments`);
    assert.deepEqual(requests.at(-1).body, { type: field, [field]: 'target_789' });
    assert.equal(params.agentId, 'a_leak');
  }
}
console.log('direct union bodies: variants, workspace defaults/overrides, query separation, readOnly filtering, and caller immutability passed');
