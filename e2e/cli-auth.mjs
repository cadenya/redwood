// Durable gate: the generated CLI's RFC 8628 device-authorization login.
// Generates a CLI against a mock authorization server (endpoints are baked
// at generation time, so the mock binds its port FIRST), then walks the
// whole lifecycle: status while logged out, login honoring
// authorization_pending and slow_down, credentials written 0600 with the
// workspaces extension stored, status while logged in, logout, and a login
// that ends in expired_token.
//
// Run: node e2e/cli-auth.mjs   (needs REDWOOD_BIN or target/debug/redwood + gen/go)
import { createServer } from 'node:http';
import { execFileSync, execFile } from 'node:child_process';
import { mkdtempSync, readFileSync, writeFileSync, statSync, existsSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const root = new URL('..', import.meta.url).pathname;
const failures = [];
const assert = (cond, label) => { if (!cond) failures.push(label); };

// ---- mock authorization server -------------------------------------------------

let tokenPolls = [];   // timestamps of token polls for the current session
let mode = 'approve';  // 'approve' walks pending -> slow_down -> approved; 'expire' fails
let issueToken = 'apikey_jwt_never_shown'; // token minted on approval (re-login rotates it)
let redeemed = false;

const server = createServer(async (req, res) => {
  let body = '';
  for await (const chunk of req) body += chunk;
  const form = new URLSearchParams(body);
  const send = (status, obj) => {
    res.statusCode = status;
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify(obj));
  };

  if (req.url === '/device_authorization') {
    assert(req.headers['content-type']?.startsWith('application/x-www-form-urlencoded'), 'device_authorization content-type');
    assert(form.get('client_id') === 'test-cli', `client_id was ${form.get('client_id')}`);
    assert((form.get('device_name') ?? '') !== '', 'device_name missing');
    tokenPolls = [];
    redeemed = false;
    send(200, {
      device_code: 'dev_secret_1',
      user_code: 'GHKQ-PZWM',
      verification_uri: 'https://example.test/cli/authorize',
      verification_uri_complete: 'https://example.test/cli/authorize?code=GHKQ-PZWM',
      expires_in: 900,
      interval: 1,
    });
    return;
  }
  if (req.url === '/token') {
    assert(form.get('grant_type') === 'urn:ietf:params:oauth:grant-type:device_code', 'grant_type');
    assert(form.get('device_code') === 'dev_secret_1', 'device_code echoed');
    tokenPolls.push(Date.now());
    if (mode === 'expire') return send(400, { error: 'expired_token' });
    switch (tokenPolls.length) {
      case 1: return send(400, { error: 'authorization_pending' });
      case 2: return req.socket.destroy(); // mid-poll connection reset: the CLI must keep polling
      case 3: return send(400, { error: 'slow_down' });
      case 4: {
        if (redeemed) return send(400, { error: 'expired_token' });
        redeemed = true;
        return send(200, {
          access_token: issueToken,
          token_type: 'bearer',
          workspaces: [{ id: 'ws_prod', name: 'Production' }],
        });
      }
      default: return send(400, { error: 'expired_token' });
    }
  }
  send(404, { error: 'invalid_request' });
});
server.keepAliveTimeout = 60_000; // deterministic: only the explicit reset below drops a poll
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const base = `http://127.0.0.1:${server.address().port}`;

// ---- generate + build the fixture CLI ------------------------------------------

