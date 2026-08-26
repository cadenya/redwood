# Redwood

Redwood is a statically typed SDK and CLI generator. One Rust binary reads an
OpenAPI 3.0 or 3.1 document, lowers it into a normalized intermediate
representation, and generates production-ready TypeScript, Go, Python, and Ruby
SDKs, a Go-based CLI, API reference documentation, and conformance artifacts.

## Installation

### Homebrew

```sh
brew install cadenya/tools/redwood
```

### GitHub Releases

Release archives for macOS, Linux, and Windows are published on the
[GitHub Releases page](https://github.com/cadenya/redwood/releases). macOS
binaries are signed and notarized by Cadenya.

### Build from source

Redwood requires Rust 1.85 or newer.

```sh
git clone https://github.com/cadenya/redwood.git
cd redwood
cargo build --release --locked
./target/release/redwood --help
```

## Quick start

```sh
redwood \
  --spec https://openapi.cadenya.com/api-spec.yml \
  --language typescript \
  --config redwood.toml \
  --out gen/typescript
```

`--spec` accepts a YAML or JSON file path or an HTTP(S) URL. `--out` defaults
to `gen/<language>`.

Available targets:

| Target | Output |
| --- | --- |
| `typescript` | Strict TypeScript SDK using native `fetch` |
| `go` | Go SDK with generated tests |
| `python` | Typed Python SDK using `httpx` |
| `ruby` | Ruby gem using Faraday, with generated RSpec tests |
| `cli` | Go CLI built on the generated Go SDK |
| `docs` | Standalone `api.md` reference |
| `manifest` | Language-neutral conformance manifest |
| `openapi` | Normalized OpenAPI document with generated code samples |

Run `redwood --help` for the complete command-line reference.

## Configuration

Configuration is optional. A single TOML file controls shared policy and
language-specific packaging:

```toml
[api]
name = "Example"
base_url = "https://api.example.com"
client_params = ["workspaceId"]

[lang.typescript]
package_name = "@example/sdk"
package_version = "1.0.0"

[lang.go]
module_path = "github.com/example/example-go"
package_name = "example"

[lang.python]
package_name = "example"

[lang.ruby]
gem_name = "example"

[lang.cli]
module_path = "github.com/example/example-cli"
binary_name = "example"
sdk_module = "github.com/example/example-go"
sdk_replace = "../go"
```

Use `--config redwood.toml` to load it. An explicit `--lang-config` remains
available for compatibility and overrides the matching `[lang.*]` section.
See the repository's [redwood.toml](redwood.toml) for the complete Cadenya
configuration, including resource nesting, positional CLI arguments, display
columns, aliases, special casing, and SSE policy.

## Architecture

```text
OpenAPI 3.0/3.1  ->  ir::lower  ->  normalized IR  ->  Backend trait
   (openapi.rs)      (policy)       (ir/mod.rs)       |-- TypeScript
                                                        |-- Go
                                                        |-- Python
                                                        |-- Ruby
                                                        |-- CLI
                                                        |-- docs
                                                        |-- manifest
                                                        `-- OpenAPI export
```

- `src/openapi.rs` is a deliberately simple Serde mirror of the OpenAPI
  document. No generator policy lives there.
- `src/ir/lower.rs` resolves references, composes schemas, lifts anonymous
  types, models unions, derives resources and method names, and detects
  pagination, SSE, webhooks, authentication, and request/response direction.
- `src/backends` contains projections of the normalized IR. Backends never see
  unresolved OpenAPI references.
- `runtime` contains the hand-written transport, pagination, SSE, and webhook
  code embedded into generated SDKs.

## Generated behavior

Redwood generates typed request and response shapes, cursor pagination,
automatic retries with `Retry-After`, structured API errors, SSE streams,
per-request options, client-level path defaults, and Standard Webhooks
verification. The CLI adds shell-oriented command grammar, persistent profiles
and defaults, structured output modes, and redacted HTTP debug logging.

## Development and testing

The deterministic Rust suite uses the checked-in OpenAPI fixture:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
```

CI additionally generates every target from the current published Cadenya
contract at `https://openapi.cadenya.com/api-spec.yml`. Generated SDK tests and
all-operation conformance runners execute independently for TypeScript, Go,
Python, Ruby, and CLI.

Read-only production tests require local credentials and are intentionally not
part of pull-request CI:

```sh
source .env.development
node e2e/live.mjs
```

## Releases

Release Please maintains the version and changelog. Merging its release PR
creates a `vX.Y.Z` tag. GoReleaser then cross-compiles Redwood, signs and
notarizes the macOS binaries, publishes archives, checksums and Linux packages,
and updates `cadenya/homebrew-tools`.

See [RELEASING.md](RELEASING.md) for maintainer instructions.

## Contributing and security

See [CONTRIBUTING.md](CONTRIBUTING.md) before submitting a change. Please report
security issues privately using the process in [SECURITY.md](SECURITY.md).

Redwood is licensed under the [Apache License 2.0](LICENSE).
