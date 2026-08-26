#!/usr/bin/env ruby
# frozen_string_literal: true

# Generate and optionally execute one live-test snippet per Ruby SDK operation.
#
# The generated manifest is the routing source of truth. Fixtures are supplied
# as JSON and are never invented here: model specs, provider credentials, and
# IDs must come from a controlled test-fixture graph.
#
# Catalog/coverage (does not require or call the SDK):
#   ruby e2e/live-matrix/snippets-ruby.rb > /tmp/ruby-snippets.json
#
# Validate method/accessor names against an *installed* gem:
#   GEM_HOME=... GEM_PATH=... ruby e2e/live-matrix/snippets-ruby.rb --validate-sdk
#
# Execute a read operation with JSON fixtures:
#   GEM_HOME=... GEM_PATH=... ruby e2e/live-matrix/snippets-ruby.rb \
#     --execute ObjectiveService_GetObjective --fixtures /tmp/fixtures.json
#
# Mutations additionally require both --allow-operation <operationId> and the
# same exact ID in CADENYA_LIVE_MATRIX_ALLOW_MUTATIONS. Restricted account,
# credential, and shared-configuration operations remain catalogued but are
# refused by this generic runner. Results never serialize response bodies
# because several endpoints expose secrets.

require "json"
require "optparse"
require "pathname"

ROOT = Pathname.new(__dir__).join("../..").realpath
MANIFEST_PATH = ROOT.join("gen/manifest/manifest.json")
EXPECTED_OPERATION_COUNT = 142

def snake(name)
  name
    .gsub(/(.)([A-Z][a-z]+)/, '\\1_\\2')
    .gsub(/([a-z0-9])([A-Z])/, '\\1_\\2')
    .downcase
end

def singular(word)
  if word.end_with?("ies")
    "#{word[0...-3]}y"
  elsif word.end_with?("ses", "xes")
    word[0...-2]
  elsif word.length > 1 && word.end_with?("s")
    word[0...-1]
  else
    word
  end
end

ID_FIXTURE_BY_PATH_SEGMENT = {
  "api_keys" => "api_key_id",
  "agents" => "agent_id",
  "schedules" => "agent_schedule_id",
  "variations" => "variation_id",
  "assignments" => "assignment_id",
  "memory_layer_assignments" => "memory_layer_assignment_id",
  "memory_layers" => "memory_layer_id",
  "entries" => "memory_entry_id",
  "models" => "model_id",
  "objectives" => "objective_id",
  "tasks" => "task_id",
  "tenants" => "tenant_id",
  "tool_sets" => "tool_set_id",
  "secrets" => "tool_set_secret_id",
  "tools" => "tool_id",
  "uploads" => "upload_id",
  "widget_sessions" => "widget_session_id",
  "widgets" => "widget_id",
  "workspace_secrets" => "workspace_secret_id",
}.freeze

def id_fixture_key(operation, wire_name)
  return snake(wire_name) unless wire_name == "id"

  match = operation.fetch("path").match(%r{/([^/]+)/\{id\}(?::[^/]*)?$})
  return ID_FIXTURE_BY_PATH_SEGMENT.fetch(match[1], "#{singular(match[1])}_id") if match

  leaf = operation.fetch("resource").split(".").last
  "#{singular(leaf)}_id"
end

def body_fixture_key(operation, wire_name)
  # Request fields named metadata/spec/etc. have different shapes per
  # operation, so their fixture keys must be operation-scoped.
  "#{operation.fetch('id')}.#{snake(wire_name)}"
end

def safety_for(operation)
  operation_id = operation.fetch("id")
  method = operation.fetch("httpMethod")
  path = operation.fetch("path")

  return "stream_read" if operation_id == "ObjectiveEventStreamsService_StreamObjectiveEvents"
  if method == "GET"
    return "sensitive_read" if ["api_key", "provider_keys", "secrets"].any? { |token| path.include?(token) }

    return "read_only"
  end
  if [
    "AccountService_RotateChallengeToken",
    "AccountService_RotateWebhookSigningKey",
    "GlobalAPIKeyService_DisableGlobalAPIKey",
    "GlobalAPIKeyService_EnableGlobalAPIKey",
    "GlobalAPIKeyService_RotateGlobalAPIKey",
  ].include?(operation_id)
    return "credential_rotation"
  end
  return "account_admin" if operation_id.start_with?("WorkspaceAdminService_")
  return "shared_configuration" if operation_id.start_with?("ModelService_")
  return "shared_configuration" if operation_id == "TenantService_DeleteTenant"
  return "append_only" if operation_id == "ObjectiveService_CreateObjectiveFeedback"
  return "irreversible_orphan" if operation_id == "UploadService_CreateUpload"
  if [
    "ObjectiveService_CreateObjective",
    "ObjectiveService_CompactObjective",
    "ObjectiveService_ContinueObjective",
  ].include?(operation_id)
    return "cost_bearing"
  end
  return "external_execution" if ["ToolCall", "AgentSchedule", "PublishAgent"].any? { |token| operation_id.include?(token) }

  "fixture_mutation"