const fixtureConfig = join(mkdtempSync(join(tmpdir(), 'redwood-cli-auth-')), 'auth.toml');
// redwood.toml may already carry a production [lang.cli.auth] table; strip
// it (through the next table header) so the appended mock one is the only
// definition.
const baseConfig = readFileSync(join(root, 'redwood.toml'), 'utf8')
  .replace(/^\[lang\.cli\.auth\]$[^]*?(?=^\[|\n*$(?![^]))/m, '');
writeFileSync(
  fixtureConfig,
  baseConfig +
    `\n[lang.cli.auth]\n` +
    `device_authorization_endpoint = "${base}/device_authorization"\n` +
    `token_endpoint = "${base}/token"\n` +
    `client_id = "test-cli"\n` +
    `workspaces_param = "workspaceId"\n`,
);
const outDir = join(root, 'gen/cli-auth-e2e'); // sibling of gen/go for the ../go replace
rmSync(outDir, { recursive: true, force: true });
const redwoodBin = process.env.REDWOOD_BIN ?? join(root, 'target/debug/redwood');
execFileSync(redwoodBin, [
  '--spec', join(root, 'api-spec.yml'),
  '--config', fixtureConfig,
  '--language', 'cli',
  '--out', outDir,
], { stdio: 'inherit' });
const bin = join(mkdtempSync(join(tmpdir(), 'redwood-cli-auth-bin-')), 'cadenya');
execFileSync('go', ['build', '-o', bin, '.'], { cwd: outDir, timeout: 300_000 });

// ---- lifecycle ------------------------------------------------------------------

const home = mkdtempSync(join(tmpdir(), 'redwood-cli-auth-home-'));
// Ambient CADENYA_* variables (a real key in the developer's shell) would
// outrank the stored login — correct resolution, wrong test conditions.
const env = Object.fromEntries(
  Object.entries({ ...process.env, HOME: home }).filter(([k]) => !k.startsWith('CADENYA_')),
);
const run = (args) =>
  new Promise((resolve) => {
    execFile(bin, args, { env, timeout: 120_000 }, (err, stdout, stderr) =>
      resolve({ code: err?.code ?? 0, stdout, stderr }),
    );
  });

// 1) logged out
{
  const { code, stdout } = await run(['auth', 'status']);
  assert(code === 0, 'status (logged out) exit code');
  assert(stdout.includes('not logged in'), `status before login: ${stdout}`);
  assert(stdout.includes('Credential source: none'), `status (none) source: ${stdout}`);
}

// 2) login: pending -> slow_down -> approved
{
  const { code, stdout, stderr } = await run(['auth', 'login', '--no-browser']);
  assert(code === 0, `login exit ${code}`);
  assert(stdout.includes('GHKQ-PZWM'), 'user code printed');
  assert(!stdout.includes('dev_secret_1'), 'device_code leaked to output');
  assert(!stdout.includes('apikey_jwt_never_shown'), 'credential leaked to output');
  assert(stdout.includes('Logged in'), `login output: ${stdout}`);
  assert(tokenPolls.length === 4, `token polls: ${tokenPolls.length}`);
  // slow_down honored: the gap after poll 3 must include the +5s penalty.
  const gap = tokenPolls[3] - tokenPolls[2];
  assert(gap >= 4000, `slow_down not honored (gap ${gap}ms)`);

  const credPath = join(home, '.cadenya', 'credentials');
  assert(existsSync(credPath), 'credentials file exists');
  const st = statSync(credPath);
  assert((st.mode & 0o777) === 0o600, `credentials mode ${(st.mode & 0o777).toString(8)}`);
  const creds = readFileSync(credPath, 'utf8');
  assert(creds.includes('apikey_jwt_never_shown'), 'credential stored');
  assert(creds.includes('ws_prod') && creds.includes('Production'), 'workspaces stored verbatim');
}

// 3) status logged in shows workspace ids only
{
  const { stdout } = await run(['auth', 'status']);
  assert(stdout.includes('logged in'), `status after login: ${stdout}`);
  assert(stdout.includes('ws_prod'), 'workspace id shown');
  assert(!stdout.includes('apikey_jwt_never_shown'), 'status leaked credential');
}

// 3b) the stored login authenticates ordinary commands (newClient fallback),
// and the single authorized workspace becomes the workspaceId default.
{
  const seen = [];
  const apiMock = createServer((req, res) => {
    seen.push({ url: req.url, auth: req.headers.authorization ?? null });
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ items: [], pagination: {} }));
  });
  await new Promise((r) => apiMock.listen(0, '127.0.0.1', r));
  const { code, stderr } = await run(['--base-url', `http://127.0.0.1:${apiMock.address().port}`, 'agents', 'list']);
  apiMock.close();
  assert(code === 0, `agents list via stored login exit ${code}: ${stderr}`);
  assert(seen.length === 1, 'agents list reached the API');
  // Never echo the actual header on failure: with polluted test conditions
  // it could be a real credential.
  assert(seen[0]?.auth === 'Bearer apikey_jwt_never_shown', 'stored credential not sent');
  assert(seen[0]?.url.includes('ws_prod'), `workspace default not applied: ${seen[0]?.url}`);
}

