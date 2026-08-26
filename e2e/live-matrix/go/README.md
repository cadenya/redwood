# Go live-operation matrix

`snippets.json` is the stable, operation-ID-keyed catalog for the generated Go
SDK. It accounts for all operations in both `api-spec.yml` and the generated
manifest, and carries the exact generated SDK invocation, required fixtures,
safety gate, cleanup expectation, and qualifying live evidence.

Regenerate and verify it from the repository root:

```sh
node e2e/live-matrix/go/generate.mjs
node e2e/live-matrix/go/generate.mjs --check
```

Reproduce the real-API waves (the environment file is outside the implementation
worktree; values are never printed):

```sh
(set -a; source ../../../.env.development; set +a
 cd e2e/live-matrix/go/live-read && go run .)
(set -a; source ../../../.env.development; set +a
 cd e2e/live-matrix/go/live-mutate && go run .)
```

Both waves merge reviewed status-only evidence into
`e2e/live-matrix/results-go.json`. The read runner evaluates every GET. The
mutation runner creates uniquely labeled Go-owned fixtures, exercises their
lifecycle operations, and cleans them up where the contract provides deletion.
Account-global auth rotations and workspace administration stay in the
serialized cross-SDK final wave because they can invalidate concurrent lanes.

The generator fails if the OpenAPI spec, manifest, generated Go conformance
driver, and catalog do not describe the same operation set. The stable locator
for an operation is:

```text
e2e/live-matrix/go/snippets.json#/operations/<operationId>
```

## Evidence rule

`completed` means a checked-in program exercised the successful operation
against `api.cadenya.com` through the generated Go SDK and decoded or asserted
the response. Mock conformance, compilation, a 404/error-mapping request, a
call made by another language, or an unrecorded ad-hoc probe does not qualify.

Each live result should record at least the operation ID, timestamp, SDK commit,
fixture/run label, pass/fail status, and a non-secret assertion. Never persist
response bodies from account, API-key, provider-key, or secret endpoints.

## Executing templates

The snippets are reviewable call templates, not a blind production runner.
Their `"sample"` values come from the mock conformance generator and must be
replaced with contract-valid values and the cataloged fixture IDs. Put the
snippet in a small `func probe(ctx context.Context, client *sdk.Client) error`
with the catalog's `sharedHelpers`, and give it a bounded context.

Execution gates are deliberate:

- `safe-read`: may use an existing test fixture and must not print bodies.
- `manual-sensitive-read`: read-only, but an operator must prevent secret data
  from entering stdout, logs, or result artifacts.
- `owned-fixture-only`: may mutate or delete only a resource created and
  recorded by the current run; cleanup is registered before mutation.
- `workflow-fixture-only`: may create persistent objective/history records;
  use a unique run label and a whole-flow deadline.
- `manual-only`: credential rotation, key/secret mutation, workspace/member
  administration, model state changes, and broad tenant-session deletion need
  explicit operator approval. They are never part of an automatic production
  sweep.

SSE probes additionally `defer stream.Close()`, use a deadline, assert at least
one typed event, record `stream.LastEventID()`, and open a second stream with
`sdk.WithLastEventID(checkpoint)` to verify replay-to-live behavior.
