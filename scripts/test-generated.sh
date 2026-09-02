#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
target="${1:?usage: scripts/test-generated.sh <typescript|go|python|ruby|cli>}"
export GOCACHE="${REDWOOD_GO_CACHE:-${TMPDIR:-/tmp}/redwood-go-build-cache}"
mkdir -p "${GOCACHE}"

case "${target}" in
  typescript)
    (
      cd "${repo_root}/gen/typescript"
      npm install --no-audit --no-fund
      npm run build
      npm run typecheck
    )
    node "${repo_root}/e2e/smoke.mjs"
    node "${repo_root}/e2e/ts-config-matrix.mjs"
    node "${repo_root}/e2e/ts-apipromise.mjs"
    node "${repo_root}/e2e/ts-directional-wire.mjs"
    node "${repo_root}/e2e/ts-query-flatten.mjs"
    node "${repo_root}/e2e/ts-sse-reconnect.mjs"
    node "${repo_root}/e2e/conformance/ts-driver.mjs"
    ;;
  go)
    (
      cd "${repo_root}/gen/go"
      go test ./...
    )
    node "${repo_root}/e2e/conformance/run-go.mjs"
    ;;
  python)
    python3 -m pip wheel --no-deps --wheel-dir "$(mktemp -d "${TMPDIR:-/tmp}/redwood-wheel.XXXXXX")" "${repo_root}/gen/python"
    PYTHON="${PYTHON:-python3}" node "${repo_root}/e2e/conformance/run-python.mjs"
    ;;
  ruby)
    (
      cd "${repo_root}/gen/ruby"
      bundle install
      bundle exec rspec
      gem_out_dir="$(mktemp -d "${TMPDIR:-/tmp}/redwood-gem.XXXXXX")"
      gem build cadenya.gemspec --output "${gem_out_dir}/cadenya.gem"
    )
    RUBY="${RUBY:-ruby}" node "${repo_root}/e2e/conformance/run-ruby.mjs"
    ;;
  cli)
    cli_bin="$(mktemp "${TMPDIR:-/tmp}/cadenya.XXXXXX")"
    (
      cd "${repo_root}/gen/cli"
      go test ./...
      go build -o "${cli_bin}" .
    )
    "${cli_bin}" --version
    "${cli_bin}" --help >/dev/null
    node "${repo_root}/e2e/conformance/run-cli.mjs"
    node "${repo_root}/e2e/cli-body.mjs"
    cargo build --locked
    REDWOOD_BIN="${repo_root}/target/debug/redwood" node "${repo_root}/e2e/cli-auth.mjs"
    ;;
  *)
    echo "unknown generated target: ${target}" >&2
    exit 2
    ;;
esac
