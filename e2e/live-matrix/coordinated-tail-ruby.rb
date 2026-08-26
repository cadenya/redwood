#!/usr/bin/env ruby
# frozen_string_literal: true

# Serialized account/credential/shared-state tail for the installed Ruby gem.
#
# THIS ROTATES THE MANAGED GLOBAL API TOKEN and other account secrets. It is
# deliberately excluded from ordinary test commands. Exact opt-in:
#
#   CADENYA_COORDINATED_TAIL=ruby \
#     ruby coordinated-tail-ruby.rb --rotate-global-and-run-account-tail
#
# If the ambient credential is independent, it remains the recovery controller
# and no env file is changed. If it is the SAME managed global token, the runner
# refuses unless CADENYA_ROOT_ENV_FILE names the root `.env.development`; it
# durably atomic-replaces that assignment immediately after rotation and before
# any later network call. No secret/response body is printed or stored in test
# artifacts. Never run tails concurrently.

require "cadenya"
require "json"
require "pathname"
require "set"
require "time"

HERE = File.expand_path(__dir__)
RESULTS = File.join(HERE, "results-ruby.json")
RUN_NAME = "sdk-tail-ruby-#{Time.now.to_i}"
OPT_IN = "--rotate-global-and-run-account-tail"

TAIL_OPERATIONS = %w[
  AccountService_RotateChallengeToken
  AccountService_RotateWebhookSigningKey
  GlobalAPIKeyService_GetGlobalAPIKey
  GlobalAPIKeyService_DisableGlobalAPIKey
  GlobalAPIKeyService_EnableGlobalAPIKey
  GlobalAPIKeyService_RotateGlobalAPIKey
  WorkspaceAdminService_ListProfiles
  WorkspaceAdminService_ListAccountWorkspaces
  WorkspaceAdminService_CreateWorkspace
  WorkspaceAdminService_GetWorkspace
  WorkspaceAdminService_ArchiveWorkspace
  WorkspaceAdminService_UpdateWorkspace
  WorkspaceAdminService_ListWorkspaceMembers
  WorkspaceAdminService_AddWorkspaceMember
  WorkspaceAdminService_RemoveWorkspaceMember
  AIProviderKeyService_ListAIProviderKeys
  AIProviderKeyService_CreateAIProviderKey
  AIProviderKeyService_GetAIProviderKey
  AIProviderKeyService_UpdateAIProviderKey
  AIProviderKeyService_DeleteAIProviderKey
  ModelService_ListModels
  ModelService_GetModel
  ModelService_DisableModel
  ModelService_EnableModel
  ModelService_SwapModelOnVariations
  ObjectiveService_ApproveToolCall
  ObjectiveService_DenyToolCall
  ObjectiveService_SetToolCallContent
  ObjectiveService_CompactObjective
  ObjectiveService_ContinueObjective
].freeze

unless ARGV == [OPT_IN] && ENV["CADENYA_COORDINATED_TAIL"] == "ruby"
  warn "refusing: pass #{OPT_IN} and set CADENYA_COORDINATED_TAIL=ruby"
  exit 2
end
ambient_token = ENV.fetch("CADENYA_API_KEY", "")
abort "CADENYA_API_KEY is required" if ambient_token.empty?

prior = File.exist?(RESULTS) ? JSON.parse(File.read(RESULTS)) : {
  "schemaVersion" => 1, "sdk" => "ruby", "operations" => {},
}
operations = prior["operations"] ||= {}
cleanup = []

record = lambda do |operation_id, status, evidence|
  operations[operation_id] = { "status" => status, "evidence" => evidence }
end
complete = lambda do |operation_id, value|
  record.call(operation_id, "completed", "installed gem; HTTP 2xx; decoded #{value.class.name}")
  value
end
attempt = lambda do |operation_id, scenario_400: false, &block|
  complete.call(operation_id, block.call)
rescue Cadenya::APIError => error
  if error.status_code == 403 || (scenario_400 && error.status_code == 400)
    prerequisite = error.status_code == 403 ? "authorization" : "lifecycle/scenario"
    record.call(operation_id, "blocked", "installed gem; HTTP #{error.status_code}; #{prerequisite} prerequisite")
  else
    record.call(operation_id, "failed", "installed gem; APIError HTTP #{error.status_code}; body not retained")
  end
  nil
