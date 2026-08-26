#!/usr/bin/env python3
"""Authorized preproduction mutation wave for the installed Python SDK.

Every mutable resource is uniquely named, IDs stay in memory, and owned
resources are deleted in reverse order. Evidence contains only operation IDs,
status codes/error classes, and decoded type names -- never bodies or secrets.
The account/global/model/member/tenant-wide operations serialized by the root
matrix coordinator are intentionally absent here.
"""

from __future__ import annotations

import json
import os
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Dict, Optional

from cadenya import APIError, APIResponseError, Cadenya


HERE = Path(__file__).resolve().parent
RESULTS = HERE / "results-python.json"
RUN = f"sdk-matrix-python-{int(time.time())}"


def main() -> int:
    prior = json.loads(RESULTS.read_text()) if RESULTS.exists() else {
        "schemaVersion": 1, "sdk": "python", "operations": {}
    }
    operations: Dict[str, Dict[str, str]] = prior["operations"]
    client = Cadenya()
    cleanup: list[tuple[str, Callable[[], Any]]] = []

    def evidence(operation_id: str, status: str, detail: str) -> None:
        operations[operation_id] = {"status": status, "evidence": detail}

    def call(operation_id: str, function: Callable[[], Any]) -> Any:
        try:
            value = function()
            evidence(operation_id, "completed", f"installed wheel; HTTP 2xx; decoded {type(value).__name__}")
            return value
        except APIError as error:
            evidence(operation_id, "failed", f"installed wheel; APIError HTTP {error.status_code}; response body not retained")
        except APIResponseError as error:
            if operation_id == "ObjectiveService_CreateObjective" and str(error).startswith(
                "response missing required field ObjectiveInfo.createdBy"
            ):
                evidence(
                    operation_id,
                    "blocked",
                    "installed wheel reached real 2xx but strict decode hit upstream "
                    "ObjectiveInfo.createdBy contract mismatch; see API contract feedback item 14",
                )
            else:
                evidence(operation_id, "failed", "installed wheel; APIResponseError; response body not retained")
        except Exception as error:
            evidence(operation_id, "failed", f"installed wheel; {type(error).__name__}; response body not retained")
        return None

    def owned(label: str, function: Callable[[], Any]) -> None:
        cleanup.append((label, function))

    def resource_id(value: Any) -> Optional[str]:
        metadata = getattr(value, "metadata", None)
        result = getattr(metadata, "id", None)
        return str(result) if result else None

    try:
        # Workspace API key lifecycle (own token, main test credential remains untouched).
        api_key = call("APIKeyService_CreateAPIKey", lambda: client.api_keys.create(
            metadata={"name": f"{RUN}-api-key"},
            spec={"description": "SDK live matrix disposable key"},
        ))
        api_key_id = resource_id(api_key)
        if api_key_id:
            owned("api key", lambda: client.api_keys.delete(api_key_id))
            call("APIKeyService_GetAPIKey", lambda: client.api_keys.retrieve(api_key_id))
            call("APIKeyService_UpdateAPIKey", lambda: client.api_keys.update(
                api_key_id, metadata={"name": f"{RUN}-api-key-updated"}, update_mask="metadata.name"
            ))
            call("APIKeyService_DisableAPIKey", lambda: client.api_keys.disable(api_key_id))
            call("APIKeyService_EnableAPIKey", lambda: client.api_keys.enable(api_key_id))
            call("APIKeyService_RotateAPIKey", lambda: client.api_keys.rotate(api_key_id))
            call("APIKeyService_DeleteAPIKey", lambda: client.api_keys.delete(api_key_id))
            cleanup.pop()

        # Provider/model mutations are serialized by the root coordinator.
        # This lane leaves those result cells untouched.
        provider_key = None
        provider_key_id = resource_id(provider_key)
        if provider_key_id:
            owned("provider key", lambda: client.ai_provider_keys.delete(provider_key_id))
            call("AIProviderKeyService_GetAIProviderKey", lambda: client.ai_provider_keys.retrieve(provider_key_id))
            call("AIProviderKeyService_UpdateAIProviderKey", lambda: client.ai_provider_keys.update(
                provider_key_id,
                metadata={"name": f"{RUN}-provider-key-updated"},
                update_mask="metadata.name",
            ))
            call("AIProviderKeyService_DeleteAIProviderKey", lambda: client.ai_provider_keys.delete(provider_key_id))
            cleanup.pop()

        # Workspace secret lifecycle.
        workspace_secret = call("WorkspaceSecretService_CreateWorkspaceSecret", lambda: client.workspace_secrets.create(
            metadata={"name": f"{RUN}-workspace-secret"}, spec={"value": f"{RUN}-value"}
        ))
        workspace_secret_id = resource_id(workspace_secret)
        if workspace_secret_id:
            owned("workspace secret", lambda: client.workspace_secrets.delete(workspace_secret_id))
            call("WorkspaceSecretService_GetWorkspaceSecret", lambda: client.workspace_secrets.retrieve(workspace_secret_id))
            call("WorkspaceSecretService_UpdateWorkspaceSecret", lambda: client.workspace_secrets.update(
                workspace_secret_id, spec={"value": f"{RUN}-updated"}, update_mask="spec.value"
            ))
            call("WorkspaceSecretService_DeleteWorkspaceSecret", lambda: client.workspace_secrets.delete(workspace_secret_id))
            cleanup.pop()

        # Memory layer + entry lifecycle.
        layer = call("MemoryService_CreateMemoryLayer", lambda: client.memory_layers.create(
            metadata={"name": f"{RUN}-memory"},
            spec={"type": "MEMORY_LAYER_TYPE_SKILLS", "description": "SDK matrix"},
        ))
        layer_id = resource_id(layer)
        if layer_id:
            owned("memory layer", lambda: client.memory_layers.delete(layer_id))
            call("MemoryService_GetMemoryLayer", lambda: client.memory_layers.retrieve(layer_id))
            call("MemoryService_ListMemoryEntries", lambda: client.memory_layers.entries.list(
                memory_layer_id=layer_id, limit=1
            ))
            call("MemoryService_UpdateMemoryLayer", lambda: client.memory_layers.update(
                layer_id, metadata={"name": f"{RUN}-memory-updated"}, update_mask="metadata.name"
            ))
            entry = call("MemoryService_CreateMemoryEntry", lambda: client.memory_layers.entries.create(
                memory_layer_id=layer_id,
                metadata={"name": f"{RUN}-entry"},
                spec={"type": "content", "content": "live matrix content", "key": f"{RUN}-key"},
            ))
            entry_id = resource_id(entry)
            if entry_id:
                owned("memory entry", lambda: client.memory_layers.entries.delete(layer_id, entry_id))
                call("MemoryService_GetMemoryEntry", lambda: client.memory_layers.entries.retrieve(layer_id, entry_id))
                call("MemoryService_UpdateMemoryEntry", lambda: client.memory_layers.entries.update(
                    layer_id, entry_id,
                    metadata={"name": f"{RUN}-entry-updated"}, update_mask="metadata.name"
                ))
                call("MemoryService_DeleteMemoryEntry", lambda: client.memory_layers.entries.delete(layer_id, entry_id))
                cleanup.pop()

        # Bare tool set, secret, and tool. Keep these alive for variation assignment.
        tool_set = call("ToolService_CreateToolSet", lambda: client.tool_sets.create(
            metadata={"name": f"{RUN}-tool-set"},
            spec={"description": "SDK matrix", "adapter": {"type": "bare", "bare": {}}},
        ))
        tool_set_id = resource_id(tool_set)
        tool_id = None
        if tool_set_id:
            owned("tool set", lambda: client.tool_sets.delete(tool_set_id))
            call("ToolService_GetToolSet", lambda: client.tool_sets.retrieve(tool_set_id))
            call("ToolService_UpdateToolSet", lambda: client.tool_sets.update(
                tool_set_id, metadata={"name": f"{RUN}-tool-set-updated"}, update_mask="metadata.name"
            ))
            call("ToolService_ArchiveToolSet", lambda: client.tool_sets.archive(tool_set_id))
            call("ToolService_UnarchiveToolSet", lambda: client.tool_sets.unarchive(tool_set_id))

            secret = call("ToolService_CreateToolSetSecret", lambda: client.tool_sets.secrets.create(
                tool_set_id=tool_set_id,
                metadata={"name": f"{RUN}-tool-secret"}, spec={"value": f"{RUN}-value"},
            ))
            secret_id = resource_id(secret)
            if secret_id:
                owned("tool secret", lambda: client.tool_sets.secrets.delete(tool_set_id, secret_id))
                call("ToolService_GetToolSetSecret", lambda: client.tool_sets.secrets.retrieve(tool_set_id, secret_id))
                call("ToolService_UpdateToolSetSecret", lambda: client.tool_sets.secrets.update(
                    tool_set_id, secret_id,
                    spec={"value": f"{RUN}-updated"}, update_mask="spec.value"
                ))
                call("ToolService_DeleteToolSetSecret", lambda: client.tool_sets.secrets.delete(tool_set_id, secret_id))
                cleanup.pop()

            tool = call("ToolService_CreateTool", lambda: client.tool_sets.tools.create(
                tool_set_id=tool_set_id,
                metadata={"name": f"{RUN}-tool"},
                spec={
                    "description": "SDK matrix bare tool", "requires_approval": False,
                    "parameters": {"type": "object", "properties": {}},
                    "config": {"type": "bare", "bare": {}},
                },
            ))
            tool_id = resource_id(tool)
            if tool_id:
                owned("tool", lambda: client.tool_sets.tools.delete(tool_set_id, tool_id))
                call("ToolService_GetTool", lambda: client.tool_sets.tools.retrieve(tool_set_id, tool_id))
                call("ToolService_UpdateTool", lambda: client.tool_sets.tools.update(
                    tool_set_id, tool_id,
                    metadata={"name": f"{RUN}-tool-updated"}, update_mask="metadata.name"
                ))
                call("ToolService_OmitTool", lambda: client.tool_sets.tools.omit(tool_set_id, tool_id))
                call("ToolService_RestoreTool", lambda: client.tool_sets.tools.restore(tool_set_id, tool_id))

        # Agent + variation + schedule + assignments.
        # Read-only fixture selection; root owns the model result cells.
        try:
            model_page = client.models.list(limit=1)
        except Exception:
            model_page = None
        model_id = resource_id(model_page.items[0]) if model_page and model_page.items else None
        agent = call("AgentService_CreateAgent", lambda: client.agents.create(
            metadata={"name": f"{RUN}-agent"},
            spec={"variation_selection_mode": "VARIATION_SELECTION_MODE_UNSPECIFIED"},
            default_variation={
                "metadata": {"name": f"{RUN}-default"},
                "spec": {"system_prompt_template": "Reply concisely.", "model_config": {"model_id": model_id}},
            },
        )) if model_id else None
        agent_id = resource_id(agent)
        variation_id = None
        if agent_id:
            owned("agent", lambda: client.agents.delete(agent_id))
            call("AgentService_GetAgent", lambda: client.agents.retrieve(agent_id))
            call("AgentService_UpdateAgent", lambda: client.agents.update(
                agent_id, metadata={"name": f"{RUN}-agent-updated"}, update_mask="metadata.name"
            ))
            call("AgentService_ArchiveAgent", lambda: client.agents.archive(agent_id))
            call("AgentService_UnarchiveAgent", lambda: client.agents.unarchive(agent_id))
            call("AgentService_PublishAgent", lambda: client.agents.publish(agent_id))
            call("AgentService_UnpublishAgent", lambda: client.agents.unpublish(agent_id))

            variation = call("AgentVariationService_CreateAgentVariation", lambda: client.agents.variations.create(
                agent_id=agent_id, metadata={"name": f"{RUN}-variation"},
                spec={"system_prompt_template": "Reply concisely.", "model_config": {"model_id": model_id}},
            ))
            variation_id = resource_id(variation)
            if variation_id:
                owned("variation", lambda: client.agents.variations.delete(agent_id, variation_id))
                call("AgentVariationService_GetAgentVariation", lambda: client.agents.variations.retrieve(agent_id, variation_id))
                call("AgentVariationService_UpdateAgentVariation", lambda: client.agents.variations.update(
                    agent_id, variation_id,
                    metadata={"name": f"{RUN}-variation-updated"}, update_mask="metadata.name"
                ))
                if tool_id:
                    assignment = call("AgentVariationService_AddAgentVariationAssignment", lambda: client.agents.variations.add_assignment(
                        agent_id=agent_id, variation_id=variation_id,
                        body={"type": "toolId", "tool_id": tool_id},
                    ))
                    assignment_id = getattr(assignment, "id", None)
                    if assignment_id:
                        call("AgentVariationService_RemoveAgentVariationAssignment", lambda: client.agents.variations.remove_assignment(
                            agent_id, variation_id, assignment_id
                        ))
                if layer_id:
                    memory_assignment = call("AgentVariationService_AddAgentVariationMemoryLayer", lambda: client.agents.variations.add_memory_layer(
                        agent_id=agent_id, variation_id=variation_id, memory_layer_id=layer_id, position=0
                    ))
                    memory_assignment_id = getattr(memory_assignment, "id", None)
                    if memory_assignment_id:
                        call("AgentVariationService_UpdateAgentVariationMemoryLayer", lambda: client.agents.variations.update_memory_layer(
                            agent_id, variation_id, memory_assignment_id, position=1
                        ))
                        call("AgentVariationService_RemoveAgentVariationMemoryLayer", lambda: client.agents.variations.remove_memory_layer(
                            agent_id, variation_id, memory_assignment_id
                        ))

                schedule = call("AgentScheduleService_CreateAgentSchedule", lambda: client.agents.schedules.create(
                    agent_id=agent_id, metadata={"name": f"{RUN}-schedule"},
                    spec={
                        "schedule": {"intervals": [{"every": "86400s"}], "timezone": "Etc/UTC"},
                        "variation_id": variation_id, "first_user_message": "Scheduled matrix probe",
                        "system_prompt_data": {},
                    },
                ))
                schedule_id = resource_id(schedule)
                if schedule_id:
                    owned("schedule", lambda: client.agents.schedules.delete(agent_id, schedule_id))
                    call("AgentScheduleService_GetAgentSchedule", lambda: client.agents.schedules.retrieve(agent_id, schedule_id))
                    call("AgentScheduleService_UpdateAgentSchedule", lambda: client.agents.schedules.update(
                        agent_id, schedule_id,
                        metadata={"name": f"{RUN}-schedule-updated"}, update_mask="metadata.name"
                    ))
                    call("AgentScheduleService_PauseAgentSchedule", lambda: client.agents.schedules.pause(agent_id, schedule_id))
                    call("AgentScheduleService_ResumeAgentSchedule", lambda: client.agents.schedules.resume(agent_id, schedule_id))
                    call("AgentScheduleService_ArchiveAgentSchedule", lambda: client.agents.schedules.archive(agent_id, schedule_id))
                    call("AgentScheduleService_DeleteAgentSchedule", lambda: client.agents.schedules.delete(agent_id, schedule_id))
                    cleanup.pop()

            # Widget lifecycle uses the owned agent.
            widget = call("WidgetService_CreateWidget", lambda: client.widgets.create(
                metadata={"name": f"{RUN}-widget"}, spec={"agent_id": agent_id}
            ))
            widget_id = resource_id(widget)
            if widget_id:
                owned("widget", lambda: client.widgets.delete(widget_id))
                call("WidgetService_GetWidget", lambda: client.widgets.retrieve(widget_id))
                call("WidgetService_UpdateWidget", lambda: client.widgets.update(
                    widget_id, metadata={"name": f"{RUN}-widget-updated"}, update_mask="metadata.name"
                ))
                call("WidgetService_ArchiveWidget", lambda: client.widgets.archive(widget_id))
                call("WidgetService_UnarchiveWidget", lambda: client.widgets.unarchive(widget_id))
                session = call("WidgetSessionService_CreateWidgetSession", lambda: client.widget_sessions.create(
                    metadata={"external_id": f"{RUN}-session"}, spec={"widget_id": widget_id}
                ))
                session_id = resource_id(session)
                if session_id:
                    owned("widget session", lambda: client.widget_sessions.delete(session_id))
                    call("WidgetSessionService_GetWidgetSession", lambda: client.widget_sessions.retrieve(session_id))
                    call("WidgetSessionService_RevokeWidgetSession", lambda: client.widget_sessions.revoke(session_id))
                    call("WidgetSessionService_DeleteWidgetSession", lambda: client.widget_sessions.delete(session_id))
                    cleanup.pop()
                tenant_external_id = f"{RUN}-tenant"
                tenant_ref = f"external_id:{tenant_external_id}"
                tenant_session = call("WidgetSessionService_CreateWidgetSession", lambda: client.widget_sessions.create(
                    metadata={"external_id": f"{RUN}-tenant-session"},
                    spec={"widget_id": widget_id, "tenant": {"id": tenant_external_id}},
                ))
                if resource_id(tenant_session):
                    call("TenantService_GetTenant", lambda: client.tenants.retrieve(tenant_ref))
                    call("TenantService_ListTenantSubjects", lambda: client.tenants.list_subjects(tenant_ref, limit=1))
                    call("WidgetSessionService_DeleteTenantWidgetSessions", lambda: client.widget_sessions.delete_tenant(
                        tenant_id=tenant_ref
                    ))
                    call("TenantService_DeleteTenant", lambda: client.tenants.delete(tenant_ref))
                call("WidgetService_DeleteWidget", lambda: client.widgets.delete(widget_id))
                cleanup.pop()

            # Basic objective action wave; specialized tool-call actions are
            # left to the composed objective runner.
            if variation_id:
                call("AgentService_PublishAgent", lambda: client.agents.publish(agent_id))
                objective = call("ObjectiveService_CreateObjective", lambda: client.objectives.create(
                    agent_id=agent_id, variation_id=variation_id, system_prompt_data={},
                    first_user_message="Reply with LIVE_MATRIX_OK.",
                    metadata={"external_id": f"{RUN}-objective"},
                ))
                objective_id = resource_id(objective)
                if objective_id:
                    owned("objective cancel", lambda: client.objectives.cancel(objective_id, reason="matrix cleanup"))
                    call("ObjectiveService_CompactObjective", lambda: client.objectives.compact(objective_id))
                    call("ObjectiveService_CreateObjectiveFeedback", lambda: client.objectives.create_feedback(
                        objective_id, metadata={"external_id": f"{RUN}-feedback"},
                        data={"score": 1.0, "comment": "SDK live matrix"},
                    ))
                    # Continue is state-dependent. Give the deliberately short
                    # objective a bounded chance to finalize before invoking it.
                    for _attempt in range(20):
                        current = client.objectives.retrieve(objective_id)
                        if current.state in {"STATE_FINALIZED", "STATE_FAILED", "STATE_TIMED_OUT"}:
                            break
                        time.sleep(0.5)
                    call("ObjectiveService_ContinueObjective", lambda: client.objectives.continue_(
                        objective_id, message="Continue the live matrix probe.", enqueue=True
                    ))
                    call("ObjectiveService_CancelObjective", lambda: client.objectives.cancel(objective_id, reason="matrix complete"))
                    cleanup.pop()

        # Uploads have no delete endpoint; this intentionally leaves a unique,
        # empty upload record that can be located by RUN.
        upload = call("UploadService_CreateUpload", lambda: client.uploads.create(
            metadata={"name": f"{RUN}-upload"},
            spec={"filename": "matrix-byte.txt", "content_type": "text/plain", "size_bytes": "1"},
        ))
        upload_id = resource_id(upload)
        if upload_id:
            call("UploadService_GetUpload", lambda: client.uploads.retrieve(upload_id))

        # Delete nested resources and parents after their operations complete.
        if tool_id and tool_set_id:
            call("ToolService_DeleteTool", lambda: client.tool_sets.tools.delete(tool_set_id, tool_id))
            # Remove the matching cleanup entry without relying on its position.
            cleanup[:] = [(label, fn) for label, fn in cleanup if label != "tool"]
        if tool_set_id:
            call("ToolService_DeleteToolSet", lambda: client.tool_sets.delete(tool_set_id))
            cleanup[:] = [(label, fn) for label, fn in cleanup if label != "tool set"]
        if layer_id:
            call("MemoryService_DeleteMemoryLayer", lambda: client.memory_layers.delete(layer_id))
            cleanup[:] = [(label, fn) for label, fn in cleanup if label != "memory layer"]
        if variation_id and agent_id:
            call("AgentVariationService_DeleteAgentVariation", lambda: client.agents.variations.delete(agent_id, variation_id))
            cleanup[:] = [(label, fn) for label, fn in cleanup if label != "variation"]
        if agent_id:
            call("AgentService_DeleteAgent", lambda: client.agents.delete(agent_id))
            cleanup[:] = [(label, fn) for label, fn in cleanup if label != "agent"]
    finally:
        for _label, function in reversed(cleanup):
            try:
                function()
            except Exception:
                pass
        client.close()
        prior["executedAt"] = datetime.now(timezone.utc).isoformat()
        prior["operations"] = operations
        RESULTS.write_text(json.dumps(prior, indent=2, sort_keys=True) + "\n")

    return 1 if any(item["status"] == "failed" for item in operations.values()) else 0


if __name__ == "__main__":
    raise SystemExit(main())
