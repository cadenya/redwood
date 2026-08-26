#!/usr/bin/env ruby
# frozen_string_literal: true

# Opt-in real-API MCP/Petstore acceptance flow for the installed Ruby SDK.

require "cadenya"
require "json"
require "time"
require "timeout"

HERE = File.expand_path(__dir__)
ROOT_ENV = File.expand_path("../../.env.development", HERE)
RESULTS = File.join(HERE, "results-ruby.json")
RUN_ID = "specialized-rb-#{(Time.now.to_f * 1000).to_i.to_s(16)}"
OPT_IN = "--run-specialized-live-fixtures"

unless ARGV == [OPT_IN] && ENV["CADENYA_LIVE_SPECIALIZED_FIXTURES"] == "ruby"
  warn "refusing: pass #{OPT_IN} and set CADENYA_LIVE_SPECIALIZED_FIXTURES=ruby"
  exit 2
end

root_env = {}
File.readlines(ROOT_ENV, chomp: true).each do |line|
  stripped = line.strip
  next if stripped.empty? || stripped.start_with?("#")

  stripped = stripped.delete_prefix("export ") if stripped.start_with?("export ")
  key, value = stripped.split("=", 2)
  next unless value && key.start_with?("CADENYA_")

  value = value.strip
  value = value[1...-1] if value.length >= 2 && ["'", '"'].include?(value[0]) && value[-1] == value[0]
  root_env[key] = value
end
ambient_token = ENV.fetch("CADENYA_API_KEY", "")
raise "CADENYA_API_KEY is required" if ambient_token.empty?

# The caller supplies the authorized token in memory. Load only non-secret
# workspace/base context from the root env; never replace or persist it.
root_env.each { |key, value| ENV[key] = value unless key == "CADENYA_API_KEY" }
client = Cadenya::Client.new(
  api_key: ambient_token,
  base_url: root_env["CADENYA_BASE_URL"],
  workspace_id: root_env["CADENYA_WORKSPACE_ID"],
  max_retries: 1
)
report = JSON.parse(File.read(RESULTS))
loaded_client = $LOADED_FEATURES.find { |path| path.end_with?("/cadenya/client.rb") }
report["installedArtifact"] = File.realpath(loaded_client) if loaded_client
operations = report.fetch("operations")
cleanup = []

resource_id = lambda do |value|
  metadata_id = value&.respond_to?(:metadata) ? value.metadata&.id : nil
  direct_id = value&.respond_to?(:id) ? value.id : nil
  (metadata_id || direct_id)&.to_s
end

step = lambda do |label, detail = ""|
  puts "#{label.ljust(24)} #{detail}".rstrip
  $stdout.flush
end

complete = lambda do |operation_id, evidence|
  operations[operation_id] = {
    "status" => "completed",
    "evidence" => "real api.cadenya.com: installed Ruby gem specialized fixture succeeded; #{evidence}",
  }
end

block = lambda do |operation_id, evidence|
  next if operations.dig(operation_id, "status") == "completed"

  operations[operation_id] = {
    "status" => "blocked",
    "evidence" => "real api.cadenya.com: installed Ruby gem; #{evidence}",
  }
end

failure_text = lambda do |error|
  if error.is_a?(Cadenya::APIError)
    "APIError HTTP #{error.status_code}; response body not retained"
  else
    "#{error.class}; response body not retained"
  end
end

fail_operation = lambda do |operation_id, error|
  next if operations.dig(operation_id, "status") == "completed"

  operations[operation_id] = {
    "status" => "failed",
    "evidence" => "real installed Ruby gem specialized fixture failed: #{failure_text.call(error)}",
  }
end

