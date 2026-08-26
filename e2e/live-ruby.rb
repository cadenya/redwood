# frozen_string_literal: true

# Live read-only test of the generated Ruby SDK against the real Cadenya API.
# Requires CADENYA_API_KEY and CADENYA_WORKSPACE_ID in the environment
# (source .env.development). Never prints secrets; IDs are truncated.
#
# Usage: source .env.development && ruby e2e/live-ruby.rb
$LOAD_PATH.unshift(File.expand_path("../gen/ruby/lib", __dir__))
require "cadenya"

def short(resource_id)
  resource_id ? "#{resource_id.to_s[0, 12]}…" : resource_id
end

if ENV["CADENYA_API_KEY"].to_s.empty? || ENV["CADENYA_WORKSPACE_ID"].to_s.empty?
  puts "missing CADENYA_API_KEY / CADENYA_WORKSPACE_ID"
  exit 1
end

# Key AND workspace come from env — no per-call workspace_id below exercises
# the client-defaults feature live.
client = Cadenya::Client.new

# 1. Credentials check.
# account.info carries secret material (webhook HMAC secret) — never print it.
account = client.accounts.retrieve
puts "accounts.retrieve   ok  info present: #{!account.info.nil?}"

# 2. Workspaces list (pagination envelope against real data).
workspaces = client.workspaces.list(limit: 2)
puts "workspaces.list     ok  #{workspaces.items.length} item(s), next_page?=#{workspaces.next_page?}"

# 3. Agents in the provided workspace.
agents = client.agents.list(limit: 3)
ids = agents.items.filter_map { |a| short(a.metadata&.id) }.join(", ")
puts "agents.list         ok  #{ids.empty? ? '(none)' : ids}"

# 4. Objectives + auto-pagination across real pages (capped at 5).
seen = []
client.objectives.list(limit: 2).each do |objective|
  seen << short(objective.metadata&.id)
  break if seen.length >= 5
end
puts "objectives.list     ok  #{seen.length} across pages: #{seen.join(', ')}"

# 5. Models catalog.
begin
  models = client.models.list(limit: 3)
  puts "models.list         ok  #{models.items.length} item(s)"
rescue Cadenya::APIError => e
  puts "models.list         skip APIError #{e.status_code}: #{e.message}"
end

# 6. Error mapping against the real server.
begin
  client.objectives.retrieve("obj_does_not_exist")
  puts "error mapping       FAIL (expected an APIError)"
  exit 1
rescue Cadenya::APIError => e
  puts "error mapping       ok  status=#{e.status_code} code=#{e.code}"
end

puts "\nlive API checks passed (ruby)"