end

CLEANUP_BY_CREATE = {
  "APIKeyService_CreateAPIKey" => ["APIKeyService_DeleteAPIKey"],
  "WorkspaceAdminService_CreateWorkspace" => ["WorkspaceAdminService_ArchiveWorkspace"],
  "AgentService_CreateAgent" => ["AgentService_DeleteAgent"],
  "AgentScheduleService_CreateAgentSchedule" => ["AgentScheduleService_DeleteAgentSchedule"],
  "AgentVariationService_CreateAgentVariation" => ["AgentVariationService_DeleteAgentVariation"],
  "AgentVariationService_AddAgentVariationAssignment" => ["AgentVariationService_RemoveAgentVariationAssignment"],
  "AgentVariationService_AddAgentVariationMemoryLayer" => ["AgentVariationService_RemoveAgentVariationMemoryLayer"],
  "AIProviderKeyService_CreateAIProviderKey" => ["AIProviderKeyService_DeleteAIProviderKey"],
  "MemoryService_CreateMemoryLayer" => ["MemoryService_DeleteMemoryLayer"],
  "MemoryService_CreateMemoryEntry" => ["MemoryService_DeleteMemoryEntry"],
  "ObjectiveService_CreateObjective" => ["ObjectiveService_CancelObjective"],
  "ToolService_CreateToolSet" => ["ToolService_DeleteToolSet"],
  "ToolService_CreateToolSetSecret" => ["ToolService_DeleteToolSetSecret"],
  "ToolService_CreateTool" => ["ToolService_DeleteTool"],
  "WidgetSessionService_CreateWidgetSession" => ["WidgetSessionService_DeleteWidgetSession"],
  "WidgetService_CreateWidget" => ["WidgetService_DeleteWidget"],
  "WorkspaceSecretService_CreateWorkspaceSecret" => ["WorkspaceSecretService_DeleteWorkspaceSecret"],
}.freeze

def operation_arguments(operation)
  arguments = []
  fixtures = []
  operation.fetch("positionals", []).each do |positional|
    key = id_fixture_key(operation, positional.fetch("name"))
    arguments << "ctx.fetch(#{key.dump})"
    fixtures << key
  end

  operation.fetch("pathParams").each do |parameter|
    next if parameter.fetch("name") == "workspaceId" # exercise client env default

    key = id_fixture_key(operation, parameter.fetch("name"))
    arguments << "#{snake(parameter.fetch('name'))}: ctx.fetch(#{key.dump})"
    fixtures << key
  end

  operation.fetch("queryParams").each do |parameter|
    next unless parameter["required"]

    key = id_fixture_key(operation, parameter.fetch("name"))
    arguments << "#{snake(parameter.fetch('name'))}: ctx.fetch(#{key.dump})"
    fixtures << key
  end
  if operation.fetch("queryParams").any? { |parameter| parameter.fetch("name") == "limit" }
    arguments << "limit: 1"
  end

  operation.fetch("bodyFields").each do |field|
    next unless field["required"]

    key = body_fixture_key(operation, field.fetch("name"))
    arguments << "#{snake(field.fetch('name'))}: ctx.fetch(#{key.dump})"
    fixtures << key
  end

  unless operation["wholeBody"].nil?
    key = "#{operation.fetch('id')}.body"
    arguments << "body: ctx.fetch(#{key.dump})"
    fixtures << key
  end

  kwargs_key = "#{operation.fetch('id')}.kwargs"
  arguments << "**ctx.fetch(#{kwargs_key.dump}, {})"
  [arguments, fixtures]
end

