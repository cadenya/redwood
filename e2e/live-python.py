# Live read-only test of the generated Python SDK against the real Cadenya
# API. Requires CADENYA_API_KEY and CADENYA_WORKSPACE_ID in the environment
# (source .env.development). Never prints secrets; IDs are truncated.
#
# Usage: source .env.development && python e2e/live-python.py
#   (run with gen/python on PYTHONPATH, e.g. PYTHONPATH=gen/python)
import os
import sys

from cadenya import APIError, Cadenya


def short(resource_id):
    return f"{str(resource_id)[:12]}…" if resource_id else resource_id


if not os.environ.get("CADENYA_API_KEY") or not os.environ.get("CADENYA_WORKSPACE_ID"):
    print("missing CADENYA_API_KEY / CADENYA_WORKSPACE_ID")
    sys.exit(1)

# Key AND workspace come from env — no per-call workspace_id below exercises
# the client-defaults feature live.
client = Cadenya()

# 1. Credentials check.
# account.info carries secret material (webhook HMAC secret) — never print it.
account = client.accounts.retrieve()
print(f"accounts.retrieve   ok  info present: {account.info is not None}")

# 2. Workspaces list (pagination envelope against real data).
workspaces = client.workspaces.list(limit=2)
print(f"workspaces.list     ok  {len(workspaces.items)} item(s), has_next_page={workspaces.has_next_page()}")

# 3. Agents in the provided workspace.
agents = client.agents.list(limit=3)
ids = ", ".join(short(a.metadata.id) for a in agents.items if a.metadata)
print(f"agents.list         ok  {ids or '(none)'}")

# 4. Objectives + auto-pagination across real pages (capped at 5).
seen = []
for objective in client.objectives.list(limit=2):
    seen.append(short(objective.metadata.id if objective.metadata else None))
    if len(seen) >= 5:
        break
print(f"objectives.list     ok  {len(seen)} across pages: {', '.join(seen) or '(none)'}")

# 5. Models catalog.
try:
    models = client.models.list(limit=3)
    print(f"models.list         ok  {len(models.items)} item(s)")
except APIError as exc:
    print(f"models.list         skip APIError {exc.status_code}: {exc.message}")

# 6. Error mapping against the real server.
try:
    client.objectives.retrieve("obj_does_not_exist")
    print("error mapping       FAIL (expected an APIError)")
    sys.exit(1)
except APIError as exc:
    print(f"error mapping       ok  status={exc.status_code} code={exc.code}")

print("\nlive API checks passed (python)")