rescue StandardError => error
  record.call(operation_id, "failed", "installed gem; #{error.class}; body not retained")
  nil
end

resource_id = lambda do |value|
  value&.respond_to?(:metadata) ? value.metadata&.id&.to_s : nil
end

shell_quote = lambda do |value|
  "'#{value.gsub("'", %q('"'"'))}'"
end

validated_env = lambda do |path, expected_old|
  pathname = File.expand_path(path)
  unless Pathname.new(path).absolute? && File.basename(pathname) == ".env.development" && File.file?(pathname)
    raise "CADENYA_ROOT_ENV_FILE must be the existing absolute root .env.development"
  end
  lines = File.readlines(pathname, chomp: false)
  matches = []
  lines.each_with_index do |line, index|
    stripped = line.strip
    candidate = stripped.start_with?("export ") ? stripped.delete_prefix("export ") : stripped
    matches << index if candidate.start_with?("CADENYA_API_KEY=")
  end
  raise "root env must contain exactly one CADENYA_API_KEY assignment" unless matches.length == 1

  index = matches.first
  stripped = lines[index].strip
  exported = stripped.start_with?("export ")
  value = (exported ? stripped.delete_prefix("export ") : stripped).split("=", 2).last
  value = value[1...-1] if value.length >= 2 && ["'", '"'].include?(value[0]) && value[-1] == value[0]
  raise "root env API key changed since process start; refusing overwrite" unless value == expected_old

  [pathname, lines, index, exported]
end


atomic_replace_api_key = lambda do |path, expected_old, replacement|
  pathname, lines, index, exported = validated_env.call(path, expected_old)
  mode = File.stat(pathname).mode & 0o777
  newline = lines[index].end_with?("\n") ? "\n" : ""
  lines[index] = "#{exported ? 'export ' : ''}CADENYA_API_KEY=#{shell_quote.call(replacement)}#{newline}"
  temporary = File.join(File.dirname(pathname), ".env.development.#{Process.pid}.rotating")
  flags = File::WRONLY | File::CREAT | File::EXCL
  File.open(temporary, flags, mode) do |file|
    file.write(lines.join)
    file.flush
    file.fsync
  end
  File.rename(temporary, pathname)
  File.open(File.dirname(pathname), File::RDONLY) { |directory| directory.fsync }
ensure
  File.unlink(temporary) if defined?(temporary) && File.exist?(temporary)
end

controller = Cadenya::Client.new(api_key: ambient_token)
client = nil
global_disabled = false
created_workspace_id = nil
provider_key_id = nil
tool_set_id = nil
agent_id = nil
objective_ids = []
disabled_model_id = nil

