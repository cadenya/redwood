#!/usr/bin/env bash
# Live read-only test of the generated CLI against the real Cadenya API.
# Requires CADENYA_API_KEY and CADENYA_WORKSPACE_ID in the environment
# (source .env.development). Never prints secrets — output is field-checked,
# not dumped.
#
# Usage: source .env.development && bash e2e/live-cli.sh
set -euo pipefail
cd "$(dirname "$0")/.."

BIN="${TMPDIR:-/tmp}/cadenya-cli-live"
(cd gen/cli && go build -o "$BIN" .)

# 1. Credentials check (account payload parses; secrets not printed).
"$BIN" accounts retrieve | node -e "
const acc = JSON.parse(require('fs').readFileSync(0, 'utf8'));
console.log('accounts retrieve   ok  metadata.id present:', Boolean(acc.metadata?.id));
"

# 2. Workspaces list.
"$BIN" workspaces list --limit=2 | node -e "
const page = JSON.parse(require('fs').readFileSync(0, 'utf8'));
console.log('workspaces list     ok ', page.items.length, 'item(s)');
"

# 3. Objectives list (client-default workspace from env).
"$BIN" objectives list --limit=2 | node -e "
const page = JSON.parse(require('fs').readFileSync(0, 'utf8'));
console.log('objectives list     ok ', page.items.length, 'item(s), nextCursor:', Boolean(page.nextCursor));
"

# 4. Error mapping: a missing objective must exit non-zero with an error line.
if "$BIN" objectives retrieve obj_does_not_exist >/dev/null 2>"$BIN.err"; then
  echo "error mapping       FAIL (expected non-zero exit)"
  exit 1
else
  echo "error mapping       ok  $(head -c 80 "$BIN.err")"
fi

echo
echo "live API checks passed (cli)"
