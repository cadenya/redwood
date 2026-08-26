#!/usr/bin/env ruby
# frozen_string_literal: true

# Authorized preproduction mutation wave for the installed Ruby gem. All
# resources are uniquely named and removed in reverse order. Evidence never
# includes response bodies, tokens, credentials, or secret values.

require "cadenya"
require "json"
require "time"

here = File.expand_path(__dir__)
results_path = File.join(here, "results-ruby.json")
run_name = "sdk-matrix-ruby-#{Time.now.to_i}"
prior = File.exist?(results_path) ? JSON.parse(File.read(results_path)) : {
  "schemaVersion" => 1, "sdk" => "ruby", "operations" => {},
}
operations = prior.fetch("operations")
client = Cadenya::Client.new
cleanup = []

record = lambda do |operation_id, status, detail|
  operations[operation_id] = { "status" => status, "evidence" => detail }
end

call = lambda do |operation_id, &block|
  value = block.call
  record.call(operation_id, "completed", "installed gem; HTTP 2xx; decoded #{value.class.name}")
  value
rescue Cadenya::APIError => error
  record.call(operation_id, "failed", "installed gem; APIError HTTP #{error.status_code}; response body not retained")
  nil
rescue StandardError => error
  record.call(operation_id, "failed", "installed gem; #{error.class}; response body not retained")
  nil
end

resource_id = lambda do |value|
  value&.respond_to?(:metadata) ? value.metadata&.id&.to_s : nil
end