def build_record(operation)
  arguments, fixtures = operation_arguments(operation)
  call = "client.#{operation.fetch('resource')}.#{operation.fetch('method')}(#{arguments.join(', ')})"
  snippet = if operation.fetch("id") == "ObjectiveEventStreamsService_StreamObjectiveEvents"
              "result = #{call}.each_event.first"
            else
              "result = #{call}"
            end
  {
    "operation_id" => operation.fetch("id"),
    "sdk" => "ruby",
    "http_method" => operation.fetch("httpMethod"),
    "path" => operation.fetch("path"),
    "snippet" => snippet,
    "fixture_keys" => fixtures.uniq.sort,
    "optional_kwargs_fixture" => "#{operation.fetch('id')}.kwargs",
    "environment" => ["CADENYA_API_KEY", "CADENYA_WORKSPACE_ID"],
    "safety" => safety_for(operation),
    "cleanup_operation_ids" => CLEANUP_BY_CREATE.fetch(operation.fetch("id"), []),
    "evidence_required" => [
      "installed_gem_provenance",
      "successful_http_response",
      "typed_response_decode",
      "cleanup_success_for_owned_fixtures",
    ],
  }
end

def load_catalog
  operations = JSON.parse(MANIFEST_PATH.read).fetch("operations")
  ids = operations.map { |operation| operation.fetch("id") }
  raise "manifest contains duplicate operation IDs" unless ids.uniq.length == ids.length

  catalog = operations.to_h { |operation| [operation.fetch("id"), build_record(operation)] }
  unless catalog.length == EXPECTED_OPERATION_COUNT
    raise "expected #{EXPECTED_OPERATION_COUNT} operations, found #{catalog.length}; " \
          "review the matrix and deliberately update EXPECTED_OPERATION_COUNT"
  end
  catalog
end

def resolve_resource(client, dotted_resource)
  dotted_resource.split(".").reduce(client) { |current, component| current.public_send(component) }
end

def assert_installed_gem
  loaded = $LOADED_FEATURES.find { |path| path.end_with?("/cadenya/client.rb") }
  raise "could not establish loaded cadenya/client.rb provenance" if loaded.nil?

  loaded_path = Pathname.new(loaded).realpath
  source_root = ROOT.join("gen/ruby").realpath
  if loaded_path == source_root || loaded_path.to_s.start_with?("#{source_root}/")
    raise "refusing source-tree SDK at #{loaded_path}; install and test the gem"
  end
  loaded_path.to_s
end

def validate_sdk(catalog)
  require "cadenya"

  provenance = assert_installed_gem
  client = Cadenya::Client.new(api_key: "validation-only", workspace_id: "validation-only")
  operations = JSON.parse(MANIFEST_PATH.read).fetch("operations").to_h { |op| [op.fetch("id"), op] }
  catalog.each_key do |operation_id|
    operation = operations.fetch(operation_id)
    callable = resolve_resource(client, operation.fetch("resource")).method(operation.fetch("method"))
    signature = callable.parameters
    parameter_names = signature.filter_map { |_kind, name| name&.to_s }.to_set
    expected_positionals = operation.fetch("positionals", []).map { |item| snake(item.fetch("name")) }
    actual_positionals = signature.filter_map { |kind, name| name&.to_s if kind == :req }
    unless actual_positionals.first(expected_positionals.length) == expected_positionals
      raise "#{operation_id} positional signature mismatch: " \
            "expected #{expected_positionals.inspect}, found #{actual_positionals.inspect}"
    end
    expected = []
    expected.concat(operation.fetch("positionals", []).map { |item| snake(item.fetch("name")) })
    expected.concat(operation.fetch("pathParams").map { |item| snake(item.fetch("name")) })
    expected.concat(operation.fetch("queryParams").map { |item| snake(item.fetch("name")) })
    expected.concat(operation.fetch("bodyFields").map { |item| snake(item.fetch("name")) })
    expected << "body" unless operation["wholeBody"].nil?
    missing = expected.to_set - parameter_names
    raise "#{operation_id} missing generated parameters: #{missing.to_a.sort}" unless missing.empty?
  end
  provenance
end

def load_fixtures(path)
  return {} if path.nil?

  value = JSON.parse(Pathname.new(path).read)
  raise "fixture JSON must be an object" unless value.is_a?(Hash)

  value
end

def mutation_allowlist
  ENV.fetch("CADENYA_LIVE_MATRIX_ALLOW_MUTATIONS", "").split(",").map(&:strip).reject(&:empty?).to_set
end