// 3b2) `config set` persistent defaults: stored default beats the
// login-derived workspace; the environment beats the stored default; both
// spellings (flag and wire) name the same key; list names the source.
{
  const hit = () => new Promise(async (resolveHit) => {
    const seen = [];
    const apiMock = createServer((req, res) => {
      seen.push(req.url);
      res.setHeader('content-type', 'application/json');
      res.end(JSON.stringify({ items: [], pagination: {} }));
    });
    await new Promise((r) => apiMock.listen(0, '127.0.0.1', r));
    resolveHit({
      run: (extraEnv = {}) => new Promise((resolve) => {
        execFile(bin, ['--base-url', `http://127.0.0.1:${apiMock.address().port}`, 'agents', 'list'],
          { env: { ...env, ...extraEnv }, timeout: 60_000 },
          (err, stdout, stderr) => resolve({ code: err?.code ?? 0, stdout, stderr }));
      }),
      seen,
      close: () => apiMock.close(),
    });
  });

  const set = await run(['config', 'set', 'workspaceId', 'ws_cfg']); // wire spelling accepted
  assert(set.code === 0 && set.stdout.includes('Set workspace-id = ws_cfg'), `config set: ${set.stdout}`);

  const viaStored = await hit();
  const r1 = await viaStored.run();
  viaStored.close();
  assert(r1.code === 0, `list with stored default exit ${r1.code}: ${r1.stderr}`);
  assert(viaStored.seen[0]?.includes('ws_cfg'), `stored default not applied: ${viaStored.seen[0]}`);

  const viaEnv = await hit();
  const r2 = await viaEnv.run({ CADENYA_WORKSPACE_ID: 'ws_env' });
  viaEnv.close();
  assert(r2.code === 0, `list with env exit ${r2.code}: ${r2.stderr}`);
  assert(viaEnv.seen[0]?.includes('ws_env'), `env must beat stored default: ${viaEnv.seen[0]}`);

  const list = await run(['config', 'list']);
  assert(list.stdout.includes('ws_cfg') && list.stdout.includes('stored default'),
    `config list source: ${list.stdout}`);

  const unset = await run(['config', 'unset', 'workspace-id']);
  assert(unset.code === 0, `config unset exit ${unset.code}`);
  const after = await run(['config', 'list']);
  assert(after.stdout.includes('stored login (single authorized workspace)'),
    `login-derived source after unset: ${after.stdout}`);
}

// 3b3) --debug dumps the HTTP exchange with the credential REDACTED, and
// structured API error details print by default (no flag needed).
{
  const apiMock = createServer((req, res) => {
    res.statusCode = 400;
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ code: 3, message: 'validation failed', details: [
      { '@type': 'type.googleapis.com/google.rpc.BadRequest',
        fieldViolations: [{ field: 'spec.widgetId', description: 'widget_id is required' }] },
    ]}));
  });
  await new Promise((r) => apiMock.listen(0, '127.0.0.1', r));
  const base = ['--base-url', `http://127.0.0.1:${apiMock.address().port}`];
  // Required leaves are checked locally now, so the request must be
  // complete for the API's own validation error to be what comes back.
  const complete = ['widget-sessions', 'create', '--widget-id', 'w', '--tenant-id', 't', '--subject-id', 's'];
  const plain = await run([...base, ...complete]);
  assert(plain.code === 1, `create exit ${plain.code}`);
  assert(plain.stderr.includes('validation failed'), `error line: ${plain.stderr}`);
  assert(plain.stderr.includes('details:') && plain.stderr.includes('spec.widgetId'),
    `details shown by default: ${plain.stderr}`);
  const dbg = await run([...base, '--debug', ...complete]);
  apiMock.close();
  assert(dbg.stderr.includes('> POST ') && dbg.stderr.includes('< HTTP 400'),
    `debug dump present: ${dbg.stderr.slice(0, 400)}`);
  assert(dbg.stderr.includes('Authorization: [redacted]'), 'auth header redacted');
  assert(!dbg.stderr.includes('apikey_jwt_never_shown'), 'credential leaked into debug output');
}

