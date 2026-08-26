// Decode-regression matrix for optional discriminated unions: protobuf JSON
// gateways encode an unset union as absent, null, {}, or `"type": ""`. Every
// backend with a runtime decoder must accept all of those as absent and still
// reject unknown NON-empty discriminators. Run: node e2e/union-empty.mjs
// (TypeScript has no runtime union decoder — responses pass through — so it
// has no cases here.)
import { execFileSync } from 'node:child_process';
import { mkdirSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { provisionPython } from './conformance/provision.mjs';

const root = new URL('..', import.meta.url).pathname;
const goCache = `${tmpdir()}/redwood-union-empty-go-cache`;
mkdirSync(goCache, { recursive: true });
const goEnv = { ...process.env, GOCACHE: goCache };
let failed = false;
const run = (label, cmd, args, opts = {}) => {
  try {
    const out = execFileSync(cmd, args, { encoding: 'utf8', timeout: 120_000, ...opts });
    process.stdout.write(out);
  } catch (err) {
    failed = true;
    process.stdout.write(err.stdout ?? '');
    process.stderr.write(err.stderr ?? String(err));
    console.error(`FAIL ${label}`);
  }
};

execFileSync('go', ['mod', 'tidy'], { cwd: `${root}e2e/union-empty/go`, timeout: 120_000, env: goEnv });
run('go', 'go', ['run', '.'], { cwd: `${root}e2e/union-empty/go`, env: goEnv });

// The host interpreter may lack the SDK's dependencies; run through the same
// provisioned venv the conformance suite installs the wheel into.
const venvPython = provisionPython(`${root}gen/python`);
run('python', venvPython, ['-c', `
import sys
from cadenya.types import _decode_AIProviderConfig, AIProviderKey

failures = 0
def check(label, fn, expect):
    global failures
    try:
        got = fn()
        ok = expect == "some" and got is not None or expect is None and got is None
        state = "ok " if ok else "FAIL"
    except ValueError as e:
        got, ok = e, expect == "raise" and "unknown type" in str(e)
        state = "ok " if ok else "FAIL"
    if not ok:
        failures += 1
    print(f"{state} python: {label}")

check("null decodes as absent", lambda: _decode_AIProviderConfig(None), None)
check("{} decodes as absent", lambda: _decode_AIProviderConfig({}), None)
check("type '' decodes as absent", lambda: _decode_AIProviderConfig({"type": ""}), None)
check("known tag openrouter", lambda: _decode_AIProviderConfig({"type": "openrouter", "openrouter": {}}), "some")
check("known tag openai", lambda: _decode_AIProviderConfig({"type": "openai", "openai": {}}), "some")
check("known tag openaiCompatible", lambda: _decode_AIProviderConfig({"type": "openaiCompatible", "openaiCompatible": {"baseUrl": "https://x.test"}}), "some")
check("unknown non-empty tag rejected", lambda: _decode_AIProviderConfig({"type": "bogus"}), "raise")
check("key with empty config decodes", lambda: AIProviderKey._from_json({
    "metadata": {"id": "apk_1", "accountId": "acct_1", "workspaceId": "ws_1", "name": "anthropic", "profileId": "prof_1", "externalId": "ext_1", "labels": {}, "createdAt": "2026-01-01T00:00:00Z"},
    "spec": {"provider": "AI_PROVIDER_ANTHROPIC", "config": {}},
}), "some")
sys.exit(1 if failures else 0)
`]);

const ruby = process.env.REDWOOD_RUBY || process.env.RUBY || 'ruby';
run('ruby', ruby, [`-I${root}gen/ruby/lib`, '-e', `
require "cadenya"

failures = 0
check = lambda do |label, expect, &fn|
  begin
    got = fn.call
    ok = expect == :some ? !got.nil? : got.nil?
  rescue ArgumentError => e
    ok = expect == :raise && e.message.include?("unknown type")
  end
  failures += 1 unless ok
  puts "#{ok ? 'ok ' : 'FAIL'} ruby: #{label}"
end

check.call("nil decodes as absent", :nil) { Cadenya::Types.decode_AIProviderConfig(nil) }
check.call("{} decodes as absent", :nil) { Cadenya::Types.decode_AIProviderConfig({}) }
check.call("type '' decodes as absent", :nil) { Cadenya::Types.decode_AIProviderConfig({"type" => ""}) }
check.call("known tag openrouter", :some) { Cadenya::Types.decode_AIProviderConfig({"type" => "openrouter"}) }
check.call("known tag openai", :some) { Cadenya::Types.decode_AIProviderConfig({"type" => "openai"}) }
check.call("known tag openaiCompatible", :some) { Cadenya::Types.decode_AIProviderConfig({"type" => "openaiCompatible"}) }
check.call("unknown non-empty tag rejected", :raise) { Cadenya::Types.decode_AIProviderConfig({"type" => "bogus"}) }
check.call("key with empty config decodes", :some) do
  Cadenya::Types::AIProviderKey.from_json({
    "metadata" => {"id" => "apk_1", "name" => "anthropic"},
    "spec" => {"provider" => "AI_PROVIDER_ANTHROPIC", "config" => {}},
  })
end
exit(failures.zero? ? 0 : 1)
`]);

if (failed) {
  console.error('\nunion empty-encoding matrix: FAILURES');
  process.exit(1);
}
console.log('\nunion empty-encoding matrix: all cases passed');