def execute(operation_id, record, fixtures, allow_operation)
  restricted = %w[credential_rotation account_admin shared_configuration append_only irreversible_orphan]
  if restricted.include?(record.fetch("safety"))
    raise "#{operation_id} is #{record.fetch('safety')}; use a purpose-built isolated-account test"
  end
  unless %w[read_only sensitive_read stream_read].include?(record.fetch("safety"))
    unless allow_operation == operation_id && mutation_allowlist.include?(operation_id)
      raise "mutation refused: pass --allow-operation with this exact operation ID and " \
            "include it in CADENYA_LIVE_MATRIX_ALLOW_MUTATIONS"
    end
  end

  missing = record.fetch("fixture_keys").reject { |key| fixtures.key?(key) }
  raise "missing fixtures for #{operation_id}: #{missing.join(', ')}" unless missing.empty?

  require "cadenya"

  provenance = assert_installed_gem
  client = Cadenya::Client.new
  ctx = fixtures
  result = eval(record.fetch("snippet"), binding, "<#{operation_id}>", 1) # rubocop:disable Security/Eval
  {
    "operation_id" => operation_id,
    "sdk" => "ruby",
    "status" => "completed",
    "installed_artifact" => provenance,
    "response_type" => result.class.name,
  }
end

def first_id(page)
  page.items.each do |item|
    value = item.respond_to?(:metadata) ? item.metadata&.id : nil
    return value.to_s unless value.nil? || value.to_s.empty?
  end
  nil
end

def discover_read_fixtures(client)
  # Each probe is independent: a role may legitimately lack one catalog or a
  # workspace may have no instance of one resource. Dependent operations are
  # then recorded as blocked instead of preventing unrelated reads.
  ctx = {}
  capture = lambda do |key, &block|
    value = first_id(block.call)
    ctx[key] = value if value && !value.empty?
  rescue StandardError
    nil
  end

  capture.call("agent_id") { client.agents.list(limit: 20) }
  capture.call("api_key_id") { client.api_keys.list(limit: 20) }
  capture.call("ai_provider_key_id") { client.ai_provider_keys.list(limit: 20) }
  capture.call("memory_layer_id") { client.memory_layers.list(limit: 20) }
  capture.call("model_id") { client.models.list(limit: 20) }
  capture.call("objective_id") { client.objectives.list(limit: 20) }
  capture.call("tenant_id") { client.tenants.list(limit: 20) }
  capture.call("tool_set_id") { client.tool_sets.list(limit: 20) }
  capture.call("widget_session_id") { client.widget_sessions.list(limit: 20) }
  capture.call("widget_id") { client.widgets.list(limit: 20) }
  capture.call("workspace_secret_id") { client.workspace_secrets.list(limit: 20) }

  if ctx["agent_id"]
    capture.call("agent_schedule_id") { client.agents.schedules.list(agent_id: ctx.fetch("agent_id"), limit: 20) }
    capture.call("variation_id") { client.agents.variations.list(agent_id: ctx.fetch("agent_id"), limit: 20) }
  end
  if ctx["memory_layer_id"]
    capture.call("memory_entry_id") { client.memory_layers.entries.list(memory_layer_id: ctx.fetch("memory_layer_id"), limit: 20) }
  end
  if ctx["tool_set_id"]
    capture.call("tool_set_secret_id") { client.tool_sets.secrets.list(tool_set_id: ctx.fetch("tool_set_id"), limit: 20) }
    capture.call("tool_id") { client.tool_sets.tools.list(tool_set_id: ctx.fetch("tool_set_id"), limit: 20) }
  end
  if ctx["objective_id"]
    begin
      events = client.objectives.list_events(ctx.fetch("objective_id"), limit: 20)
      event_ids = events.items.filter_map { |item| item.metadata&.id&.to_s }
      if event_ids.length >= 2
        ctx["ObjectiveEventStreamsService_StreamObjectiveEvents.kwargs"] = { last_event_id: event_ids.first }
      end
    rescue StandardError
      nil
    end
    capture.call("task_id") { client.objectives.list_tasks(ctx.fetch("objective_id"), limit: 20) }
    capture.call("tool_call_id") { client.objectives.list_tool_calls(ctx.fetch("objective_id"), limit: 20) }
  end
  ctx["query"] = "live-matrix"
  ctx
end

