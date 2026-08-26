// Durable Gate-3 regression: TypeScript runtime wire projection. Passing
// OUTPUT-shaped values (carrying server-owned readOnly fields — the
// fetched-modify-resubmit pattern, or plain JavaScript callers) through the
// public built client must strip readOnly keys from the actual JSON without
// mutating the caller's objects. Runs against gen/typescript/dist.
// Run: node e2e/ts-directional-wire.mjs
import { createServer } from 'node:http';

const { default: Cadenya } = await import('../gen/typescript/dist/index.js');

let captured = null;
const server = createServer((req, res) => {
  let body = '';
  req.on('data', (d) => { body += d; });
  req.on('end', () => {
    captured = body ? JSON.parse(body) : null;
    // 400 is not retryable — the probe only needs the outbound body.
    res.statusCode = 400;
    res.setHeader('content-type', 'application/json');
    res.end('{"code":3,"message":"probe"}');
  });
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const baseURL = `http://127.0.0.1:${server.address().port}`;
const client = new Cadenya({ apiKey: 'probe', workspaceId: 'w', baseURL });

const failures = [];
async function capture(fn) {
  captured = null;
  try {
    await fn();
  } catch {
    // expected: the probe server answers 400
  }
  return captured;
}

// --- 1) Closed struct: output-shaped APIKeySpec (the schema that exposed
// the hole). token/system are readOnly; description/permissions are input.
const outputShapedSpec = {
  description: 'ci key',
  permissions: ['read'],
  token: 'sk-SECRET-SERVER-OWNED',
  system: true,
};
const before = JSON.stringify(outputShapedSpec);
{
  const body = await capture(() => client.apiKeys.create({
    metadata: { name: 'k' },
    spec: outputShapedSpec,
  }));
  const spec = body?.spec ?? {};
  if (spec.token !== undefined || spec.system !== undefined) {
    failures.push(`apiKeys.create leaked readOnly keys: ${JSON.stringify(spec)}`);
  }
  if (spec.description !== 'ci key' || JSON.stringify(spec.permissions) !== '["read"]') {
    failures.push(`apiKeys.create dropped input fields: ${JSON.stringify(spec)}`);
  }
}
if (JSON.stringify(outputShapedSpec) !== before) {
  failures.push('caller object was mutated by the wire projection');
}

// --- 2) Nested struct: MemoryLayerSpec carries four readOnly fields.
{
  const body = await capture(() => client.memoryLayers.create({
    metadata: { name: 'm' },
    spec: {
      type: 'TYPE_WORKING',
      description: 'probe layer',
      systemManaged: true,
      expiresAt: '2026-01-01T00:00:00Z',
      agentId: 'agent_x',
      episodicKey: 'k',
    },
  }));
  const spec = body?.spec ?? {};
  for (const key of ['systemManaged', 'expiresAt', 'agentId', 'episodicKey']) {
    if (spec[key] !== undefined) failures.push(`memoryLayers.create leaked readOnly ${key}`);
  }
  if (spec.type !== 'TYPE_WORKING' || spec.description !== 'probe layer') failures.push(`memoryLayers.create dropped input fields: ${JSON.stringify(spec)}`);
}

// --- 3) Union whole body: assignment variants carry readOnly
// workspaceId/agentId/variationId alongside the input ID field.
{
  const body = await capture(() => client.agents.variations.addAssignment('agent_1', 'variation_1', {
    body: {
      type: 'toolId',
      toolId: 'tool_9',
      workspaceId: 'w_leak',
      agentId: 'a_leak',
      variationId: 'v_leak',
    },
  }));
  const flat = JSON.stringify(body ?? {});
  if (flat.includes('w_leak') || flat.includes('a_leak') || flat.includes('v_leak')) {
    failures.push(`addAssignment leaked readOnly union fields: ${flat}`);
  }
  if (!flat.includes('tool_9')) failures.push(`addAssignment dropped the input toolId: ${flat}`);
}

server.close();
if (failures.length) {
  console.log(failures.join('\n'));
  console.error('ts directional wire gate: FAILED');
  process.exit(1);
}
console.log('ts directional wire gate: all cases passed');
