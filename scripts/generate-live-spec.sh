#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
spec_url="${REDWOOD_SPEC_URL:-https://openapi.cadenya.com/api-spec.yml}"
out_root="${1:-${repo_root}/gen}"
redwood_bin="${REDWOOD_BIN:-${repo_root}/target/release/redwood}"

if [[ ! -x "${redwood_bin}" ]]; then
  echo "redwood binary not found or not executable: ${redwood_bin}" >&2
  exit 1
fi

mkdir -p "${out_root}"

spec_snapshot="${out_root}/api-spec.ci.yml"
curl --fail --silent --show-error --location "${spec_url}" --output "${spec_snapshot}"

if command -v sha256sum >/dev/null 2>&1; then
  sha256sum "${spec_snapshot}"
else
  shasum -a 256 "${spec_snapshot}"
fi

for target in typescript go python ruby cli docs manifest openapi; do
  "${redwood_bin}" \
    --spec "${spec_snapshot}" \
    --language "${target}" \
    --config "${repo_root}/redwood.toml" \
    --out "${out_root}/${target}"
done

# Exercise Redwood's URL input path directly in addition to using the frozen
# snapshot above for a consistent cross-language matrix.
url_probe="$(mktemp -d "${TMPDIR:-/tmp}/redwood-url-probe.XXXXXX")"
trap 'rm -rf "${url_probe}"' EXIT
"${redwood_bin}" --spec "${spec_url}" --language docs --out "${url_probe}"

echo "generated all targets from ${spec_url}"
