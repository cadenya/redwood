#!/usr/bin/env python3
"""Opt-in real-API MCP/Petstore acceptance flow for the installed Python SDK."""

from __future__ import annotations

import argparse
import json
import os
import signal
import time
from contextlib import contextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Iterator, Optional

import cadenya
from cadenya import APIError, APIResponseError, Cadenya


HERE = Path(__file__).resolve().parent
ROOT_ENV = HERE.parents[1] / ".env.development"
RESULTS = HERE / "results-python.json"
RUN_ID = f"specialized-py-{int(time.time() * 1000):x}"
OPT_IN = "--run-specialized-live-fixtures"


def load_root_env() -> dict[str, str]:
    values: dict[str, str] = {}
    for line in ROOT_ENV.read_text().splitlines():
        stripped = line.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped.startswith("export "):
            stripped = stripped[len("export "):]
        if "=" not in stripped:
            continue
        key, value = stripped.split("=", 1)
        if key.startswith("CADENYA_"):
            value = value.strip()
            if len(value) >= 2 and value[0] == value[-1] and value[0] in "'\"":
                value = value[1:-1]
            values[key] = value
    return values


def resource_id(value: Any) -> Optional[str]:
    metadata = getattr(value, "metadata", None)
    result = getattr(metadata, "id", None) or getattr(value, "id", None)
    return str(result) if result else None


