# Go specialized live fixture

This opt-in runner exercises the stateful adapter and objective flows that
cannot be proven by independent calls: Petstore OpenAPI ingestion, Faker MCP
execution with independent approval and denial objectives, a bare-tool content
objective, feedback, continuation, compaction, cancellation while running, and
SSE replay through `Last-Event-ID`.

It mutates the selected workspace, uses uniquely prefixed resources, performs
best-effort cleanup in reverse order, and merges evidence into
`../../results-go.json`. It never prints credentials, response bodies, or IDs.

Run only in the parent-controlled serialized live-test phase:

```sh
GO_LIVE_SPECIALIZED=1 \
GO_LIVE_ENV_PATH=/absolute/path/to/.env.development \
go run .
```
