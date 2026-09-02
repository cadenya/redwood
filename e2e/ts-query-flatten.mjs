// Generated TypeScript resource regression: dotted OpenAPI query parameters
// become nested SDK inputs, reach the transport, and flatten back to the
// original dotted wire names.

import assert from 'node:assert/strict';

const sdkPath = process.argv[2]
  ?? new URL('../gen/fixtures/typescript-dotted-query/dist/index.js', import.meta.url).pathname;
const { default: QueryFixture } = await import(sdkPath);

let requestedURL;
let rejectNext = false;
const client = new QueryFixture({
  baseURL: 'https://api.example.test',
  fetch: async (input) => {
    requestedURL = new URL(input);
    if (rejectNext) {
      return new Response('{"code":13,"message":"service unavailable"}', {
        status: 500,
        headers: { 'content-type': 'application/json' },
      });
    }
    return new Response('{"items":[{"timestamp":"2026-09-01T00:00:00Z","value":3}]}', {
      status: 200,
      headers: { 'content-type': 'application/json' },
    });
  },
});

const params = {
  range: {
    start: '2026-09-01T00:00:00Z',
    end: '2026-09-02T00:00:00Z',
  },
  filters: {
    resourceId: 'resource_123',
    states: ['running', 'completed'],
  },
  interval: 'REPORT_INTERVAL_DAY',
  groupBy: 'REPORT_GROUP_BY_STATE',
};
const result = await client.report.querySummary(params);

assert.equal(requestedURL.pathname, '/v1/reports/summary');
assert.equal(requestedURL.searchParams.get('range.start'), '2026-09-01T00:00:00Z');
assert.equal(requestedURL.searchParams.get('range.end'), '2026-09-02T00:00:00Z');
assert.equal(requestedURL.searchParams.get('filters.resourceId'), 'resource_123');
assert.deepEqual(requestedURL.searchParams.getAll('filters.states'), ['running', 'completed']);
assert.equal(requestedURL.searchParams.get('interval'), 'REPORT_INTERVAL_DAY');
assert.equal(requestedURL.searchParams.get('groupBy'), 'REPORT_GROUP_BY_STATE');
assert.equal(requestedURL.searchParams.has('range'), false);
assert.deepEqual(result.items, [{ timestamp: '2026-09-01T00:00:00Z', value: 3 }]);

// Failed requests remain rejections; the SDK must not turn them into an
// empty successful response.
rejectNext = true;
await assert.rejects(
  () => client.report.querySummary(params),
  (err) => err.name === 'APIError'
    && err.status === 500
    && err.message === 'service unavailable',
);

console.log('nested GET query flattening + rejection propagation ok');
