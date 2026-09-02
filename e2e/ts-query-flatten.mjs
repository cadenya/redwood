// TypeScript transport regression: nested GET query objects use dotted field
// paths instead of being coerced to "[object Object]".

import assert from 'node:assert/strict';

const httpPath = process.argv[2]
  ?? new URL('../gen/typescript/dist/core/http.js', import.meta.url).pathname;
const { HttpClient } = await import(httpPath);

let requestedURL;
const client = new HttpClient({
  baseURL: 'https://api.example.test',
  authHeader: () => ({}),
  fetch: async (input) => {
    requestedURL = new URL(input);
    if (requestedURL.pathname.endsWith('/rejected')) {
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

const result = await client.request({
  method: 'GET',
  path: '/v1/reports/activity',
  query: {
    range: {
      start: '2026-09-01T00:00:00Z',
      end: '2026-09-02T00:00:00Z',
    },
    filters: {
      resourceId: 'resource_123',
      states: ['running', 'completed'],
      omitted: undefined,
    },
    includeEmpty: false,
  },
});

assert.equal(requestedURL.searchParams.get('range.start'), '2026-09-01T00:00:00Z');
assert.equal(requestedURL.searchParams.get('range.end'), '2026-09-02T00:00:00Z');
assert.equal(requestedURL.searchParams.get('filters.resourceId'), 'resource_123');
assert.deepEqual(requestedURL.searchParams.getAll('filters.states'), ['running', 'completed']);
assert.equal(requestedURL.searchParams.get('includeEmpty'), 'false');
assert.equal(requestedURL.searchParams.has('filters.omitted'), false);
assert.equal(requestedURL.searchParams.has('range'), false);
assert.deepEqual(result.items, [{ timestamp: '2026-09-01T00:00:00Z', value: 3 }]);

// Failed requests remain rejections; the SDK must not turn them into an
// empty successful response.
await assert.rejects(
  () => client.request({
    method: 'GET',
    path: '/v1/reports/rejected',
    query: { filters: { resourceId: 'resource_123' } },
  }),
  (err) => err.name === 'APIError'
    && err.status === 500
    && err.message === 'service unavailable',
);

console.log('nested GET query flattening + rejection propagation ok');
