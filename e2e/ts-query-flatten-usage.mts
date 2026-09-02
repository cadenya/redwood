import type {
  QueryFixture,
  ReportQuerySummaryParams,
} from '../gen/fixtures/typescript-dotted-query/dist/index.js';

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
} satisfies ReportQuerySummaryParams;

export function querySummary(client: QueryFixture) {
  return client.report.querySummary(params);
}
