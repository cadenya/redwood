// Conformance mock: serves every operation in manifest.json, validating each
// request's method, path shape, required query params, and required body
// fields. Violations return 400 with a reason the driver records.

import { createServer } from 'node:http';

export function buildRoutes(manifest) {
  const routes = manifest.operations.map((op) => {
    const pattern = new RegExp(
      '^' +
        op.path
          .split(/(\{[^}]+\})/)
          .map((part) =>
            part.startsWith('{')
              ? '([^/]+)'
              : part.replace(/[.*+?^${}()|[\]\\]/g, '\\$&'),
          )
          .join('') +
        '$',
    );
    const literalLength = op.path.replace(/\{[^}]+\}/g, '').length;
    return { op, pattern, literalLength };
  });
  // Most-specific template first so /a/{id}:action beats /a/{id}.
  routes.sort((a, b) => b.literalLength - a.literalLength);
  return routes;
}

const PAGE2_CURSOR = 'conformance-page-2';

export function startMock(manifest) {
  const routes = buildRoutes(manifest);
  // Per-op record of first-page query params, so the second page can be
  // checked for parameter drift (e.g. a filter overwritten by the cursor
  // plumbing — a real bug class the single-page check missed).
  const firstPageParams = new Map();

  const server = createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    const route = routes.find(
      (r) => r.op.httpMethod === req.method && r.pattern.test(url.pathname),
    );
    if (!route) {
      res.statusCode = 404;
      res.end(JSON.stringify({ code: 5, message: `no route: ${req.method} ${url.pathname}` }));
      return;
    }
    const { op } = route;

    const fail = (reason) => {
      res.statusCode = 400;
      res.end(JSON.stringify({ code: 3, message: `[conformance] ${op.id}: ${reason}` }));
    };

    for (const q of op.queryParams.filter((q) => q.required)) {
      if (!url.searchParams.has(q.name)) return fail(`missing required query param ${q.name}`);
    }

    let body = '';
    req.on('data', (c) => (body += c));
    req.on('end', () => {
      if (op.bodyFields.length > 0) {
        let parsed;
        try {
          parsed = JSON.parse(body || '{}');
        } catch {
          return fail('body is not valid JSON');
        }
        for (const f of op.bodyFields.filter((f) => f.required)) {
          if (!(f.name in parsed)) return fail(`missing required body field ${f.name}`);
        }
      }
      if (op.wholeBody) {
        // The entire body is one (typically union-typed) value; require the
        // sample's top-level keys so a bodyless call cannot pass.
        let parsed;
        try {
          parsed = JSON.parse(body);
        } catch {
          return fail('whole-body request is missing or not valid JSON');
        }
        const sample = op.wholeBody.sample;
        if (sample && typeof sample === 'object' && !Array.isArray(sample)) {
          if (parsed === null || typeof parsed !== 'object') {
            return fail('whole-body request must be a JSON object');
          }
          for (const key of Object.keys(sample)) {
            if (!(key in parsed)) return fail(`missing whole-body key ${key}`);
          }
        }
      }

      if (op.response.kind === 'sse') {
        res.setHeader('content-type', 'text/event-stream');
        // Two events: streaming consumers must handle consecutive events
        // (and the CLI driver asserts one NDJSON line per event).
        res.write(`id: evt-1\ndata: ${JSON.stringify(op.response.sample)}\n\n`);
        res.write(`id: evt-2\ndata: ${JSON.stringify(op.response.sample)}\n\n`);
        res.end();
        return;
      }
      res.setHeader('content-type', 'application/json');
      if (op.response.kind === 'empty') {
        res.end('{}');
        return;
      }
      if (op.pagination) {
        const { cursorParam } = op.pagination;
        if (url.searchParams.get(cursorParam) === PAGE2_CURSOR) {
          // Second page: every non-cursor query param must be byte-identical
          // to the first request, or the SDK corrupted state between pages.
          const recorded = firstPageParams.get(op.id) ?? new Map();
          for (const key of new Set([...url.searchParams.keys(), ...recorded.keys()])) {
            if (key === cursorParam) continue;
            const got = url.searchParams.getAll(key).sort().join('\0');
            if (got !== (recorded.get(key) ?? '')) {
              return fail(`page-2 query param ${key} drifted: ${url.searchParams.getAll(key)}`);
            }
          }
          res.end(JSON.stringify(op.response.sample));
          return;
        }
        // First page: record params and hand back a next cursor.
        const recorded = new Map();
        for (const key of new Set([...url.searchParams.keys()])) {
          recorded.set(key, url.searchParams.getAll(key).sort().join('\0'));
        }
        firstPageParams.set(op.id, recorded);
        const body = structuredClone(op.response.sample);
        const segments = op.pagination.nextCursorPath.split('.');
        let target = body;
        for (const segment of segments.slice(0, -1)) {
          if (typeof target[segment] !== 'object' || target[segment] === null) {
            target[segment] = {};
          }
          target = target[segment];
        }
        target[segments.at(-1)] = PAGE2_CURSOR;
        res.end(JSON.stringify(body));
        return;
      }
      res.end(JSON.stringify(op.response.sample));
    });
  });

  return new Promise((resolve) => {
    server.listen(0, () =>
      resolve({ server, baseURL: `http://127.0.0.1:${server.address().port}` }),
    );
  });
}