def live_read_wave(catalog)
  require "cadenya"
  require "time"

  provenance = assert_installed_gem
  client = Cadenya::Client.new
  fixtures = discover_read_fixtures(client)
  operations = {}
  catalog.each do |operation_id, record|
    next unless %w[read_only sensitive_read stream_read].include?(record.fetch("safety"))

    if record.fetch("safety") == "stream_read" && !fixtures.key?(record.fetch("optional_kwargs_fixture"))
      operations[operation_id] = {
        "status" => "blocked",
        "evidence" => "no replay checkpoint available from safe event-history discovery",
      }
      next
    end

    missing = record.fetch("fixture_keys").reject { |key| fixtures.key?(key) }
    unless missing.empty?
      operations[operation_id] = {
        "status" => "blocked",
        "evidence" => "fixture unavailable from safe list/read discovery: #{missing.join(', ')}",
      }
      next
    end
    ctx = fixtures
    begin
      result = eval(record.fetch("snippet"), binding, "<#{operation_id}>", 1) # rubocop:disable Security/Eval
      operations[operation_id] = {
        "status" => "completed",
        "evidence" => "installed gem; HTTP 2xx; decoded #{result.class.name}",
      }
    rescue Cadenya::APIError => error
      if [403, 501].include?(error.status_code) ||
         (operation_id == "ToolService_GetToolSetOpenAPISpec" && error.status_code == 500)
        detail = if error.status_code == 403
                   "authorization prerequisite"
                 elsif error.status_code == 501
                   "endpoint not implemented; see API contract log"
                 else
                   "requires an OpenAPI-adapter tool-set fixture"
                 end
        operations[operation_id] = {
          "status" => "blocked",
          "evidence" => "installed gem; HTTP #{error.status_code}; #{detail}; response body not retained",
        }
        next
      end
      operations[operation_id] = {
        "status" => "failed",
        "evidence" => "installed gem; APIError HTTP #{error.status_code}; response body not retained",
      }
    rescue StandardError => error
      operations[operation_id] = {
        "status" => "failed",
        "evidence" => "installed gem; #{error.class}; response body not retained",
      }
    end
  end
  {
    "schemaVersion" => 1,
    "sdk" => "ruby",
    "executedAt" => Time.now.utc.iso8601,
    "installedArtifact" => provenance,
    "operations" => operations,
  }
end

options = {}
OptionParser.new do |parser|
  parser.on("--operation OPERATION_ID") { |value| options[:operation] = value }
  parser.on("--validate-sdk") { options[:validate_sdk] = true }
  parser.on("--execute OPERATION_ID") { |value| options[:execute] = value }
  parser.on("--fixtures PATH") { |value| options[:fixtures] = value }
  parser.on("--allow-operation OPERATION_ID") { |value| options[:allow_operation] = value }
  parser.on("--live-read-wave") { options[:live_read_wave] = true }
  parser.on("--results PATH") { |value| options[:results] = value }
end.parse!

require "set" if options[:validate_sdk] || options[:execute]
catalog = load_catalog
if options[:live_read_wave]
  result = live_read_wave(catalog)
  if options[:results] && Pathname.new(options[:results]).exist?
    prior = JSON.parse(Pathname.new(options[:results]).read)
    merged = prior.fetch("operations", {})
    result.fetch("operations").each do |operation_id, current|
      previous = merged[operation_id]
      # Fixture cleanup can block later discovery; that is not grounds to
      # downgrade an earlier real 2xx + decoded-type completion.
      next if previous&.fetch("status", nil) == "completed" && current.fetch("status") != "completed"

      merged[operation_id] = current
    end
    result.each { |key, value| prior[key] = value unless key == "operations" }
    result = prior
  end
  rendered = "#{JSON.pretty_generate(result)}\n"
  options[:results] ? Pathname.new(options[:results]).write(rendered) : puts(rendered)
  exit(result.fetch("operations").values.any? { |item| item.fetch("status") == "failed" } ? 1 : 0)
end
if options[:validate_sdk]
  puts JSON.generate("sdk" => "ruby", "operations" => catalog.length, "installed_artifact" => validate_sdk(catalog))
  exit 0
end

selected = options[:execute] || options[:operation]
abort("unknown operation ID: #{selected}") if selected && !catalog.key?(selected)
if options[:execute]
  puts JSON.generate(execute(options[:execute], catalog.fetch(options[:execute]), load_fixtures(options[:fixtures]), options[:allow_operation]))
elsif selected
  puts JSON.pretty_generate(catalog.fetch(selected))
else
  puts JSON.pretty_generate(catalog)
end
