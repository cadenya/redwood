#!/usr/bin/env python3
"""Serialized account/credential/shared-state tail for the installed Python SDK.

THIS ROTATES THE MANAGED GLOBAL API TOKEN and other account secrets. It is deliberately
not part of any ordinary test command. The coordinator must invoke it with both:

    CADENYA_COORDINATED_TAIL=python \
    python coordinated-tail-python.py --rotate-global-and-run-account-tail

The managed global token is retrieved before rotation. If it is distinct from
the ambient credential, the ambient credential remains the recovery controller
and no env file is changed. If it is the SAME credential, the runner refuses
unless CADENYA_ROOT_ENV_FILE names the root `.env.development`; immediately
after rotation it durably atomic-replaces that assignment before any later call.
No secret or response body is printed or stored in test artifacts. Do not run
tails concurrently.
"""

from __future__ import annotations

import argparse
import json
import os
import stat
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Dict, Iterable, Optional

from cadenya import APIError, Cadenya


HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results-python.json"
RUN = f"sdk-tail-python-{int(time.time())}"
OPT_IN = "--rotate-global-and-run-account-tail"

TAIL_OPERATIONS = {
    "AccountService_RotateChallengeToken",
    "AccountService_RotateWebhookSigningKey",
    "GlobalAPIKeyService_GetGlobalAPIKey",
    "GlobalAPIKeyService_DisableGlobalAPIKey",
    "GlobalAPIKeyService_EnableGlobalAPIKey",
    "GlobalAPIKeyService_RotateGlobalAPIKey",
    "WorkspaceAdminService_ListProfiles",
    "WorkspaceAdminService_ListAccountWorkspaces",
    "WorkspaceAdminService_CreateWorkspace",
    "WorkspaceAdminService_GetWorkspace",
    "WorkspaceAdminService_ArchiveWorkspace",
    "WorkspaceAdminService_UpdateWorkspace",
    "WorkspaceAdminService_ListWorkspaceMembers",
    "WorkspaceAdminService_AddWorkspaceMember",
    "WorkspaceAdminService_RemoveWorkspaceMember",
    "AIProviderKeyService_ListAIProviderKeys",
    "AIProviderKeyService_CreateAIProviderKey",
    "AIProviderKeyService_GetAIProviderKey",
    "AIProviderKeyService_UpdateAIProviderKey",
    "AIProviderKeyService_DeleteAIProviderKey",
    "ModelService_ListModels",
    "ModelService_GetModel",
    "ModelService_DisableModel",
    "ModelService_EnableModel",
    "ModelService_SwapModelOnVariations",
    "ObjectiveService_ApproveToolCall",
    "ObjectiveService_DenyToolCall",
    "ObjectiveService_SetToolCallContent",
    "ObjectiveService_CompactObjective",
    "ObjectiveService_ContinueObjective",
}


def resource_id(value: Any) -> Optional[str]:
    metadata = getattr(value, "metadata", None)
    result = getattr(metadata, "id", None)
    return str(result) if result else None


def shell_quote(value: str) -> str:
    return "'" + value.replace("'", "'\"'\"'") + "'"


def validated_env_lines(path: Path, expected_old: str) -> tuple[list[str], int, bool]:
    if not path.is_absolute() or path.name != ".env.development" or not path.is_file():
        raise RuntimeError(
            "CADENYA_ROOT_ENV_FILE must be the existing absolute root .env.development"
        )
    lines = path.read_text().splitlines(keepends=True)
    matches = []
    for index, line in enumerate(lines):
        stripped = line.strip()
        candidate = stripped[len("export "):] if stripped.startswith("export ") else stripped
        if candidate.startswith("CADENYA_API_KEY="):
            matches.append(index)
    if len(matches) != 1:
        raise RuntimeError("root env must contain exactly one CADENYA_API_KEY assignment")
    index = matches[0]
    stripped = lines[index].strip()
    exported = stripped.startswith("export ")
    raw_value = (stripped[len("export "):] if exported else stripped).split("=", 1)[1]
    # The existing repository format is shell assignment syntax. Parse only
    # its safe single-token forms without ever interpolating it into a shell.
    if len(raw_value) >= 2 and raw_value[0] == raw_value[-1] and raw_value[0] in "'\"":
        raw_value = raw_value[1:-1]
    if raw_value != expected_old:
        raise RuntimeError("root env API key changed since process start; refusing overwrite")
    return lines, index, exported