begin
  # Workspace API key lifecycle.
  api_key = call.call("APIKeyService_CreateAPIKey") do
    client.api_keys.create(
      metadata: { name: "#{run_name}-api-key" },
      spec: { description: "SDK live matrix disposable key" }
    )
  end
  api_key_id = resource_id.call(api_key)
  if api_key_id
    cleanup << ["api key", -> { client.api_keys.delete(api_key_id) }]
    call.call("APIKeyService_GetAPIKey") { client.api_keys.retrieve(api_key_id) }
    call.call("APIKeyService_UpdateAPIKey") do
      client.api_keys.update(api_key_id, metadata: { name: "#{run_name}-api-key-updated" }, update_mask: "metadata.name")
    end
    call.call("APIKeyService_DisableAPIKey") { client.api_keys.disable(api_key_id) }
    call.call("APIKeyService_EnableAPIKey") { client.api_keys.enable(api_key_id) }
    call.call("APIKeyService_RotateAPIKey") { client.api_keys.rotate(api_key_id) }
    call.call("APIKeyService_DeleteAPIKey") { client.api_keys.delete(api_key_id) }
    cleanup.pop
  end

  # Provider/model mutations are serialized by the root coordinator. This
  # lane leaves those result cells untouched.
  provider_key = nil
  provider_key_id = resource_id.call(provider_key)
  if provider_key_id
    cleanup << ["provider key", -> { client.ai_provider_keys.delete(provider_key_id) }]
    call.call("AIProviderKeyService_GetAIProviderKey") { client.ai_provider_keys.retrieve(provider_key_id) }
    call.call("AIProviderKeyService_UpdateAIProviderKey") do
      client.ai_provider_keys.update(
        provider_key_id, metadata: { name: "#{run_name}-provider-key-updated" }, update_mask: "metadata.name"
      )
    end
    call.call("AIProviderKeyService_DeleteAIProviderKey") { client.ai_provider_keys.delete(provider_key_id) }
    cleanup.pop
  end

  workspace_secret = call.call("WorkspaceSecretService_CreateWorkspaceSecret") do
    client.workspace_secrets.create(
      metadata: { name: "#{run_name}-workspace-secret" }, spec: { value: "#{run_name}-value" }
    )
  end
  workspace_secret_id = resource_id.call(workspace_secret)
  if workspace_secret_id
    cleanup << ["workspace secret", -> { client.workspace_secrets.delete(workspace_secret_id) }]
    call.call("WorkspaceSecretService_GetWorkspaceSecret") { client.workspace_secrets.retrieve(workspace_secret_id) }
    call.call("WorkspaceSecretService_UpdateWorkspaceSecret") do
      client.workspace_secrets.update(workspace_secret_id, spec: { value: "#{run_name}-updated" }, update_mask: "spec.value")
    end
    call.call("WorkspaceSecretService_DeleteWorkspaceSecret") { client.workspace_secrets.delete(workspace_secret_id) }
    cleanup.pop
  end

  layer = call.call("MemoryService_CreateMemoryLayer") do
    client.memory_layers.create(
      metadata: { name: "#{run_name}-memory" },
      spec: { type: "MEMORY_LAYER_TYPE_SKILLS", description: "SDK matrix" }
    )
  end
  layer_id = resource_id.call(layer)
  if layer_id
    cleanup << ["memory layer", -> { client.memory_layers.delete(layer_id) }]
    call.call("MemoryService_GetMemoryLayer") { client.memory_layers.retrieve(layer_id) }
    call.call("MemoryService_ListMemoryEntries") do
      client.memory_layers.entries.list(layer_id, limit: 1)
    end
    call.call("MemoryService_UpdateMemoryLayer") do
      client.memory_layers.update(layer_id, metadata: { name: "#{run_name}-memory-updated" }, update_mask: "metadata.name")
    end
    entry = call.call("MemoryService_CreateMemoryEntry") do
      client.memory_layers.entries.create(
        layer_id, metadata: { name: "#{run_name}-entry" },
        spec: { type: "content", content: "live matrix content", key: "#{run_name}-key" }
      )
    end
    entry_id = resource_id.call(entry)
    if entry_id
      cleanup << ["memory entry", -> { client.memory_layers.entries.delete(layer_id, entry_id) }]
      call.call("MemoryService_GetMemoryEntry") { client.memory_layers.entries.retrieve(layer_id, entry_id) }
      call.call("MemoryService_UpdateMemoryEntry") do
        client.memory_layers.entries.update(
          layer_id, entry_id,
          metadata: { name: "#{run_name}-entry-updated" }, update_mask: "metadata.name"
        )
      end
      call.call("MemoryService_DeleteMemoryEntry") { client.memory_layers.entries.delete(layer_id, entry_id) }
      cleanup.pop
    end
  end

  tool_set = call.call("ToolService_CreateToolSet") do
    client.tool_sets.create(
      metadata: { name: "#{run_name}-tool-set" },
      spec: { description: "SDK matrix", adapter: { type: "bare", bare: {} } }
    )
  end
  tool_set_id = resource_id.call(tool_set)
  tool_id = nil
  if tool_set_id
    cleanup << ["tool set", -> { client.tool_sets.delete(tool_set_id) }]
    call.call("ToolService_GetToolSet") { client.tool_sets.retrieve(tool_set_id) }
    call.call("ToolService_UpdateToolSet") do
      client.tool_sets.update(tool_set_id, metadata: { name: "#{run_name}-tool-set-updated" }, update_mask: "metadata.name")
    end
    call.call("ToolService_ArchiveToolSet") { client.tool_sets.archive(tool_set_id) }
    call.call("ToolService_UnarchiveToolSet") { client.tool_sets.unarchive(tool_set_id) }

    secret = call.call("ToolService_CreateToolSetSecret") do
      client.tool_sets.secrets.create(
        tool_set_id, metadata: { name: "#{run_name}-tool-secret" }, spec: { value: "#{run_name}-value" }
      )
    end
    secret_id = resource_id.call(secret)
    if secret_id
      cleanup << ["tool secret", -> { client.tool_sets.secrets.delete(tool_set_id, secret_id) }]
      call.call("ToolService_GetToolSetSecret") { client.tool_sets.secrets.retrieve(tool_set_id, secret_id) }
      call.call("ToolService_UpdateToolSetSecret") do
        client.tool_sets.secrets.update(
          tool_set_id, secret_id, spec: { value: "#{run_name}-updated" }, update_mask: "spec.value"
        )
      end
      call.call("ToolService_DeleteToolSetSecret") { client.tool_sets.secrets.delete(tool_set_id, secret_id) }
      cleanup.pop
    end

    tool = call.call("ToolService_CreateTool") do
      client.tool_sets.tools.create(
        tool_set_id, metadata: { name: "#{run_name}-tool" },
        spec: {
          description: "SDK matrix bare tool", requires_approval: false,
          parameters: { type: "object", properties: {} }, config: { type: "bare", bare: {} },
        }
      )
    end
    tool_id = resource_id.call(tool)
    if tool_id
      cleanup << ["tool", -> { client.tool_sets.tools.delete(tool_set_id, tool_id) }]
      call.call("ToolService_GetTool") { client.tool_sets.tools.retrieve(tool_set_id, tool_id) }
      call.call("ToolService_UpdateTool") do
        client.tool_sets.tools.update(
          tool_set_id, tool_id,
          metadata: { name: "#{run_name}-tool-updated" }, update_mask: "metadata.name"
        )
      end
      call.call("ToolService_OmitTool") { client.tool_sets.tools.omit(tool_set_id, tool_id) }
      call.call("ToolService_RestoreTool") { client.tool_sets.tools.restore(tool_set_id, tool_id) }
    end
  end

  # Read-only fixture selection; root owns the model result cells.
  model_page = begin
    client.models.list(limit: 1)
  rescue StandardError
    nil
  end
  model_id = model_page&.items&.first&.metadata&.id
  agent = if model_id
            call.call("AgentService_CreateAgent") do
              client.agents.create(
                metadata: { name: "#{run_name}-agent" },
                spec: { variation_selection_mode: "VARIATION_SELECTION_MODE_UNSPECIFIED" },
                default_variation: {
                  metadata: { name: "#{run_name}-default" },
                  spec: { system_prompt_template: "Reply concisely.", model_config: { model_id: model_id } },
                }
              )
            end
          end
  agent_id = resource_id.call(agent)
  variation_id = nil
  if agent_id
    cleanup << ["agent", -> { client.agents.delete(agent_id) }]
    call.call("AgentService_GetAgent") { client.agents.retrieve(agent_id) }
    call.call("AgentService_UpdateAgent") do
      client.agents.update(agent_id, metadata: { name: "#{run_name}-agent-updated" }, update_mask: "metadata.name")
    end
    call.call("AgentService_ArchiveAgent") { client.agents.archive(agent_id) }
    call.call("AgentService_UnarchiveAgent") { client.agents.unarchive(agent_id) }
    call.call("AgentService_PublishAgent") { client.agents.publish(agent_id) }
    call.call("AgentService_UnpublishAgent") { client.agents.unpublish(agent_id) }

      variation = call.call("AgentVariationService_CreateAgentVariation") do
        client.agents.variations.create(
          agent_id, metadata: { name: "#{run_name}-variation" },
        spec: { system_prompt_template: "Reply concisely.", model_config: { model_id: model_id } }
      )
    end
    variation_id = resource_id.call(variation)
    if variation_id
      cleanup << ["variation", -> { client.agents.variations.delete(agent_id, variation_id) }]
      call.call("AgentVariationService_GetAgentVariation") do
        client.agents.variations.retrieve(agent_id, variation_id)
      end
      call.call("AgentVariationService_UpdateAgentVariation") do
        client.agents.variations.update(
          agent_id, variation_id,
          metadata: { name: "#{run_name}-variation-updated" }, update_mask: "metadata.name"
        )
      end
      if tool_id
          assignment = call.call("AgentVariationService_AddAgentVariationAssignment") do
            client.agents.variations.add_assignment(
              agent_id, variation_id, body: { type: "toolId", tool_id: tool_id }
          )
        end
        if assignment&.id
          call.call("AgentVariationService_RemoveAgentVariationAssignment") do
            client.agents.variations.remove_assignment(agent_id, variation_id, assignment.id)
          end
        end
      end
      if layer_id
        memory_assignment = call.call("AgentVariationService_AddAgentVariationMemoryLayer") do
          client.agents.variations.add_memory_layer(
            agent_id, variation_id, memory_layer_id: layer_id, position: 0
          )
        end
        if memory_assignment&.id
          call.call("AgentVariationService_UpdateAgentVariationMemoryLayer") do
            client.agents.variations.update_memory_layer(
              agent_id, variation_id, memory_assignment.id, position: 1
            )
          end
          call.call("AgentVariationService_RemoveAgentVariationMemoryLayer") do
            client.agents.variations.remove_memory_layer(
              agent_id, variation_id, memory_assignment.id
            )
          end
        end
      end

      schedule = call.call("AgentScheduleService_CreateAgentSchedule") do
        client.agents.schedules.create(
          agent_id, metadata: { name: "#{run_name}-schedule" },
          spec: {
            schedule: { intervals: [{ every: "86400s" }], timezone: "Etc/UTC" },
            variation_id: variation_id, first_user_message: "Scheduled matrix probe", system_prompt_data: {},
          }
        )
      end
      schedule_id = resource_id.call(schedule)
      if schedule_id
        cleanup << ["schedule", -> { client.agents.schedules.delete(agent_id, schedule_id) }]
        call.call("AgentScheduleService_GetAgentSchedule") { client.agents.schedules.retrieve(agent_id, schedule_id) }
        call.call("AgentScheduleService_UpdateAgentSchedule") do
          client.agents.schedules.update(
            agent_id, schedule_id,
            metadata: { name: "#{run_name}-schedule-updated" }, update_mask: "metadata.name"
          )
        end
        call.call("AgentScheduleService_PauseAgentSchedule") { client.agents.schedules.pause(agent_id, schedule_id) }
        call.call("AgentScheduleService_ResumeAgentSchedule") { client.agents.schedules.resume(agent_id, schedule_id) }
        call.call("AgentScheduleService_ArchiveAgentSchedule") { client.agents.schedules.archive(agent_id, schedule_id) }
        call.call("AgentScheduleService_DeleteAgentSchedule") { client.agents.schedules.delete(agent_id, schedule_id) }
        cleanup.pop
      end
    end

    widget = call.call("WidgetService_CreateWidget") do
      client.widgets.create(metadata: { name: "#{run_name}-widget" }, spec: { agent_id: agent_id })
    end
    widget_id = resource_id.call(widget)
    if widget_id
      cleanup << ["widget", -> { client.widgets.delete(widget_id) }]
      call.call("WidgetService_GetWidget") { client.widgets.retrieve(widget_id) }
      call.call("WidgetService_UpdateWidget") do
        client.widgets.update(widget_id, metadata: { name: "#{run_name}-widget-updated" }, update_mask: "metadata.name")
      end
      call.call("WidgetService_ArchiveWidget") { client.widgets.archive(widget_id) }
      call.call("WidgetService_UnarchiveWidget") { client.widgets.unarchive(widget_id) }
      session = call.call("WidgetSessionService_CreateWidgetSession") do
        client.widget_sessions.create(metadata: { external_id: "#{run_name}-session" }, spec: { widget_id: widget_id })
      end
      session_id = resource_id.call(session)
      if session_id
        cleanup << ["widget session", -> { client.widget_sessions.delete(session_id) }]
        call.call("WidgetSessionService_GetWidgetSession") { client.widget_sessions.retrieve(session_id) }
        call.call("WidgetSessionService_RevokeWidgetSession") { client.widget_sessions.revoke(session_id) }
        call.call("WidgetSessionService_DeleteWidgetSession") { client.widget_sessions.delete(session_id) }
        cleanup.pop
      end
      tenant_external_id = "#{run_name}-tenant"
      tenant_ref = "external_id:#{tenant_external_id}"
      tenant_session = call.call("WidgetSessionService_CreateWidgetSession") do
        client.widget_sessions.create(
          metadata: { external_id: "#{run_name}-tenant-session" },
          spec: { widget_id: widget_id, tenant: { id: tenant_external_id } }
        )
      end
      if resource_id.call(tenant_session)
        call.call("TenantService_GetTenant") { client.tenants.retrieve(tenant_ref) }
        call.call("TenantService_ListTenantSubjects") { client.tenants.list_subjects(tenant_ref, limit: 1) }
        call.call("WidgetSessionService_DeleteTenantWidgetSessions") do
          client.widget_sessions.delete_tenant(tenant_id: tenant_ref)
        end
        call.call("TenantService_DeleteTenant") { client.tenants.delete(tenant_ref) }
      end
      call.call("WidgetService_DeleteWidget") { client.widgets.delete(widget_id) }
      cleanup.pop
    end

    if variation_id
      call.call("AgentService_PublishAgent") { client.agents.publish(agent_id) }
      objective = call.call("ObjectiveService_CreateObjective") do
        client.objectives.create(
          agent_id: agent_id, variation_id: variation_id, system_prompt_data: {},
          first_user_message: "Reply with LIVE_MATRIX_OK.", metadata: { external_id: "#{run_name}-objective" }
        )
      end
      objective_id = resource_id.call(objective)
      if objective_id
        cleanup << ["objective", -> { client.objectives.cancel(objective_id, reason: "matrix cleanup") }]
        call.call("ObjectiveService_CompactObjective") { client.objectives.compact(objective_id) }
        call.call("ObjectiveService_CreateObjectiveFeedback") do
          client.objectives.create_feedback(
            objective_id, metadata: { external_id: "#{run_name}-feedback" },
            data: { score: 1.0, comment: "SDK live matrix" }
          )
        end
        20.times do
          current = client.objectives.retrieve(objective_id)
          break if %w[STATE_FINALIZED STATE_FAILED STATE_TIMED_OUT].include?(current.state)

          sleep 0.5
        end
        call.call("ObjectiveService_ContinueObjective") do
          client.objectives.continue(objective_id, message: "Continue the live matrix probe.", enqueue: true)
        end
        call.call("ObjectiveService_CancelObjective") { client.objectives.cancel(objective_id, reason: "matrix complete") }
        cleanup.pop
      end
    end
  end

  upload = call.call("UploadService_CreateUpload") do
    client.uploads.create(
      metadata: { name: "#{run_name}-upload" },
      spec: { filename: "matrix-byte.txt", content_type: "text/plain", size_bytes: "1" }
    )
  end
  upload_id = resource_id.call(upload)
  call.call("UploadService_GetUpload") { client.uploads.retrieve(upload_id) } if upload_id

  if tool_id && tool_set_id
    call.call("ToolService_DeleteTool") { client.tool_sets.tools.delete(tool_set_id, tool_id) }
    cleanup.reject! { |label, _function| label == "tool" }
  end
  if tool_set_id
    call.call("ToolService_DeleteToolSet") { client.tool_sets.delete(tool_set_id) }
    cleanup.reject! { |label, _function| label == "tool set" }
  end
  if layer_id
    call.call("MemoryService_DeleteMemoryLayer") { client.memory_layers.delete(layer_id) }
    cleanup.reject! { |label, _function| label == "memory layer" }
  end
  if variation_id && agent_id
    call.call("AgentVariationService_DeleteAgentVariation") do
      client.agents.variations.delete(agent_id, variation_id)
    end
    cleanup.reject! { |label, _function| label == "variation" }
  end
  if agent_id
    call.call("AgentService_DeleteAgent") { client.agents.delete(agent_id) }
    cleanup.reject! { |label, _function| label == "agent" }
  end
ensure
  cleanup.reverse_each do |_label, function|
    function.call
  rescue StandardError
    nil
  end
  prior["executedAt"] = Time.now.utc.iso8601
  prior["operations"] = operations
  File.write(results_path, "#{JSON.pretty_generate(prior)}\n")
end

exit(operations.values.any? { |item| item.fetch("status") == "failed" } ? 1 : 0)