// 3c) re-login over an EXISTING credentials file replaces the credential —
// a stale stored token must never survive a fresh approval.
{
  issueToken = 'apikey_jwt_rotated';
  const { code, stdout, stderr } = await run(['auth', 'login', '--no-browser']);
  assert(code === 0, `re-login exit ${code}: ${stdout} ${stderr}`);
  const creds = readFileSync(join(home, '.cadenya', 'credentials'), 'utf8');
  assert(creds.includes('apikey_jwt_rotated'), 're-login stored the new credential');
  assert(!creds.includes('apikey_jwt_never_shown'), 'stale credential still present after re-login');
}

// 3d) status names the EFFECTIVE credential source, so "why 403 after
// re-login" is answerable at a glance: env var > stored login, flag > env.
{
  const stored = await run(['auth', 'status']);
  assert(stored.stdout.includes('Credential source: stored login'),
    `status (stored) source: ${stored.stdout}`);
  const viaEnv = await new Promise((resolve) => {
    execFile(bin, ['auth', 'status'], { env: { ...env, CADENYA_API_KEY: 'env_key_zzz' }, timeout: 60_000 },
      (err, stdout, stderr) => resolve({ code: err?.code ?? 0, stdout, stderr }));
  });
  assert(viaEnv.stdout.includes('Credential source: $CADENYA_API_KEY'),
    `status (env) source: ${viaEnv.stdout}`);
  assert(viaEnv.stdout.includes('overrides the stored login'),
    `status (env) override warning: ${viaEnv.stdout}`);
  assert(!viaEnv.stdout.includes('env_key_zzz'), 'status leaked the env credential');
  const viaFlag = await run(['--api-key', 'flag_key_zzz', 'auth', 'status']);
  assert(viaFlag.stdout.includes('Credential source: --api-key flag'),
    `status (flag) source: ${viaFlag.stdout}`);
  assert(!viaFlag.stdout.includes('flag_key_zzz'), 'status leaked the flag credential');
}

// 3e) login warns when the env var will keep outranking the fresh login.
{
  issueToken = 'apikey_jwt_rotated_again';
  const { code, stdout } = await new Promise((resolve) => {
    execFile(bin, ['auth', 'login', '--no-browser'], { env: { ...env, CADENYA_API_KEY: 'env_key_zzz' }, timeout: 120_000 },
      (err, stdout, stderr) => resolve({ code: err?.code ?? 0, stdout: stdout + stderr, stderr }));
  });
  assert(code === 0, `login-under-env exit ${code}`);
  assert(stdout.includes('CADENYA_API_KEY is set') && stdout.includes('takes precedence'),
    `login-under-env note: ${stdout}`);
}

// 4) logout removes the profile (and the file, as the last profile)
{
  const { code, stdout } = await run(['auth', 'logout']);
  assert(code === 0, 'logout exit code');
  assert(stdout.includes('Logged out'), `logout output: ${stdout}`);
  assert(!existsSync(join(home, '.cadenya', 'credentials')), 'credentials file removed');
}

// 5) a session that expires server-side fails cleanly
{
  mode = 'expire';
  const { code, stdout, stderr } = await run(['auth', 'login', '--no-browser']);
  assert(code !== 0, 'expired login should fail');
  assert((stdout + stderr).includes('expired'), `expired login message: ${stderr}`);
}

// 6) CADENYA_AUTH_BASE_URL rebases both endpoints (preview-deploy testing):
// requests land on the override host with the original paths.
{
  let overrideHits = [];
  const override = createServer((req, res) => {
    overrideHits.push(req.url);
    res.statusCode = 400;
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ error: 'invalid_request' }));
  });
  await new Promise((r) => override.listen(0, '127.0.0.1', r));
  const { code } = await new Promise((resolve) => {
    execFile(
      bin,
      ['auth', 'login', '--no-browser'],
      { env: { ...env, CADENYA_AUTH_BASE_URL: `http://127.0.0.1:${override.address().port}` }, timeout: 60_000 },
      (err, stdout, stderr) => resolve({ code: err?.code ?? 0, stdout, stderr }),
    );
  });
  override.close();
  assert(code !== 0, 'override login should fail against the 400 stub');
  assert(overrideHits.length === 1 && overrideHits[0] === '/device_authorization',
    `override not honored: ${JSON.stringify(overrideHits)}`);
}

server.close();
rmSync(outDir, { recursive: true, force: true });
if (failures.length) {
  console.log(failures.join('\n'));
  console.error('cli auth gate: FAILED');
  process.exit(1);
}
console.log('cli auth gate: all cases passed');
