# API endpoint × SDK live-test matrix

`api-endpoint-live-test-matrix.csv` is the canonical tracker for all 142
OpenAPI operations across TypeScript, Go, Python, Ruby, and the generated CLI.
It is generated from `api-spec.yml`, cross-checked against the generated
manifest, and merged with the checked-in per-SDK result artifacts:

```sh
cargo run --bin live_matrix -- .
```

The generator refuses unknown operations/SDKs, spec-manifest drift, duplicate
operation IDs, invalid statuses, and `completed` cells without evidence. Each
SDK has a status, evidence, and stable snippet locator column. Mock conformance
does not count as live evidence.

Current status vocabulary:

- `completed`: the operation's success path ran against the real API through
  that SDK and its decoded result/effect was asserted;
- `blocked`: a real prerequisite is unavailable, such as an admin role,
  adapter-specific fixture, lifecycle state, or unimplemented endpoint;
- `failed`: an unresolved SDK construction, transport, or decoding defect;
- `not_started`: no qualifying live attempt is recorded yet; and
- `attempted`: historical real calls whose assertion was too broad to prove a
  success path.

`overall_live_status` is `completed` only when all five SDK cells are
completed.

## Python and Ruby catalogs

`snippets-python.py` and `snippets-ruby.rb` derive one deterministic SDK call
from every operation in `gen/manifest/manifest.json`. Each catalog asserts the
current 142-operation cardinality, unique operation IDs, and generated method
and parameter presence against an installed wheel/gem. Catalog generation is
offline and never imports the SDK.

Each record includes:

- `snippet`: a language-native call against `client` and `ctx`;
- `fixture_keys`: required IDs or operation-scoped request fields;
- `optional_kwargs_fixture`: operation-scoped optional arguments;
- `safety`: read, stream, mutation, credential, administrator, or shared-state
  classification;
- `cleanup_operation_ids`: cleanup API relationships where the contract has
  one; and
- `evidence_required`: the minimum proof for a `completed` live status.

The result files have schema version 1 and never contain response bodies,
tokens, credentials, secret values, request headers, or full resource objects.
Statuses mean:

- `completed`: an installed registry artifact issued the real request, got a
  2xx response, and decoded the declared response type. Owned mutable fixtures
  must also be cleaned up successfully before this status is accepted.
- `failed`: the request reached the endpoint but the SDK could not construct,
  transport, or decode it as declared. Only SDK problems belong here.
- `blocked`: a prerequisite was unavailable (authorization, a resource fixture,
  endpoint implementation, required lifecycle state, or adapter-specific
  setup). A 403/501 or deliberately mismatched fixture is not an SDK failure.

Run catalog coverage and installed-artifact validation:

```sh
python e2e/live-matrix/snippets-python.py
/path/to/installed/venv/bin/python e2e/live-matrix/snippets-python.py --validate-sdk

ruby e2e/live-matrix/snippets-ruby.rb
GEM_HOME=/path/to/gem-home GEM_PATH=/path/to/gem-home \
  ruby e2e/live-matrix/snippets-ruby.rb --validate-sdk
```

Read waves include secret-bearing GET endpoints but persist only status and
decoded type evidence. SSE runs only when history discovery supplies a replay
checkpoint, avoiding an unbounded wait:

```sh
/path/to/installed/venv/bin/python e2e/live-matrix/snippets-python.py \
  --live-read-wave --results e2e/live-matrix/results-python.json

GEM_HOME=/path/to/gem-home GEM_PATH=/path/to/gem-home \
  ruby e2e/live-matrix/snippets-ruby.rb \
  --live-read-wave --results e2e/live-matrix/results-ruby.json
```

`owned-wave-python.py` and `owned-wave-ruby.rb` exercise create/update/action/
delete lifecycles with unique names and reverse-order best-effort cleanup.
They intentionally exclude the coordinator-owned account/global-key/model/
membership/tenant-wide operations, which must run serially because they can
invalidate credentials or affect other lanes. These runners are destructive
preproduction tests and require explicit authorization for that exact scope.

The generic single-operation execution mode has a dual mutation gate: the
exact operation ID must be passed to `--allow-operation` and included in
`CADENYA_LIVE_MATRIX_ALLOW_MUTATIONS`. Credential rotation, account-admin,
shared-configuration, append-only, and orphan-producing operations are refused
there and belong in a purpose-built coordinated scenario.

`coordinated-tail-python.py` and `coordinated-tail-ruby.rb` cover the serialized
account/global-credential/provider/model/workspace-member/objective-action
tail. They require an exact language-specific environment opt-in and destructive
CLI flag. Before rotating the managed global token they compare it to the
ambient credential:

- when distinct, the ambient token remains the recovery controller and the
  replacement managed token stays in process memory;
- when identical, the runner refuses unless `CADENYA_ROOT_ENV_FILE` points to
  the absolute root `.env.development`; it validates the old assignment before
  rotation and fsyncs an atomic same-directory replacement immediately after
  obtaining the new token, before another API call;
- global disable/enable is refused when there is no independent recovery
  controller, even after the replacement is safely persisted.

These tails must be run one language at a time by the coordinator. They are
never invoked by ordinary conformance or live-read commands.

TypeScript and CLI use these equivalent waves:

```sh
node e2e/live-matrix/run-typescript-read.mjs
node e2e/live-matrix/run-cli-read.mjs

CADENYA_LIVE_MATRIX_MUTATIONS=typescript node e2e/live-matrix/run-typescript-mutations.mjs
CADENYA_LIVE_MATRIX_MUTATIONS=cli node e2e/live-matrix/run-cli-mutations.mjs
```

The final account/shared-state phase must run serially after all ordinary
lanes. It retrieves global/rotated credentials only in memory and never
rewrites or prints `.env.development`:

```sh
CADENYA_LIVE_MATRIX_COORDINATED=typescript node e2e/live-matrix/run-typescript-coordinated.mjs
CADENYA_LIVE_MATRIX_COORDINATED=cli node e2e/live-matrix/run-cli-coordinated.mjs
```

State-dependent objective actions deliberately use separate fixtures, so an
approve cannot invalidate deny or content submission. Supply only fixtures in
the state required by the operation:

- `CADENYA_LIVE_MATRIX_{FEEDBACK,COMPACT,CONTINUE,CANCEL}_OBJECTIVE_ID`
- `CADENYA_LIVE_MATRIX_{APPROVE,DENY,CONTENT}_OBJECTIVE_ID`
- `CADENYA_LIVE_MATRIX_{APPROVE,DENY,CONTENT}_TOOL_CALL_ID`

Missing fixtures are recorded as `blocked`, never as passing evidence.