def atomic_replace_api_key(path: Path, expected_old: str, replacement: str) -> None:
    """Durably replace exactly one prevalidated CADENYA_API_KEY assignment."""
    info = path.stat()
    lines, index, exported = validated_env_lines(path, expected_old)
    newline = "\n" if lines[index].endswith("\n") else ""
    lines[index] = ("export " if exported else "") + "CADENYA_API_KEY=" + shell_quote(replacement) + newline

    temporary = path.parent / f".env.development.{os.getpid()}.rotating"
    if temporary.exists():
        raise RuntimeError(f"temporary env path already exists: {temporary}")
    fd = os.open(str(temporary), os.O_WRONLY | os.O_CREAT | os.O_EXCL, stat.S_IMODE(info.st_mode))
    try:
        with os.fdopen(fd, "w") as stream:
            fd = -1
            stream.writelines(lines)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory_fd = os.open(str(path.parent), os.O_RDONLY)
        try:
            os.fsync(directory_fd)
        finally:
            os.close(directory_fd)
    finally:
        if fd >= 0:
            os.close(fd)
        try:
            temporary.unlink()
        except FileNotFoundError:
            pass


def main(argv: Optional[Iterable[str]] = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(OPT_IN, action="store_true", dest="execute")
    args = parser.parse_args(argv)
    if not args.execute or os.environ.get("CADENYA_COORDINATED_TAIL") != "python":
        parser.error(
            "refusing: pass the exact destructive opt-in argument and set "
            "CADENYA_COORDINATED_TAIL=python"
        )
    current_token = os.environ.get("CADENYA_API_KEY", "")
    if not current_token:
        parser.error("CADENYA_API_KEY is required")

    prior = json.loads(RESULTS.read_text()) if RESULTS.exists() else {
        "schemaVersion": 1, "sdk": "python", "operations": {}
    }
    operations: Dict[str, Dict[str, str]] = prior.setdefault("operations", {})
    cleanup: list[tuple[str, Callable[[], Any]]] = []

    def record(operation_id: str, status: str, evidence: str) -> None:
        operations[operation_id] = {"status": status, "evidence": evidence}

    def complete(operation_id: str, value: Any) -> Any:
        record(operation_id, "completed", f"installed wheel; HTTP 2xx; decoded {type(value).__name__}")
        return value

    def attempt(operation_id: str, function: Callable[[], Any], scenario_400: bool = False) -> Any:
        try:
            return complete(operation_id, function())
        except APIError as error:
            if error.status_code == 403 or (scenario_400 and error.status_code == 400):
                prerequisite = "authorization" if error.status_code == 403 else "lifecycle/scenario"
                record(operation_id, "blocked", f"installed wheel; HTTP {error.status_code}; {prerequisite} prerequisite")
            else:
                record(operation_id, "failed", f"installed wheel; APIError HTTP {error.status_code}; body not retained")
        except Exception as error:
            record(operation_id, "failed", f"installed wheel; {type(error).__name__}; body not retained")
        return None

    controller = Cadenya(api_key=current_token)
    client: Optional[Cadenya] = None
    global_disabled = False
    created_workspace_id: Optional[str] = None
    provider_key_id: Optional[str] = None
    tool_set_id: Optional[str] = None
    agent_id: Optional[str] = None
    objective_ids: list[str] = []
    disabled_model_id: Optional[str] = None
    try:
        # The ambient controller is not the managed global key. Prove it can
        # retrieve the current managed token, rotate through the controller,
        # retrieve the fresh token again, and chain that token only in memory.
        before = attempt("GlobalAPIKeyService_GetGlobalAPIKey", controller.api_keys.retrieve_global)
        if before is None:
            raise RuntimeError("RetrieveGlobal failed; refusing rotation")
        before_token = getattr(getattr(before, "spec", None), "token", None)
        if not isinstance(before_token, str) or not before_token.strip():
            raise RuntimeError("RetrieveGlobal returned no managed token")
        del before
        ambient_is_managed = current_token == before_token
        env_path: Optional[Path] = None
        if ambient_is_managed:
            env_destination = os.environ.get("CADENYA_ROOT_ENV_FILE", "")
            if not env_destination:
                raise RuntimeError(
                    "ambient credential is the managed global token; refusing rotation without "
                    "CADENYA_ROOT_ENV_FILE"
                )
            env_path = Path(env_destination)
            # All fallible destination/old-value validation must happen before
            # the irreversible rotation, not after the old token is invalid.
            validated_env_lines(env_path, current_token)
        rotated = attempt("GlobalAPIKeyService_RotateGlobalAPIKey", controller.api_keys.rotate_global)
        if rotated is None:
            raise RuntimeError("RotateGlobal failed; coordinated tail cannot continue")
        if ambient_is_managed:
            # The old controller is now invalid, so the response itself must
            # carry the replacement. Persist it before another network call.
            rotated_token = getattr(getattr(rotated, "spec", None), "token", None)
        else:
            after = controller.api_keys.retrieve_global()
            rotated_token = getattr(getattr(after, "spec", None), "token", None)
            del after
        if not isinstance(rotated_token, str) or not rotated_token.strip() or rotated_token == before_token:
            raise RuntimeError("managed global rotation did not yield a distinct retrievable token")
        if ambient_is_managed:
            assert env_path is not None
            atomic_replace_api_key(env_path, current_token, rotated_token)
            os.environ["CADENYA_API_KEY"] = rotated_token
            controller.close()
            controller = Cadenya(api_key=rotated_token)
        del before_token, rotated
        client = Cadenya(api_key=rotated_token)
        del rotated_token

        challenge = attempt("AccountService_RotateChallengeToken", client.accounts.rotate_challenge_token)
        del challenge  # secret-bearing response intentionally discarded
        webhook = attempt("AccountService_RotateWebhookSigningKey", client.accounts.rotate_webhook_signing_key)
        del webhook

        # Disable/enable are controlled by the independent ambient credential,
        # so disabling the managed token cannot strand the runner.
        if ambient_is_managed:
            record(
                "GlobalAPIKeyService_DisableGlobalAPIKey", "blocked",
                "ambient credential is the managed key; no independent recovery controller",
            )
            record(
                "GlobalAPIKeyService_EnableGlobalAPIKey", "blocked",
                "global disable intentionally not attempted without independent recovery",
            )
        else:
            attempt("GlobalAPIKeyService_DisableGlobalAPIKey", controller.api_keys.disable_global)
            if operations["GlobalAPIKeyService_DisableGlobalAPIKey"]["status"] == "completed":
                global_disabled = True
                attempt("GlobalAPIKeyService_EnableGlobalAPIKey", controller.api_keys.enable_global)
                if operations["GlobalAPIKeyService_EnableGlobalAPIKey"]["status"] == "completed":
                    global_disabled = False

        # Account workspace and membership lifecycle.
        profiles = attempt("WorkspaceAdminService_ListProfiles", lambda: client.workspace_admin.list_profiles(limit=100))
        attempt("WorkspaceAdminService_ListAccountWorkspaces", lambda: client.workspace_admin.list_account(limit=10, include_archived=True))
        workspace = attempt("WorkspaceAdminService_CreateWorkspace", lambda: client.workspace_admin.create(
            metadata={"name": f"{RUN}-workspace"}, spec={"description": "SDK coordinated tail"}
        ))
        created_workspace_id = resource_id(workspace)
        if created_workspace_id:
            cleanup.append(("workspace", lambda: client.workspace_admin.archive(workspace_id=created_workspace_id)))
            attempt("WorkspaceAdminService_GetWorkspace", lambda: client.workspace_admin.retrieve(workspace_id=created_workspace_id))
            attempt("WorkspaceAdminService_UpdateWorkspace", lambda: client.workspace_admin.update(
                workspace_id=created_workspace_id,
                metadata={"name": f"{RUN}-workspace-updated"}, update_mask="metadata.name",
            ))
            members = attempt("WorkspaceAdminService_ListWorkspaceMembers", lambda: client.workspace_admin.list_members(
                workspace_id=created_workspace_id, limit=100
            ))
            existing_ids = {
                str(item.profile_id) for item in (members.items if members else []) if item.profile_id
            }
            profile_ids = [
                profile_id
                for item in (profiles.items if profiles else [])
                if (profile_id := resource_id(item)) is not None
            ]
            candidate = next((profile_id for profile_id in profile_ids if profile_id not in existing_ids), None)
            if candidate:
                added = attempt("WorkspaceAdminService_AddWorkspaceMember", lambda: client.workspace_admin.add_member(
                    workspace_id=created_workspace_id, profile_id=candidate
                ))
                if added is not None:
                    complete("WorkspaceAdminService_RemoveWorkspaceMember", client.workspace_admin.remove_member(
                        workspace_id=created_workspace_id, profile_id=candidate
                    ))
            else:
                record("WorkspaceAdminService_AddWorkspaceMember", "blocked", "no non-member profile fixture")
                record("WorkspaceAdminService_RemoveWorkspaceMember", "blocked", "member was not added")

        # Provider key and its private model set.
        attempt("AIProviderKeyService_ListAIProviderKeys", lambda: client.ai_provider_keys.list(limit=10))
        provider = attempt("AIProviderKeyService_CreateAIProviderKey", lambda: client.ai_provider_keys.create(
            metadata={"name": f"{RUN}-provider"},
            spec={
                "provider": "AI_PROVIDER_OPENAI",
                "credentials": {"type": "apiKey", "api_key": {"api_key": f"{RUN}-not-real"}},
                "config": {"type": "openai", "openai": {}},
            },
        ))
        provider_key_id = resource_id(provider)
        if provider_key_id:
            cleanup.append(("provider", lambda: client.ai_provider_keys.delete(provider_key_id)))
            attempt("AIProviderKeyService_GetAIProviderKey", lambda: client.ai_provider_keys.retrieve(provider_key_id))
            attempt("AIProviderKeyService_UpdateAIProviderKey", lambda: client.ai_provider_keys.update(
                provider_key_id, metadata={"name": f"{RUN}-provider-updated"}, update_mask="metadata.name"
            ))

        model_page = attempt("ModelService_ListModels", lambda: client.models.list(limit=100))
        models = list(model_page.items) if model_page else []
        model_ids = [resource_id(item) for item in models if resource_id(item)]
        primary_model = next((item for item in models if getattr(item, "state", None) == "STATE_ENABLED" and resource_id(item)), None)
        primary_model_id = resource_id(primary_model)
        if primary_model_id:
            attempt("ModelService_GetModel", lambda: client.models.retrieve(primary_model_id))
            attempt("ModelService_DisableModel", lambda: client.models.disable(primary_model_id))
            if operations["ModelService_DisableModel"]["status"] == "completed":
                disabled_model_id = primary_model_id
                attempt("ModelService_EnableModel", lambda: client.models.enable(primary_model_id))
                if operations["ModelService_EnableModel"]["status"] == "completed":
                    disabled_model_id = None
        else:
            for operation_id in ("ModelService_GetModel", "ModelService_DisableModel", "ModelService_EnableModel"):
                record(operation_id, "blocked", "no enabled model fixture")

        provider_models = []
        if provider_key_id:
            for _attempt in range(10):
                try:
                    provider_models = [
                        resource_id(item) for item in client.models.list(
                            ai_provider_key_id=provider_key_id, limit=100
                        ).items if resource_id(item)
                    ]
                except Exception:
                    provider_models = []
                if len(provider_models) >= 2:
                    break
                time.sleep(0.5)
        if len(provider_models) >= 2:
            attempt("ModelService_SwapModelOnVariations", lambda: client.models.swap_on_variations(model_swaps=[{
                "current_model_id": provider_models[0], "next_model_id": provider_models[1],
                "disable_current_after_swap": False,
            }]))
        else:
            record("ModelService_SwapModelOnVariations", "blocked", "owned provider did not expose two model fixtures")

        # Owned approval/denial/content objective scenario.
        scenario_model_id = primary_model_id or (model_ids[0] if model_ids else None)
        if scenario_model_id:
            tool_set = client.tool_sets.create(
                metadata={"name": f"{RUN}-tools"},
                spec={"description": "coordinated tail", "adapter": {"type": "bare", "bare": {}}},
            )
            tool_set_id = resource_id(tool_set)
            cleanup.append(("tool set", lambda: client.tool_sets.delete(tool_set_id)))
            tools = []
            for suffix in ("approve", "deny"):
                tool = client.tool_sets.tools.create(
                    tool_set_id=tool_set_id, metadata={"name": f"{RUN}-{suffix}"},
                    spec={
                        "description": f"Call the {suffix} matrix tool", "requires_approval": True,
                        "parameters": {"type": "object", "properties": {}},
                        "config": {"type": "bare", "bare": {}},
                    },
                )
                tools.append(resource_id(tool))
                cleanup.append(("tool", lambda tool_id=resource_id(tool): client.tool_sets.tools.delete(
                    tool_id, tool_set_id=tool_set_id
                )))
            agent = client.agents.create(
                metadata={"name": f"{RUN}-agent"},
                spec={"variation_selection_mode": "VARIATION_SELECTION_MODE_UNSPECIFIED"},
                default_variation={
                    "metadata": {"name": f"{RUN}-variation"},
                    "spec": {"system_prompt_template": "Call exactly the requested tool.", "model_config": {"model_id": scenario_model_id}},
                },
            )
            agent_id = resource_id(agent)
            cleanup.append(("agent", lambda: client.agents.delete(agent_id)))
            variation_page = client.agents.variations.list(agent_id=agent_id, limit=1)
            variation_id = resource_id(variation_page.items[0])
            for tool_id in tools:
                client.agents.variations.add_assignment(
                    agent_id=agent_id, variation_id=variation_id,
                    body={"type": "toolId", "tool_id": tool_id},
                )
            client.agents.publish(agent_id)

            def create_tool_objective(tool_name: str) -> tuple[str, Optional[str]]:
                objective = client.objectives.create(
                    agent_id=agent_id, variation_id=variation_id, system_prompt_data={},
                    first_user_message=f"Call the tool named {tool_name} now.",
                    metadata={"external_id": f"{RUN}-{tool_name}"},
                )
                objective_id = resource_id(objective)
                objective_ids.append(objective_id)
                cleanup.append(("objective", lambda objective_id=objective_id: client.objectives.cancel(
                    objective_id, reason="coordinated tail cleanup"
                )))
                for _attempt in range(80):
                    page = client.objectives.list_tool_calls(objective_id, limit=20)
                    waiting = next(
                        (item for item in page.items if getattr(item, "status", None) == "TOOL_CALL_STATUS_WAITING_FOR_APPROVAL"),
                        None,
                    )
                    if waiting:
                        return objective_id, resource_id(waiting)
                    time.sleep(0.5)
                return objective_id, None

            approve_objective, approve_call = create_tool_objective(f"{RUN}-approve")
            if approve_call:
                attempt("ObjectiveService_CompactObjective", lambda: client.objectives.compact(approve_objective), scenario_400=True)
                attempt("ObjectiveService_ApproveToolCall", lambda: client.objectives.approve_tool_call(
                    approve_objective, tool_call_id=approve_call
                ))
                if operations["ObjectiveService_ApproveToolCall"]["status"] == "completed":
                    attempt("ObjectiveService_SetToolCallContent", lambda: client.objectives.set_tool_call_content(
                        approve_objective, tool_call_id=approve_call,
                        content=[{"type": "text", "text": {"text": "approved live tail result"}}],
                    ))
            else:
                for operation_id in ("ObjectiveService_CompactObjective", "ObjectiveService_ApproveToolCall", "ObjectiveService_SetToolCallContent"):
                    record(operation_id, "blocked", "approval tool-call fixture did not materialize")

            deny_objective, deny_call = create_tool_objective(f"{RUN}-deny")
            if deny_call:
                attempt("ObjectiveService_DenyToolCall", lambda: client.objectives.deny_tool_call(
                    deny_objective, tool_call_id=deny_call, memo="coordinated tail denial"
                ))
            else:
                record("ObjectiveService_DenyToolCall", "blocked", "denial tool-call fixture did not materialize")

            # Continue only after an objective reaches a contract-accepted
            # completed state; otherwise preserve the scenario as blocked.
            continued = False
            for objective_id in objective_ids:
                for _attempt in range(60):
                    current = client.objectives.retrieve(objective_id)
                    if getattr(current, "state", None) == "STATE_FINALIZED":
                        attempt("ObjectiveService_ContinueObjective", lambda objective_id=objective_id: client.objectives.continue_(
                            objective_id, message="Continue coordinated tail.", enqueue=True
                        ), scenario_400=True)
                        continued = True
                        break
                    if getattr(current, "state", None) in {"STATE_FAILED", "STATE_CANCELLED", "STATE_TIMED_OUT"}:
                        break
                    time.sleep(0.5)
                if continued:
                    break
            if not continued:
                record("ObjectiveService_ContinueObjective", "blocked", "no finalized owned objective fixture")
        else:
            for operation_id in (
                "ObjectiveService_ApproveToolCall", "ObjectiveService_DenyToolCall",
                "ObjectiveService_SetToolCallContent", "ObjectiveService_CompactObjective",
                "ObjectiveService_ContinueObjective",
            ):
                record(operation_id, "blocked", "no model fixture for owned objective scenario")

        # Cleanup explicit account resources last and record their operations.
        if provider_key_id:
            attempt("AIProviderKeyService_DeleteAIProviderKey", lambda: client.ai_provider_keys.delete(provider_key_id))
            cleanup[:] = [(label, fn) for label, fn in cleanup if label != "provider"]
        if created_workspace_id:
            attempt("WorkspaceAdminService_ArchiveWorkspace", lambda: client.workspace_admin.archive(
                workspace_id=created_workspace_id
            ))
            cleanup[:] = [(label, fn) for label, fn in cleanup if label != "workspace"]
    finally:
        if global_disabled:
            try:
                controller.api_keys.enable_global()
                global_disabled = False
            except Exception:
                pass
        if disabled_model_id and client is not None:
            try:
                client.models.enable(disabled_model_id)
                disabled_model_id = None
            except Exception:
                pass
        for _label, function in reversed(cleanup):
            try:
                function()
            except Exception:
                pass
        if client is not None:
            client.close()
        controller.close()
        for operation_id in sorted(TAIL_OPERATIONS):
            operations.setdefault(operation_id, {
                "status": "blocked", "evidence": "coordinated tail stopped before this operation"
            })
        prior["executedAt"] = datetime.now(timezone.utc).isoformat()
        prior["operations"] = operations
        RESULTS.write_text(json.dumps(prior, indent=2, sort_keys=True) + "\n")

    covered = TAIL_OPERATIONS & operations.keys()
    if covered != TAIL_OPERATIONS:
        raise RuntimeError("internal coverage assertion failed")
    return 1 if any(operations[operation_id]["status"] == "failed" for operation_id in TAIL_OPERATIONS) else 0


if __name__ == "__main__":
    raise SystemExit(main())