begin
  before = attempt.call("GlobalAPIKeyService_GetGlobalAPIKey") { controller.api_keys.retrieve_global }
  raise "RetrieveGlobal failed; refusing rotation" unless before
  before_token = before.spec&.token
  raise "RetrieveGlobal returned no managed token" if before_token.to_s.empty?

  before = nil
  ambient_is_managed = ambient_token == before_token
  env_path = nil
  if ambient_is_managed
    env_path = ENV.fetch("CADENYA_ROOT_ENV_FILE", "")
    raise "ambient token is managed; refusing rotation without CADENYA_ROOT_ENV_FILE" if env_path.empty?

    # Validate destination and expected old value before irreversible rotation.
    validated_env.call(env_path, ambient_token)
  end
  rotated = attempt.call("GlobalAPIKeyService_RotateGlobalAPIKey") { controller.api_keys.rotate_global }
  raise "RotateGlobal failed; coordinated tail cannot continue" unless rotated
  if ambient_is_managed
    rotated_token = rotated.spec&.token
  else
    after = controller.api_keys.retrieve_global
    rotated_token = after.spec&.token
    after = nil
  end
  if rotated_token.to_s.empty? || rotated_token == before_token
    raise "managed global rotation did not yield a distinct retrievable token"
  end
  if ambient_is_managed
    atomic_replace_api_key.call(env_path, ambient_token, rotated_token)
    ENV["CADENYA_API_KEY"] = rotated_token
    controller = Cadenya::Client.new(api_key: rotated_token)
  end
  before_token = nil
  rotated = nil
  client = Cadenya::Client.new(api_key: rotated_token)
  rotated_token = nil

  attempt.call("AccountService_RotateChallengeToken") { client.accounts.rotate_challenge_token }
  attempt.call("AccountService_RotateWebhookSigningKey") { client.accounts.rotate_webhook_signing_key }

  # The ambient controller remains independently authenticated, so disabling
  # the managed token cannot strand the process.
  if ambient_is_managed
    record.call(
      "GlobalAPIKeyService_DisableGlobalAPIKey", "blocked",
      "ambient credential is the managed key; no independent recovery controller"
    )
    record.call(
      "GlobalAPIKeyService_EnableGlobalAPIKey", "blocked",
      "global disable intentionally not attempted without independent recovery"
    )
  else
    attempt.call("GlobalAPIKeyService_DisableGlobalAPIKey") { controller.api_keys.disable_global }
    if operations.dig("GlobalAPIKeyService_DisableGlobalAPIKey", "status") == "completed"
      global_disabled = true
      attempt.call("GlobalAPIKeyService_EnableGlobalAPIKey") { controller.api_keys.enable_global }
      global_disabled = false if operations.dig("GlobalAPIKeyService_EnableGlobalAPIKey", "status") == "completed"
    end
  end

  profiles = attempt.call("WorkspaceAdminService_ListProfiles") do
    client.workspace_admin.list_profiles(limit: 100)
  end
  attempt.call("WorkspaceAdminService_ListAccountWorkspaces") do
    client.workspace_admin.list_account(limit: 10, include_archived: true)
  end
  workspace = attempt.call("WorkspaceAdminService_CreateWorkspace") do
    client.workspace_admin.create(
      metadata: { name: "#{RUN_NAME}-workspace" },
      spec: { description: "SDK coordinated tail" }
    )
  end
  created_workspace_id = resource_id.call(workspace)
  if created_workspace_id
    cleanup << ["workspace", -> { client.workspace_admin.archive(workspace_id: created_workspace_id) }]
    attempt.call("WorkspaceAdminService_GetWorkspace") do
      client.workspace_admin.retrieve(workspace_id: created_workspace_id)
    end
    attempt.call("WorkspaceAdminService_UpdateWorkspace") do
      client.workspace_admin.update(
        workspace_id: created_workspace_id,
        metadata: { name: "#{RUN_NAME}-workspace-updated" }, update_mask: "metadata.name"
      )
    end
    members = attempt.call("WorkspaceAdminService_ListWorkspaceMembers") do
      client.workspace_admin.list_members(workspace_id: created_workspace_id, limit: 100)
    end
    existing_ids = (members&.items || []).filter_map { |member| member.profile_id&.to_s }.to_set
    candidate = (profiles&.items || []).map { |profile| resource_id.call(profile) }.compact.find do |id|
      !existing_ids.include?(id)
    end
    if candidate
      added = attempt.call("WorkspaceAdminService_AddWorkspaceMember") do
        client.workspace_admin.add_member(workspace_id: created_workspace_id, profile_id: candidate)
      end
      if added
        complete.call(
          "WorkspaceAdminService_RemoveWorkspaceMember",
          client.workspace_admin.remove_member(workspace_id: created_workspace_id, profile_id: candidate)
        )
      end
    else
      record.call("WorkspaceAdminService_AddWorkspaceMember", "blocked", "no non-member profile fixture")
      record.call("WorkspaceAdminService_RemoveWorkspaceMember", "blocked", "member was not added")
    end
  end

  attempt.call("AIProviderKeyService_ListAIProviderKeys") { client.ai_provider_keys.list(limit: 10) }
  provider = attempt.call("AIProviderKeyService_CreateAIProviderKey") do
    client.ai_provider_keys.create(
      metadata: { name: "#{RUN_NAME}-provider" },
      spec: {
        provider: "AI_PROVIDER_OPENAI",
        credentials: { type: "apiKey", api_key: { api_key: "#{RUN_NAME}-not-real" } },
        config: { type: "openai", openai: {} },
      }
    )
  end
  provider_key_id = resource_id.call(provider)
  if provider_key_id
    cleanup << ["provider", -> { client.ai_provider_keys.delete(provider_key_id) }]
    attempt.call("AIProviderKeyService_GetAIProviderKey") do
      client.ai_provider_keys.retrieve(provider_key_id)
    end
    attempt.call("AIProviderKeyService_UpdateAIProviderKey") do
      client.ai_provider_keys.update(
        provider_key_id,
        metadata: { name: "#{RUN_NAME}-provider-updated" }, update_mask: "metadata.name"
      )
    end
  end

  model_page = attempt.call("ModelService_ListModels") { client.models.list(limit: 100) }
  models = model_page&.items || []
  model_ids = models.filter_map { |model| resource_id.call(model) }
  primary_model = models.find { |model| model.state == "STATE_ENABLED" && resource_id.call(model) }
  primary_model_id = resource_id.call(primary_model)
  if primary_model_id
    attempt.call("ModelService_GetModel") { client.models.retrieve(primary_model_id) }
    attempt.call("ModelService_DisableModel") { client.models.disable(primary_model_id) }
    if operations.dig("ModelService_DisableModel", "status") == "completed"
      disabled_model_id = primary_model_id
      attempt.call("ModelService_EnableModel") { client.models.enable(primary_model_id) }
      disabled_model_id = nil if operations.dig("ModelService_EnableModel", "status") == "completed"
    end
  else
    %w[ModelService_GetModel ModelService_DisableModel ModelService_EnableModel].each do |operation_id|
      record.call(operation_id, "blocked", "no enabled model fixture")
    end
  end

  provider_models = []
  if provider_key_id
    10.times do
      begin
        provider_models = client.models.list(ai_provider_key_id: provider_key_id, limit: 100).items.filter_map do |model|
          resource_id.call(model)
        end
      rescue StandardError
        provider_models = []
      end
      break if provider_models.length >= 2

      sleep 0.5
    end
  end
  if provider_models.length >= 2
    attempt.call("ModelService_SwapModelOnVariations") do
      client.models.swap_on_variations(model_swaps: [{
        current_model_id: provider_models[0], next_model_id: provider_models[1],
        disable_current_after_swap: false,
      }])
    end
  else
    record.call("ModelService_SwapModelOnVariations", "blocked", "owned provider did not expose two model fixtures")
  end

  scenario_model_id = primary_model_id || model_ids.first
  if scenario_model_id
    tool_set = client.tool_sets.create(
      metadata: { name: "#{RUN_NAME}-tools" },
      spec: { description: "coordinated tail", adapter: { type: "bare", bare: {} } }
    )
    tool_set_id = resource_id.call(tool_set)
    cleanup << ["tool set", -> { client.tool_sets.delete(tool_set_id) }]
    tools = %w[approve deny].map do |suffix|
      tool = client.tool_sets.tools.create(
        tool_set_id: tool_set_id, metadata: { name: "#{RUN_NAME}-#{suffix}" },
        spec: {
          description: "Call the #{suffix} matrix tool", requires_approval: true,
          parameters: { type: "object", properties: {} }, config: { type: "bare", bare: {} },
        }
      )
      tool_id = resource_id.call(tool)
      cleanup << ["tool", ->(id = tool_id) { client.tool_sets.tools.delete(id, tool_set_id: tool_set_id) }]
      tool_id
    end
    agent = client.agents.create(
      metadata: { name: "#{RUN_NAME}-agent" },
      spec: { variation_selection_mode: "VARIATION_SELECTION_MODE_UNSPECIFIED" },
      default_variation: {
        metadata: { name: "#{RUN_NAME}-variation" },
        spec: {
          system_prompt_template: "Call exactly the requested tool.",
          model_config: { model_id: scenario_model_id },
        },
      }
    )
    agent_id = resource_id.call(agent)
    cleanup << ["agent", -> { client.agents.delete(agent_id) }]
    variation = client.agents.variations.list(agent_id: agent_id, limit: 1).items.first
    variation_id = resource_id.call(variation)
    tools.each do |tool_id|
      client.agents.variations.add_assignment(
        agent_id: agent_id, variation_id: variation_id,
        body: { type: "toolId", tool_id: tool_id }
      )
    end
    client.agents.publish(agent_id)

    create_tool_objective = lambda do |tool_name|
      objective = client.objectives.create(
        agent_id: agent_id, variation_id: variation_id, system_prompt_data: {},
        first_user_message: "Call the tool named #{tool_name} now.",
        metadata: { external_id: "#{RUN_NAME}-#{tool_name}" }
      )
      objective_id = resource_id.call(objective)
      objective_ids << objective_id
      cleanup << ["objective", -> { client.objectives.cancel(objective_id, reason: "tail cleanup") }]
      tool_call_id = nil
      80.times do
        page = client.objectives.list_tool_calls(objective_id, limit: 20)
        waiting = page.items.find { |item| item.status == "TOOL_CALL_STATUS_WAITING_FOR_APPROVAL" }
        if waiting
          tool_call_id = resource_id.call(waiting)
          break
        end
        sleep 0.5
      end
      [objective_id, tool_call_id]
    end

    approve_objective, approve_call = create_tool_objective.call("#{RUN_NAME}-approve")
    if approve_call
      attempt.call("ObjectiveService_CompactObjective", scenario_400: true) do
        client.objectives.compact(approve_objective)
      end
      attempt.call("ObjectiveService_ApproveToolCall") do
        client.objectives.approve_tool_call(approve_objective, tool_call_id: approve_call)
      end
      if operations.dig("ObjectiveService_ApproveToolCall", "status") == "completed"
        attempt.call("ObjectiveService_SetToolCallContent") do
          client.objectives.set_tool_call_content(
            approve_objective, tool_call_id: approve_call,
            content: [{ type: "text", text: { text: "approved live tail result" } }]
          )
        end
      end
    else
      %w[
        ObjectiveService_CompactObjective ObjectiveService_ApproveToolCall
        ObjectiveService_SetToolCallContent
      ].each do |operation_id|
        record.call(operation_id, "blocked", "approval tool-call fixture did not materialize")
      end
    end

    deny_objective, deny_call = create_tool_objective.call("#{RUN_NAME}-deny")
    if deny_call
      attempt.call("ObjectiveService_DenyToolCall") do
        client.objectives.deny_tool_call(
          deny_objective, tool_call_id: deny_call, memo: "coordinated tail denial"
        )
      end
    else
      record.call("ObjectiveService_DenyToolCall", "blocked", "denial tool-call fixture did not materialize")
    end

    continued = false
    objective_ids.each do |objective_id|
      60.times do
        current = client.objectives.retrieve(objective_id)
        if current.state == "STATE_FINALIZED"
          attempt.call("ObjectiveService_ContinueObjective", scenario_400: true) do
            client.objectives.continue(
              objective_id, message: "Continue coordinated tail.", enqueue: true
            )
          end
          continued = true
          break
        end
        break if %w[STATE_FAILED STATE_CANCELLED STATE_TIMED_OUT].include?(current.state)

        sleep 0.5
      end
      break if continued
    end
    unless continued
      record.call("ObjectiveService_ContinueObjective", "blocked", "no finalized owned objective fixture")
    end
  else
    %w[
      ObjectiveService_ApproveToolCall ObjectiveService_DenyToolCall
      ObjectiveService_SetToolCallContent ObjectiveService_CompactObjective
      ObjectiveService_ContinueObjective
    ].each do |operation_id|
      record.call(operation_id, "blocked", "no model fixture for owned objective scenario")
    end
  end

  if provider_key_id
    attempt.call("AIProviderKeyService_DeleteAIProviderKey") do
      client.ai_provider_keys.delete(provider_key_id)
    end
    cleanup.reject! { |label, _function| label == "provider" }
  end
  if created_workspace_id
    attempt.call("WorkspaceAdminService_ArchiveWorkspace") do
      client.workspace_admin.archive(workspace_id: created_workspace_id)
    end
    cleanup.reject! { |label, _function| label == "workspace" }
  end
ensure
  if global_disabled
    begin
      controller.api_keys.enable_global
      global_disabled = false
    rescue StandardError
      nil
    end
  end
  if disabled_model_id && client
    begin
      client.models.enable(disabled_model_id)
      disabled_model_id = nil
    rescue StandardError
      nil
    end
  end
  cleanup.reverse_each do |_label, function|
    function.call
  rescue StandardError
    nil
  end
  TAIL_OPERATIONS.each do |operation_id|
    operations[operation_id] ||= {
      "status" => "blocked", "evidence" => "coordinated tail stopped before this operation",
    }
  end
  prior["executedAt"] = Time.now.utc.iso8601
  prior["operations"] = operations
  File.write(RESULTS, "#{JSON.pretty_generate(prior)}\n")
end

unless (TAIL_OPERATIONS - operations.keys).empty?
  raise "internal coverage assertion failed"
end
exit(TAIL_OPERATIONS.any? { |operation_id| operations.fetch(operation_id).fetch("status") == "failed" } ? 1 : 0)
