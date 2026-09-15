import type {
  UnionFixture,
  AssignmentAddParams,
  AssignmentRemoveParams,
  AgentVariationAddAssignmentParams,
} from '../gen/fixtures/typescript-union-body/dist/index.js';

export function fixtureCalls(client: UnionFixture, params: AssignmentAddParams) {
  if (params.type === 'toolId') {
    params.toolId.toUpperCase();
    // @ts-expect-error The other variant's field is unavailable after narrowing.
    params.toolSetId;
  } else {
    params.toolSetId.toUpperCase();
    params.scope.toUpperCase();
  }
  client.assignment.add({ type: 'toolId', toolId: 'tool_789' });
  client.assignment.remove({ type: 'toolSetId', toolSetId: 'toolset_789', scope: 'all', workspaceId: 'ws_123' } satisfies AssignmentRemoveParams);
  client.submission.submit({ type: 'toolId', toolId: 'tool_789' });
  client.batch.submit({ body: ['one', 'two'] });
  // @ts-expect-error Each variant's payload remains required.
  client.assignment.add({ type: 'toolId' });
  // @ts-expect-error All required fields of multi-field variants remain required.
  client.assignment.remove({ type: 'toolSetId', toolSetId: 'toolset_789' });
  // @ts-expect-error A payload from another variant does not satisfy the selected arm.
  client.assignment.add({ type: 'toolId', toolSetId: 'toolset_789' });
  // @ts-expect-error The discriminator is required.
  client.assignment.add({ toolId: 'tool_789' });
  // @ts-expect-error Unknown tags are invalid.
  client.assignment.remove({ type: 'unknown', toolId: 'tool_789' });
  // @ts-expect-error Whole-body unions require params, even with a workspace default.
  client.assignment.add();
  // @ts-expect-error The old body wrapper is no longer the public input shape.
  client.assignment.add({ body: { type: 'toolId', toolId: 'tool_789' } });
}

export function nestedAssignmentCall(client: UnionFixture, params: AgentVariationAddAssignmentParams) {
  if (params.type === 'toolSetId') params.toolSetId.toUpperCase();
  return client.agentVariations.addAssignment('agent_123', 'agentvar_456', {
    type: 'toolSetId', toolSetId: 'toolset_789',
  });
}