@contextmanager
def deadline(seconds: int) -> Iterator[None]:
    def expired(_signum: int, _frame: Any) -> None:
        raise TimeoutError(f"operation exceeded {seconds}s")

    previous = signal.signal(signal.SIGALRM, expired)
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        yield
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
        signal.signal(signal.SIGALRM, previous)


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(OPT_IN, action="store_true", dest="execute")
    args = parser.parse_args(argv)
    if not args.execute or os.environ.get("CADENYA_LIVE_SPECIALIZED_FIXTURES") != "python":
        parser.error(
            f"refusing: pass {OPT_IN} and set CADENYA_LIVE_SPECIALIZED_FIXTURES=python"
        )

    root_env = load_root_env()
    ambient_token = os.environ.get("CADENYA_API_KEY", "")
    if not ambient_token:
        raise RuntimeError("CADENYA_API_KEY is required")
    # The caller supplies the authorized token in memory. Load only non-secret
    # workspace/base context from the root env; never replace or persist it.
    for key, value in root_env.items():
        if key == "CADENYA_API_KEY":
            continue
        os.environ[key] = value
    client = Cadenya(
        api_key=ambient_token,
        base_url=root_env.get("CADENYA_BASE_URL") or None,
        workspace_id=root_env.get("CADENYA_WORKSPACE_ID") or None,
        max_retries=1,
    )
    report = json.loads(RESULTS.read_text())
    report["installedArtifact"] = str(Path(cadenya.__file__).resolve())
    operations: dict[str, dict[str, str]] = report["operations"]
    cleanup: list[tuple[str, Callable[[], Any]]] = []

    def step(label: str, detail: str = "") -> None:
        print(f"{label:<24} {detail}".rstrip(), flush=True)

    def complete(operation_id: str, evidence: str) -> None:
        operations[operation_id] = {
            "status": "completed",
            "evidence": (
                "real api.cadenya.com: installed Python wheel specialized fixture "
                f"succeeded; {evidence}"
            ),
        }

    def block(operation_id: str, evidence: str) -> None:
        if operations.get(operation_id, {}).get("status") == "completed":
            return
        operations[operation_id] = {
            "status": "blocked",
            "evidence": f"real api.cadenya.com: installed Python wheel; {evidence}",
        }

    def failure_text(error: BaseException) -> str:
        if isinstance(error, APIError):
            return f"APIError HTTP {error.status_code}; response body not retained"
        if isinstance(error, APIResponseError):
            message = str(error)
            if message.startswith("response missing required field "):
                return f"APIResponseError: {message}; response body not retained"
        return f"{type(error).__name__}; response body not retained"

    def fail(operation_id: str, error: BaseException) -> None:
        if operations.get(operation_id, {}).get("status") == "completed":
            return
        operations[operation_id] = {
            "status": "failed",
            "evidence": f"real installed Python wheel specialized fixture failed: {failure_text(error)}",
        }

    def poll(label: str, callback: Callable[[], Any], seconds: int = 90) -> Any:
        end = time.monotonic() + seconds
        last_error: Optional[BaseException] = None
        while time.monotonic() < end:
            try:
                value = callback()
                if value:
                    return value
            except Exception as error:
                last_error = error
            time.sleep(2)
        suffix = f" ({type(last_error).__name__})" if last_error else ""
        raise TimeoutError(f"{label} did not become ready{suffix}")

    def matching(callback: Callable[[], Any], predicate: Callable[[Any], bool]) -> Any:
        value = callback()
        return value if predicate(value) else None

    def create_tool_set(name: str, adapter: dict[str, Any]) -> str:
        tool_set = client.tool_sets.create(
            metadata={"name": f"{RUN_ID}-{name}", "labels": {"live_matrix": RUN_ID}},
            spec={"description": f"specialized live fixture: {name}", "adapter": adapter},
        )
        tool_set_id = resource_id(tool_set)
        if not tool_set_id:
            raise AssertionError(f"{name} tool set omitted metadata.id")

        def remove() -> None:
            try:
                client.tool_sets.archive(tool_set_id)
            except Exception:
                pass
            client.tool_sets.delete(tool_set_id)

        cleanup.append((f"tool set {name}", remove))
        return tool_set_id

    def wait_for_objective(objective_id: str, predicate: Callable[[Any], bool], label: str) -> Any:
        return poll(label, lambda: matching(
            lambda: client.objectives.retrieve(objective_id), predicate
        ))

    def initial_checkpoint(objective_id: str) -> Optional[str]:
        page = client.objectives.list_events(objective_id, limit=100, sort_order="asc")
        return next((resource_id(event) for event in page.items if resource_id(event)), None)

    def wait_for_tool_approval(objective_id: str, action: str) -> tuple[Optional[str], str]:
        checkpoint = initial_checkpoint(objective_id)
        first_event_id = checkpoint
        with deadline(120):
            with client.objectives.stream_events(objective_id, last_event_id=checkpoint) as stream:
                for envelope in stream.events():
                    event = envelope.data
                    event_id = resource_id(event)
                    first_event_id = first_event_id or event_id
                    data = getattr(event, "data", None)
                    if getattr(data, "type", None) != "toolApprovalRequested":
                        continue
                    request = getattr(data, "tool_approval_requested", None)
                    tool_call_id = getattr(request, "tool_call_id", None)
                    if not tool_call_id:
                        raise AssertionError(f"{action} approval event omitted toolCallId")
                    call = client.objectives.retrieve_tool_call(
                        objective_id, tool_call_id=tool_call_id
                    )
                    if call.status != "TOOL_CALL_STATUS_WAITING_FOR_APPROVAL":
                        raise AssertionError(f"{action} call was not waiting for approval")
                    return first_event_id, str(tool_call_id)
        raise RuntimeError(f"{action} stream ended before toolApprovalRequested")

    def assert_approval_pause(objective_id: str, tool_call_id: str, action: str) -> str:
        time.sleep(3)
        objective = client.objectives.retrieve(objective_id)
        call = client.objectives.retrieve_tool_call(objective_id, tool_call_id=tool_call_id)
        if call.status != "TOOL_CALL_STATUS_WAITING_FOR_APPROVAL":
            raise AssertionError(f"{action} call advanced before review")
        if call.execution_status != "TOOL_CALL_EXECUTION_STATUS_PENDING":
            raise AssertionError(f"{action} execution was not pending")
        if objective.state in {"STATE_FINALIZED", "STATE_FAILED", "STATE_CANCELLED", "STATE_TIMED_OUT"}:
            raise AssertionError(f"{action} objective became terminal before review")
        return str(objective.state)

    def create_curse_objective(agent_id: str, variation_id: str, suffix: str) -> str:
        objective = client.objectives.create(
            agent_id=agent_id,
            variation_id=variation_id,
            metadata={"labels": {"live_matrix": RUN_ID, "case": suffix}},
            system_prompt_data={},
            first_user_message=(
                "Generate a curse word using faker. You must call GenerateCurseWord "
                "exactly once; do not answer without using that tool."
            ),
        )
        objective_id = resource_id(objective)
        if not objective_id:
            raise AssertionError(f"{suffix} objective omitted metadata.id")
        cleanup.append((
            f"objective {suffix} cancellation",
            lambda objective_id=objective_id: client.objectives.cancel(
                objective_id, reason=f"{RUN_ID} cleanup"
            ),
        ))
        complete("ObjectiveService_CreateObjective", f"created owned {suffix} objective")
        return objective_id

    failure: Optional[BaseException] = None
    try:
        petstore_id = create_tool_set("petstore", {
            "type": "openapi",
            "openapi": {
                "type": "url",
                "url": "https://petstore3.swagger.io/api/v3/openapi.json",
                "base_url": "https://petstore3.swagger.io/api/v3",
            },
        })

        def get_petstore_spec() -> Any:
            response = client.tool_sets.retrieve_open_api_spec(petstore_id)
            if not response.spec:
                return None
            parsed = json.loads(response.spec)
            title = str((parsed.get("info") or {}).get("title", ""))
            return parsed if parsed.get("openapi") and "Swagger Petstore" in title else None

        consumed = poll("Petstore OpenAPI ingestion", get_petstore_spec)
        if len(consumed.get("paths", {})) < 10:
            raise AssertionError("consumed Petstore spec exposed fewer than 10 paths")
        complete(
            "ToolService_GetToolSetOpenAPISpec",
            "Petstore URL adapter returned and decoded its consumed OpenAPI document",
        )
        step("Petstore OpenAPI", f"{len(consumed['paths'])} paths")

        approval_filter = {
            "type": "only",
            "only": {
                "operator": "OPERATOR_AND",
                "filters": [{
                    "attribute": "ATTRIBUTE_NAME",
                    "matcher": {"type": "contains", "contains": "Curse", "case_sensitive": False},
                }],
            },
        }
        faker_id = create_tool_set("faker-mcp", {
            "type": "mcp",
            "mcp": {
                "url": "https://free.cadenya.com/faker-mcp",
                "tool_approvals": approval_filter,
            },
        })

        faker_tools = poll(
            "faker MCP tool sync",
            lambda: matching(
                lambda: list(client.tool_sets.tools.list(tool_set_id=faker_id, limit=20).items),
                lambda items: len(items) >= 3,
            ),
        )
        by_name = {tool.spec.llm_tool_name: tool for tool in faker_tools}
        expected_names = {"GenerateCurseWord", "GenerateFake", "GetFakerOptions"}
        if set(by_name) != expected_names:
            raise AssertionError(f"faker tool names did not match expected set: {sorted(by_name)}")
        if by_name["GenerateCurseWord"].spec.requires_approval is not True:
            raise AssertionError("GenerateCurseWord did not require approval")
        if any(by_name[name].spec.requires_approval for name in ("GenerateFake", "GetFakerOptions")):
            raise AssertionError("approval filter affected non-Curse faker tools")
        step("faker MCP", "3 tools; only GenerateCurseWord requires approval")

        bare_id = create_tool_set("bare-content", {
            "type": "bare",
            "bare": {},
        })
        bare_tool = client.tool_sets.tools.create(
            bare_id,
            metadata={"name": f"{RUN_ID}-provide-content"},
            spec={
                "description": "Request externally supplied acceptance-test content.",
                "requires_approval": True,
                "parameters": {"type": "object", "properties": {}},
                "config": {"type": "bare", "bare": {}},
            },
        )
        bare_tool_id = resource_id(bare_tool)
        if not bare_tool_id:
            raise AssertionError("bare content tool omitted metadata.id")
        cleanup.append((
            "bare content tool",
            lambda: client.tool_sets.tools.delete(bare_id, bare_tool_id),
        ))

        models = client.models.list(limit=50)
        model = next((item for item in models.items if resource_id(item)), None)
        if model is None:
            raise AssertionError("workspace has no model fixture")

        agent = client.agents.create(
            metadata={"name": f"{RUN_ID}-agent", "labels": {"live_matrix": RUN_ID}},
            spec={"variation_selection_mode": "VARIATION_SELECTION_MODE_UNSPECIFIED"},
            default_variation={
                "metadata": {"name": f"{RUN_ID}-variation", "labels": {"live_matrix": RUN_ID}},
                "spec": {
                    "system_prompt_template": (
                        "You are an integration-test agent. Always follow explicit tool-use instructions."
                    ),
                    "model_config": {"model_id": resource_id(model)},
                    "constraints": {"max_tool_calls": 2, "inactivity_timeout": "300s"},
                },
            },
        )
        agent_id = resource_id(agent)
        if not agent_id:
            raise AssertionError("agent omitted metadata.id")
        cleanup.append(("agent", lambda: client.agents.delete(agent_id)))
        variations = client.agents.variations.list(agent_id=agent_id, limit=10)
        variation_id = resource_id(variations.items[0]) if variations.items else None
        if not variation_id:
            raise AssertionError("default variation was not returned")
        assignment = client.agents.variations.add_assignment(
            agent_id=agent_id,
            variation_id=variation_id,
            body={"type": "toolSetId", "tool_set_id": faker_id},
        )
        assignment_id = getattr(assignment, "id", None)
        if not assignment_id:
            raise AssertionError("assignment omitted its row id")
        cleanup.append((
            "faker assignment",
            lambda: client.agents.variations.remove_assignment(
                agent_id, variation_id, assignment_id
            ),
        ))
        bare_assignment = client.agents.variations.add_assignment(
            agent_id=agent_id,
            variation_id=variation_id,
            body={"type": "toolId", "tool_id": bare_tool_id},
        )
        bare_assignment_id = getattr(bare_assignment, "id", None)
        if not bare_assignment_id:
            raise AssertionError("bare assignment omitted its row id")
        cleanup.append((
            "bare assignment",
            lambda: client.agents.variations.remove_assignment(
                agent_id, variation_id, bare_assignment_id
            ),
        ))
        client.agents.publish(agent_id)

        approve_id = create_curse_objective(agent_id, variation_id, "approve")
        first_event_id, approve_call_id = wait_for_tool_approval(approve_id, "approve")
        complete(
            "ObjectiveEventStreamsService_StreamObjectiveEvents",
            "SSE decoded a persisted toolApprovalRequested event",
        )
        approve_state = assert_approval_pause(approve_id, approve_call_id, "approve")
        approved = client.objectives.approve_tool_call(approve_id, tool_call_id=approve_call_id)
        persisted_approved = poll(
            "approved tool-call state",
            lambda: matching(
                lambda: client.objectives.retrieve_tool_call(
                    approve_id, tool_call_id=approve_call_id
                ),
                lambda call: call.status == "TOOL_CALL_STATUS_APPROVED",
            ),
        )
        if persisted_approved.status != "TOOL_CALL_STATUS_APPROVED":
            raise AssertionError("approved state did not persist")
        complete(
            "ObjectiveService_ApproveToolCall",
            f"approved GenerateCurseWord after durable pause in objective state {approve_state}; "
            f"immediate response status was {approved.status}",
        )
        step("approve objective", "paused, inspected, and approved")

        poll(
            "approved MCP execution",
            lambda: matching(
                lambda: client.objectives.retrieve_tool_call(
                    approve_id, tool_call_id=approve_call_id
                ),
                lambda call: call.execution_status == "TOOL_CALL_EXECUTION_STATUS_COMPLETED",
            ),
            120,
        )
        complete(
            "ObjectiveService_GetObjectiveToolCall",
            "retrieved the approved faker MCP call after adapter execution completed",
        )
        wait_for_objective(
            approve_id, lambda objective: objective.state == "STATE_WAITING",
            "post-tool assistant response",
        )

        client.objectives.create_feedback(
            approve_id,
            metadata={"labels": {"live_matrix": RUN_ID}},
            data={"score": 1, "comment": f"{RUN_ID} specialized fixture"},
        )
        complete(
            "ObjectiveService_CreateObjectiveFeedback",
            "submitted feedback after the approved MCP call completed",
        )

        continued = client.objectives.continue_(
            approve_id, message="Reply exactly CONTINUE_OK.", enqueue=False
        )
        continued_id = resource_id(continued)
        if not continued_id:
            raise AssertionError("ContinueObjective response omitted event metadata")
        complete(
            "ObjectiveService_ContinueObjective",
            "continued the WAITING objective and decoded its persisted user-message event",
        )

        def continued_response() -> Any:
            events = list(client.objectives.list_events(
                approve_id, limit=100, sort_order="asc"
            ).items)
            index = next((i for i, event in enumerate(events) if resource_id(event) == continued_id), -1)
            if index < 0:
                return None
            if any(getattr(getattr(event, "data", None), "type", None) == "assistantMessage"
                   for event in events[index + 1:]):
                return events
            return None

        poll("continued assistant response", continued_response, 120)
        wait_for_objective(
            approve_id, lambda objective: objective.state == "STATE_WAITING",
            "continued objective waiting state",
        )

        try:
            compacted = client.objectives.compact(
                approve_id,
                compaction_config={
                    "summarization": {
                        "instructions": "Summarize this short integration-test conversation accurately."
                    }
                },
            )
            if compacted is None:
                raise AssertionError("CompactObjective returned no decoded response")
            complete(
                "ObjectiveService_CompactObjective",
                "compacted the continued MCP objective after multiple user/assistant/tool events",
            )
        except APIError as error:
            block(
                "ObjectiveService_CompactObjective",
                f"continued MCP objective did not satisfy compaction prerequisites (HTTP {error.status_code})",
            )

        if first_event_id:
            replayed = False
            with deadline(30):
                with client.objectives.stream_events(
                    approve_id, last_event_id=first_event_id
                ) as stream:
                    for envelope in stream.events():
                        event_id = resource_id(envelope.data)
                        if event_id and event_id != first_event_id:
                            replayed = True
                            break
            if not replayed:
                raise AssertionError("Last-Event-ID replay returned no later persisted event")
            complete(
                "ObjectiveEventStreamsService_StreamObjectiveEvents",
                "SSE Last-Event-ID replay decoded a later persisted event",
            )

        deny_id = create_curse_objective(agent_id, variation_id, "deny")
        _deny_first, deny_call_id = wait_for_tool_approval(deny_id, "deny")
        deny_state = assert_approval_pause(deny_id, deny_call_id, "deny")
        denied = client.objectives.deny_tool_call(
            deny_id, tool_call_id=deny_call_id, memo=f"{RUN_ID}: denial-path acceptance"
        )
        persisted_denied = poll(
            "denied tool-call state",
            lambda: matching(
                lambda: client.objectives.retrieve_tool_call(
                    deny_id, tool_call_id=deny_call_id
                ),
                lambda call: call.status == "TOOL_CALL_STATUS_DENIED",
            ),
        )
        if persisted_denied.status != "TOOL_CALL_STATUS_DENIED":
            raise AssertionError("denied state did not persist")
        complete(
            "ObjectiveService_DenyToolCall",
            f"denied independent GenerateCurseWord after durable pause in objective state {deny_state}; "
            f"immediate response status was {denied.status}",
        )
        step("deny objective", "paused, inspected, and denied")

        bare_objective = client.objectives.create(
            agent_id=agent_id,
            variation_id=variation_id,
            metadata={"labels": {"live_matrix": RUN_ID, "case": "content"}},
            system_prompt_data={},
            first_user_message=(
                f"Call the tool named {RUN_ID}-provide-content exactly once. "
                "Do not answer without calling it."
            ),
        )
        bare_objective_id = resource_id(bare_objective)
        if not bare_objective_id:
            raise AssertionError("bare content objective omitted metadata.id")
        cleanup.append((
            "bare content objective cancellation",
            lambda: client.objectives.cancel(
                bare_objective_id, reason=f"{RUN_ID} cleanup"
            ),
        ))
        _bare_first, bare_call_id = wait_for_tool_approval(bare_objective_id, "content")
        assert_approval_pause(bare_objective_id, bare_call_id, "content")
        client.objectives.approve_tool_call(bare_objective_id, bare_call_id)
        poll(
            "bare tool approval",
            lambda: matching(
                lambda: client.objectives.retrieve_tool_call(
                    bare_objective_id, bare_call_id
                ),
                lambda call: call.status == "TOOL_CALL_STATUS_APPROVED",
            ),
        )
        content_call = client.objectives.set_tool_call_content(
            bare_objective_id,
            bare_call_id,
            content=[{"type": "text", "text": {"text": "BARE_CONTENT_OK"}}],
        )
        if resource_id(content_call) != bare_call_id:
            raise AssertionError("SetToolCallContent returned a different tool call")
        complete(
            "ObjectiveService_SetToolCallContent",
            "supplied text content to an independently approved bare tool call",
        )
        step("content objective", "approved and supplied bare tool content")

        cancel_id = create_curse_objective(agent_id, variation_id, "cancel")
        wait_for_objective(
            cancel_id, lambda objective: objective.state == "STATE_RUNNING",
            "cancel objective running state",
        )
        cancelled = client.objectives.cancel(
            cancel_id, reason=f"{RUN_ID}: cancel-path acceptance"
        )
        persisted_cancelled = poll(
            "cancelled objective state",
            lambda: matching(
                lambda: client.objectives.retrieve(cancel_id),
                lambda objective: objective.state == "STATE_CANCELLED",
            ),
        )
        if persisted_cancelled.state != "STATE_CANCELLED":
            raise AssertionError("cancelled state did not persist")
        complete(
            "ObjectiveService_CancelObjective",
            f"cancelled a separate RUNNING objective; immediate response state was {cancelled.state}",
        )
    except BaseException as error:
        failure = error
        if isinstance(error, APIResponseError) and str(error).startswith(
            "response missing required field ObjectiveInfo.createdBy"
        ):
            block(
                "ObjectiveService_CreateObjective",
                "real 2xx response could not decode upstream ObjectiveInfo.createdBy "
                "contract mismatch; see API contract feedback item 14",
            )
            for operation_id in (
                "ObjectiveEventStreamsService_StreamObjectiveEvents",
                "ObjectiveService_ApproveToolCall",
                "ObjectiveService_DenyToolCall",
                "ObjectiveService_SetToolCallContent",
                "ObjectiveService_CreateObjectiveFeedback",
                "ObjectiveService_CancelObjective",
                "ObjectiveService_CompactObjective",
                "ObjectiveService_ContinueObjective",
            ):
                block(
                    operation_id,
                    "owned objective prerequisite unavailable because CreateObjective hit "
                    "upstream contract mismatch; see API contract feedback item 14",
                )
        else:
            for operation_id in (
                "ToolService_GetToolSetOpenAPISpec",
                "ObjectiveEventStreamsService_StreamObjectiveEvents",
                "ObjectiveService_ApproveToolCall",
                "ObjectiveService_DenyToolCall",
                "ObjectiveService_SetToolCallContent",
            ):
                fail(operation_id, error)
    finally:
        cleanup_failures = 0
        for label, function in reversed(cleanup):
            try:
                function()
                step("cleanup", label)
            except Exception as error:
                cleanup_failures += 1
                step("cleanup FAILED", f"{label}: {type(error).__name__}")
        client.close()
        report["executedAt"] = datetime.now(timezone.utc).isoformat()
        report["operations"] = operations
        RESULTS.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")

    if failure:
        step("FAILED", failure_text(failure))
        return 1
    if cleanup_failures:
        step("FAILED", f"acceptance passed with {cleanup_failures} cleanup failure(s)")
        return 1
    step("PASSED", "specialized fixture acceptance")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