poll = lambda do |label, seconds: 90, &callback|
  finish = Process.clock_gettime(Process::CLOCK_MONOTONIC) + seconds
  last_error = nil
  while Process.clock_gettime(Process::CLOCK_MONOTONIC) < finish
    begin
      value = callback.call
      break value if value
    rescue StandardError => error
      last_error = error
    end
    sleep 2
  end || raise(Timeout::Error, "#{label} did not become ready#{last_error ? " (#{last_error.class})" : ""}")
end

create_tool_set = lambda do |name, adapter|
  tool_set = client.tool_sets.create(
    metadata: { name: "#{RUN_ID}-#{name}", labels: { live_matrix: RUN_ID } },
    spec: { description: "specialized live fixture: #{name}", adapter: adapter }
  )
  tool_set_id = resource_id.call(tool_set)
  raise "#{name} tool set omitted metadata.id" if tool_set_id.to_s.empty?

  cleanup << ["tool set #{name}", lambda do
    begin
      client.tool_sets.archive(tool_set_id)
    rescue StandardError
      nil
    end
    client.tool_sets.delete(tool_set_id)
  end]
  tool_set_id
end

wait_for_objective = lambda do |objective_id, predicate, label|
  poll.call(label) do
    objective = client.objectives.retrieve(objective_id)
    predicate.call(objective) ? objective : nil
  end
end

initial_checkpoint = lambda do |objective_id|
  page = client.objectives.list_events(objective_id, limit: 100, sort_order: "asc")
  page.items.map { |event| resource_id.call(event) }.compact.first
end

wait_for_tool_approval = lambda do |objective_id, action|
  checkpoint = initial_checkpoint.call(objective_id)
  first_event_id = checkpoint
  result = nil
  Timeout.timeout(120) do
    stream = client.objectives.stream_events(objective_id, last_event_id: checkpoint)
    stream.each_event do |envelope|
      event = envelope.data
      event_id = resource_id.call(event)
      first_event_id ||= event_id
      data = event&.data
      next unless data&.type == "toolApprovalRequested"

      tool_call_id = data.tool_approval_requested&.tool_call_id
      raise "#{action} approval event omitted toolCallId" if tool_call_id.to_s.empty?

      call = client.objectives.retrieve_tool_call(objective_id, tool_call_id)
      raise "#{action} call was not waiting for approval" unless call.status == "TOOL_CALL_STATUS_WAITING_FOR_APPROVAL"

      result = [first_event_id, tool_call_id.to_s]
      break
    end
  end
  raise "#{action} stream ended before toolApprovalRequested" unless result

  result
end

assert_approval_pause = lambda do |objective_id, tool_call_id, action|
  sleep 3
  objective = client.objectives.retrieve(objective_id)
  call = client.objectives.retrieve_tool_call(objective_id, tool_call_id)
  raise "#{action} call advanced before review" unless call.status == "TOOL_CALL_STATUS_WAITING_FOR_APPROVAL"
  unless call.execution_status == "TOOL_CALL_EXECUTION_STATUS_PENDING"
    raise "#{action} execution was not pending"
  end
  if %w[STATE_FINALIZED STATE_FAILED STATE_CANCELLED STATE_TIMED_OUT].include?(objective.state)
    raise "#{action} objective became terminal before review"
  end
  objective.state.to_s
end

create_curse_objective = lambda do |agent_id, variation_id, suffix|
  objective = client.objectives.create(
    agent_id: agent_id,
    variation_id: variation_id,
    metadata: { labels: { live_matrix: RUN_ID, case: suffix } },
    system_prompt_data: {},
    first_user_message: (
      "Generate a curse word using faker. You must call GenerateCurseWord " \
      "exactly once; do not answer without using that tool."
    )
  )
  objective_id = resource_id.call(objective)
  raise "#{suffix} objective omitted metadata.id" if objective_id.to_s.empty?

  cleanup << ["objective #{suffix} cancellation", ->(id = objective_id) do
    client.objectives.cancel(id, reason: "#{RUN_ID} cleanup")
  end]
  complete.call("ObjectiveService_CreateObjective", "created owned #{suffix} objective")
  objective_id
end

failure = nil
cleanup_failures = 0
begin
  petstore_id = create_tool_set.call("petstore", {
    type: "openapi",
    openapi: {
      type: "url",
      url: "https://petstore3.swagger.io/api/v3/openapi.json",
      base_url: "https://petstore3.swagger.io/api/v3",
    },
  })

  consumed = poll.call("Petstore OpenAPI ingestion") do
    response = client.tool_sets.retrieve_open_api_spec(petstore_id)
    next nil if response.spec.to_s.empty?

    parsed = JSON.parse(response.spec)
    title = (parsed["info"] || {})["title"].to_s
    parsed["openapi"] && title.include?("Swagger Petstore") ? parsed : nil
  end
  raise "consumed Petstore spec exposed fewer than 10 paths" if (consumed["paths"] || {}).length < 10

  complete.call(
    "ToolService_GetToolSetOpenAPISpec",
    "Petstore URL adapter returned and decoded its consumed OpenAPI document"
  )
  step.call("Petstore OpenAPI", "#{consumed.fetch("paths").length} paths")

  approval_filter = {
    type: "only",
    only: {
      operator: "OPERATOR_AND",
      filters: [{
        attribute: "ATTRIBUTE_NAME",
        matcher: { type: "contains", contains: "Curse", case_sensitive: false },
      }],
    },
  }
  faker_id = create_tool_set.call("faker-mcp", {
    type: "mcp",
    mcp: {
      url: "https://free.cadenya.com/faker-mcp",
      tool_approvals: approval_filter,
    },
  })

  faker_tools = poll.call("faker MCP tool sync") do
    items = client.tool_sets.tools.list(faker_id, limit: 20).items
    items.length >= 3 ? items : nil
  end
  by_name = faker_tools.each_with_object({}) { |tool, map| map[tool.spec.llm_tool_name] = tool }
  expected_names = %w[GenerateCurseWord GenerateFake GetFakerOptions].sort
  raise "faker tool names did not match expected set" unless by_name.keys.sort == expected_names
  unless by_name.fetch("GenerateCurseWord").spec.requires_approval == true
    raise "GenerateCurseWord did not require approval"
  end
  if %w[GenerateFake GetFakerOptions].any? { |name| by_name.fetch(name).spec.requires_approval }
    raise "approval filter affected non-Curse faker tools"
  end
  step.call("faker MCP", "3 tools; only GenerateCurseWord requires approval")

  bare_id = create_tool_set.call("bare-content", {
    type: "bare",
    bare: {},
  })
  bare_tool = client.tool_sets.tools.create(
    bare_id,
    metadata: { name: "#{RUN_ID}-provide-content" },
    spec: {
      description: "Request externally supplied acceptance-test content.",
      requires_approval: true,
      parameters: { type: "object", properties: {} },
      config: { type: "bare", bare: {} },
    }
  )
  bare_tool_id = resource_id.call(bare_tool)
  raise "bare content tool omitted metadata.id" if bare_tool_id.to_s.empty?
  cleanup << ["bare content tool", -> { client.tool_sets.tools.delete(bare_id, bare_tool_id) }]

  models = client.models.list(limit: 50)
  model = models.items.find { |candidate| resource_id.call(candidate) }
  raise "workspace has no model fixture" unless model

  agent = client.agents.create(
    metadata: { name: "#{RUN_ID}-agent", labels: { live_matrix: RUN_ID } },
    spec: { variation_selection_mode: "VARIATION_SELECTION_MODE_UNSPECIFIED" },
    default_variation: {
      metadata: { name: "#{RUN_ID}-variation", labels: { live_matrix: RUN_ID } },
      spec: {
        system_prompt_template: (
          "You are an integration-test agent. Always follow explicit tool-use instructions."
        ),
        model_config: { model_id: resource_id.call(model) },
        constraints: { max_tool_calls: 2, inactivity_timeout: "300s" },
      },
    }
  )
  agent_id = resource_id.call(agent)
  raise "agent omitted metadata.id" if agent_id.to_s.empty?

  cleanup << ["agent", -> { client.agents.delete(agent_id) }]
  variations = client.agents.variations.list(agent_id, limit: 10)
  variation_id = variations.items.empty? ? nil : resource_id.call(variations.items.first)
  raise "default variation was not returned" if variation_id.to_s.empty?

  assignment = client.agents.variations.add_assignment(
    agent_id,
    variation_id,
    body: { type: "toolSetId", tool_set_id: faker_id }
  )
  assignment_id = assignment.id
  raise "assignment omitted its row id" if assignment_id.to_s.empty?

  cleanup << ["faker assignment", lambda do
    client.agents.variations.remove_assignment(
      agent_id, variation_id, assignment_id
    )
  end]
  bare_assignment = client.agents.variations.add_assignment(
    agent_id,
    variation_id,
    body: { type: "toolId", tool_id: bare_tool_id }
  )
  bare_assignment_id = bare_assignment.id
  raise "bare assignment omitted its row id" if bare_assignment_id.to_s.empty?
  cleanup << ["bare assignment", lambda do
    client.agents.variations.remove_assignment(
      agent_id, variation_id, bare_assignment_id
    )
  end]
  client.agents.publish(agent_id)

  approve_id = create_curse_objective.call(agent_id, variation_id, "approve")
  first_event_id, approve_call_id = wait_for_tool_approval.call(approve_id, "approve")
  complete.call(
    "ObjectiveEventStreamsService_StreamObjectiveEvents",
    "SSE decoded a persisted toolApprovalRequested event"
  )
  approve_state = assert_approval_pause.call(approve_id, approve_call_id, "approve")
  approved = client.objectives.approve_tool_call(approve_id, approve_call_id)
  persisted_approved = poll.call("approved tool-call state") do
    call = client.objectives.retrieve_tool_call(approve_id, approve_call_id)
    call.status == "TOOL_CALL_STATUS_APPROVED" ? call : nil
  end
  raise "approved state did not persist" unless persisted_approved.status == "TOOL_CALL_STATUS_APPROVED"

  complete.call(
    "ObjectiveService_ApproveToolCall",
    "approved GenerateCurseWord after durable pause in objective state #{approve_state}; " \
    "immediate response status was #{approved.status}"
  )
  step.call("approve objective", "paused, inspected, and approved")

  poll.call("approved MCP execution", seconds: 120) do
    call = client.objectives.retrieve_tool_call(approve_id, approve_call_id)
    call.execution_status == "TOOL_CALL_EXECUTION_STATUS_COMPLETED" ? call : nil
  end
  complete.call(
    "ObjectiveService_GetObjectiveToolCall",
    "retrieved the approved faker MCP call after adapter execution completed"
  )
  wait_for_objective.call(
    approve_id, ->(objective) { objective.state == "STATE_WAITING" },
    "post-tool assistant response"
  )

  client.objectives.create_feedback(
    approve_id,
    metadata: { labels: { live_matrix: RUN_ID } },
    data: { score: 1, comment: "#{RUN_ID} specialized fixture" }
  )
  complete.call(
    "ObjectiveService_CreateObjectiveFeedback",
    "submitted feedback after the approved MCP call completed"
  )

  continued = client.objectives.continue(
    approve_id, message: "Reply exactly CONTINUE_OK.", enqueue: false
  )
  continued_id = resource_id.call(continued)
  raise "ContinueObjective response omitted event metadata" if continued_id.to_s.empty?

  complete.call(
    "ObjectiveService_ContinueObjective",
    "continued the WAITING objective and decoded its persisted user-message event"
  )

  poll.call("continued assistant response", seconds: 120) do
    events = client.objectives.list_events(approve_id, limit: 100, sort_order: "asc").items
    index = events.index { |event| resource_id.call(event) == continued_id }
    next nil unless index

    events[(index + 1)..-1].any? { |event| event.data&.type == "assistantMessage" } ? events : nil
  end
  wait_for_objective.call(
    approve_id, ->(objective) { objective.state == "STATE_WAITING" },
    "continued objective waiting state"
  )

  begin
    compacted = client.objectives.compact(
      approve_id,
      compaction_config: {
        summarization: {
          instructions: "Summarize this short integration-test conversation accurately.",
        },
      }
    )
    raise "CompactObjective returned no decoded response" unless compacted

    complete.call(
      "ObjectiveService_CompactObjective",
      "compacted the continued MCP objective after multiple user/assistant/tool events"
    )
  rescue Cadenya::APIError => error
    block.call(
      "ObjectiveService_CompactObjective",
      "continued MCP objective did not satisfy compaction prerequisites (HTTP #{error.status_code})"
    )
  end

  if first_event_id
    replayed = false
    Timeout.timeout(30) do
      stream = client.objectives.stream_events(approve_id, last_event_id: first_event_id)
      stream.each_event do |envelope|
        event_id = resource_id.call(envelope.data)
        if event_id && event_id != first_event_id
          replayed = true
          break
        end
      end
    end
    raise "Last-Event-ID replay returned no later persisted event" unless replayed

    complete.call(
      "ObjectiveEventStreamsService_StreamObjectiveEvents",
      "SSE Last-Event-ID replay decoded a later persisted event"
    )
  end

  deny_id = create_curse_objective.call(agent_id, variation_id, "deny")
  _deny_first, deny_call_id = wait_for_tool_approval.call(deny_id, "deny")
  deny_state = assert_approval_pause.call(deny_id, deny_call_id, "deny")
  denied = client.objectives.deny_tool_call(
    deny_id, deny_call_id, memo: "#{RUN_ID}: denial-path acceptance"
  )
  persisted_denied = poll.call("denied tool-call state") do
    call = client.objectives.retrieve_tool_call(deny_id, deny_call_id)
    call.status == "TOOL_CALL_STATUS_DENIED" ? call : nil
  end
  raise "denied state did not persist" unless persisted_denied.status == "TOOL_CALL_STATUS_DENIED"

  complete.call(
    "ObjectiveService_DenyToolCall",
    "denied independent GenerateCurseWord after durable pause in objective state #{deny_state}; " \
    "immediate response status was #{denied.status}"
  )
  step.call("deny objective", "paused, inspected, and denied")

  bare_objective = client.objectives.create(
    agent_id: agent_id,
    variation_id: variation_id,
    metadata: { labels: { live_matrix: RUN_ID, case: "content" } },
    system_prompt_data: {},
    first_user_message: (
      "Call the tool named #{RUN_ID}-provide-content exactly once. " \
      "Do not answer without calling it."
    )
  )
  bare_objective_id = resource_id.call(bare_objective)
  raise "bare content objective omitted metadata.id" if bare_objective_id.to_s.empty?
  cleanup << ["bare content objective cancellation", -> do
    client.objectives.cancel(bare_objective_id, reason: "#{RUN_ID} cleanup")
  end]
  _bare_first, bare_call_id = wait_for_tool_approval.call(bare_objective_id, "content")
  assert_approval_pause.call(bare_objective_id, bare_call_id, "content")
  client.objectives.approve_tool_call(bare_objective_id, bare_call_id)
  poll.call("bare tool approval") do
    call = client.objectives.retrieve_tool_call(bare_objective_id, bare_call_id)
    call.status == "TOOL_CALL_STATUS_APPROVED" ? call : nil
  end
  content_call = client.objectives.set_tool_call_content(
    bare_objective_id,
    bare_call_id,
    content: [{ type: "text", text: { text: "BARE_CONTENT_OK" } }]
  )
  unless resource_id.call(content_call) == bare_call_id
    raise "SetToolCallContent returned a different tool call"
  end
  complete.call(
    "ObjectiveService_SetToolCallContent",
    "supplied text content to an independently approved bare tool call"
  )
  step.call("content objective", "approved and supplied bare tool content")

  cancel_id = create_curse_objective.call(agent_id, variation_id, "cancel")
  wait_for_objective.call(
    cancel_id, ->(objective) { objective.state == "STATE_RUNNING" },
    "cancel objective running state"
  )
  cancelled = client.objectives.cancel(
    cancel_id, reason: "#{RUN_ID}: cancel-path acceptance"
  )
  persisted_cancelled = poll.call("cancelled objective state") do
    objective = client.objectives.retrieve(cancel_id)
    objective.state == "STATE_CANCELLED" ? objective : nil
  end
  raise "cancelled state did not persist" unless persisted_cancelled.state == "STATE_CANCELLED"

  complete.call(
    "ObjectiveService_CancelObjective",
    "cancelled a separate RUNNING objective; immediate response state was #{cancelled.state}"
  )
rescue StandardError => error
  failure = error
  %w[
    ToolService_GetToolSetOpenAPISpec
    ObjectiveEventStreamsService_StreamObjectiveEvents
    ObjectiveService_ApproveToolCall
    ObjectiveService_DenyToolCall
    ObjectiveService_SetToolCallContent
  ].each { |operation_id| fail_operation.call(operation_id, error) }
ensure
  cleanup.reverse_each do |label, function|
    begin
      function.call
      step.call("cleanup", label)
    rescue StandardError => error
      cleanup_failures += 1
      step.call("cleanup FAILED", "#{label}: #{error.class}")
    end
  end
  report["executedAt"] = Time.now.utc.iso8601(9)
  report["operations"] = operations
  File.write(RESULTS, "#{JSON.pretty_generate(report)}\n")
end

if failure
  step.call("FAILED", failure_text.call(failure))
  exit 1
elsif cleanup_failures.positive?
  step.call("FAILED", "acceptance passed with #{cleanup_failures} cleanup failure(s)")
  exit 1
else
  step.call("PASSED", "specialized fixture acceptance")
end
