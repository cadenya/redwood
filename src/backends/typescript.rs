//! TypeScript backend: emits a dependency-less, strictly-typed SDK on top of
//! the vendored runtime in `runtime/typescript/`.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use heck::{ToKebabCase, ToLowerCamelCase, ToUpperCamelCase};

use crate::backends::{Backend, FileSet};
use crate::config::TypeScriptConfig;
use crate::ir::*;

const RT_ERROR: &str = include_str!("../../runtime/typescript/error.ts");
const RT_HTTP: &str = include_str!("../../runtime/typescript/http.ts");
const RT_PAGINATION: &str = include_str!("../../runtime/typescript/pagination.ts");
const RT_SSE: &str = include_str!("../../runtime/typescript/sse.ts");
const RT_WEBHOOKS: &str = include_str!("../../runtime/typescript/webhooks.ts");

pub struct TypeScriptBackend {
    pub config: TypeScriptConfig,
}

impl Backend for TypeScriptBackend {
    fn name(&self) -> &'static str {
        "typescript"
    }

    fn generate(&self, api: &Api) -> anyhow::Result<FileSet> {
        let mut files = FileSet::new();
        files.insert("src/core/error.ts".into(), RT_ERROR.to_string());
        files.insert("src/core/http.ts".into(), RT_HTTP.to_string());
        files.insert("src/core/pagination.ts".into(), RT_PAGINATION.to_string());
        files.insert("src/core/sse.ts".into(), RT_SSE.to_string());
        files.insert("src/types.ts".into(), emit_types(api));
        for resource in &api.resources {
            files.insert(
                format!("src/resources/{}.ts", resource.ident.to_kebab_case()),
                emit_resource(api, resource),
            );
        }
        if !api.webhooks.is_empty() {
            files.insert("src/core/webhooks.ts".into(), RT_WEBHOOKS.to_string());
            files.insert("src/webhooks.ts".into(), emit_webhooks(api));
        }
        files.insert("src/client.ts".into(), emit_client(api, &self.config));
        files.insert("src/index.ts".into(), emit_index(api));
        files.insert("package.json".into(), self.emit_package_json(api));
        files.insert("api.md".into(), emit_api_md(api));
        files.insert("README.md".into(), self.emit_readme(api));
        files.insert("tsconfig.json".into(), TSCONFIG.to_string());
        Ok(files)
    }
}

impl TypeScriptBackend {
    fn emit_readme(&self, api: &Api) -> String {
        let package = self
            .config
            .package_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase());
        let name = &api.name;
        let api_env = &api.api_key_env;
        let ws_env_note = api
            .client_params
            .first()
            .map(|c| format!(" (and {})", c.env_var))
            .unwrap_or_default();
        // Doc anchors are chosen STRUCTURALLY from the IR — backends know
        // nothing about any particular spec.
        let ops = || {
            api.resources
                .iter()
                .flat_map(|r| r.operations.iter().map(move |o| (r, o)))
        };
        let ts_accessor = |r: &Resource| -> String {
            r.path()
                .split('.')
                .map(|p| p.to_lower_camel_case())
                .collect::<Vec<_>>()
                .join(".")
        };
        // Selectors return None when the IR lacks the capability — a README
        // must never advertise phantom resources or signature-invalid calls.
        let retrieve_example = ops()
            .find(|(_, o)| {
                o.positionals.is_empty()
                    && o.body_fields.is_empty()
                    && o.whole_body.is_none()
                    && o.pagination.is_none()
                    && o.query_params.iter().all(|q| !q.required)
                    && matches!(o.response, ResponseKind::Json(_))
            })
            .map(|(r, o)| {
                format!(
                    "client.{}.{}()",
                    ts_accessor(r),
                    o.name.to_lower_camel_case()
                )
            });
        let list_example = ops()
            .find(|(_, o)| {
                o.pagination.is_some()
                    && o.positionals.is_empty()
                    && o.query_params.iter().all(|q| !q.required)
            })
            .map(|(r, o)| {
                format!(
                    "client.{}.{}()",
                    ts_accessor(r),
                    o.name.to_lower_camel_case()
                )
            });
        let stream_anchor = ops()
            .find(|(_, o)| matches!(o.response, ResponseKind::Sse(_)) && o.pagination.is_none())
            .map(|(r, o)| {
                let pos: String = o
                    .positionals
                    .iter()
                    .map(|p| format!("{}, ", p.wire_name.to_lower_camel_case()))
                    .collect();
                (
                    format!("client.{}.{}", ts_accessor(r), o.name.to_lower_camel_case()),
                    pos,
                )
            });
        let getting_started_call = retrieve_example
            .clone()
            .map(|e| format!("const result = await {e};"))
            .unwrap_or_else(|| "// See api.md for every method signature.".to_string());
        let pagination_section = list_example
            .map(|list| {
                format!(
                    "\n## Pagination\n\n```ts\nconst page = await {list};\nfor await (const item of page) {{\n  // auto-fetches every page\n}}\n```\n"
                )
            })
            .unwrap_or_default();
        let raw_response_section = retrieve_example
        .clone()
        .map(|call| {
            format!(
                "\n## Raw response access\n\nPlain methods return an `APIPromise`: awaiting yields the decoded value;\n`.withResponse()` pairs it with the `Response` (status, headers);\n`.asResponse()` — called synchronously, before any `await` — yields the raw\n`Response` with the body UNCONSUMED, so you own reading it:\n\n```ts\nconst {{ data, response }} = await {call}.withResponse();\nconsole.log(response.status, response.headers.get('x-request-id'));\n\nconst raw = await {call}.asResponse();\nconst body = await raw.text();\n```\n\nA dropped (never-awaited) call still cleans up after itself: the response\nis consumed and the request deadline is released automatically.\n"
            )
        })
        .unwrap_or_default();
        let logging_section = format!(
        "\n## Logging\n\n```ts\nconst client = new {name}({{ logLevel: 'debug' }}); // or logger: myLogger\n```\n\n`'warn'` (default) logs only retries; `'debug'` adds one line per request\nand response (method, path, status, duration); `'off'` silences the SDK.\nHeaders and bodies are NEVER logged.\n",
    );
        let streaming_section = stream_anchor
            .map(|(acc, pos)| {
                format!(
                    "\n## Streaming (SSE)\n\nA stream wraps one HTTP response body, so it can be consumed **once**, with\none of two views — pick whichever fits and iterate that one:\n\n```ts\n// View 1 (normal case): iterate decoded event payloads directly.\nconst stream = await {acc}({pos}params);\nfor await (const event of stream) {{\n  // event is the typed payload\n}}\n```\n\n```ts\n// View 2 (alternative): stream.events() keeps the SSE metadata envelope,\n// so the payload sits one level deeper: {{ event, data, id }}.\nconst envelopes = await {acc}({pos}params);\nfor await (const {{ data, id }} of envelopes.events()) {{\n  // ...\n}}\n```\n\nStreams RECONNECT AUTOMATICALLY on mid-stream transport drops (like\nEventSource): they resume from the last received event id, retry at most 5\ntimes per outage with backoff (honoring the server's `retry:` hint), and\nreset the budget once events flow again. A clean stream end, `close()`, and\na caller abort never reconnect; HTTP-level reconnect failures (e.g. expired\ncredentials) surface immediately. Opt out with `{{ reconnect: false }}` in\nrequest options — then drops raise `APIConnectionError` and you can resume\nmanually:\n\n```ts\nconst resumed = await {acc}({pos}params, {{\n  lastEventId: stream.lastEventId,\n}});\ntry {{\n  for await (const event of resumed) {{\n    // ...\n  }}\n}} finally {{\n  await resumed.close();\n}}\n```\n\nConsume (or close) every opened stream — an unconsumed stream holds its\nconnection until `close()`.\n"
                )
            })
            .unwrap_or_default();
        let webhooks_section = if api.webhooks.is_empty() {
            String::new()
        } else {
            format!(
                "\n## Webhooks (no API key required)\n\n```ts\nimport {{ Webhooks }} from '{package}';\n\nconst webhooks = new Webhooks(process.env.{wh_env});\nconst event = await webhooks.unwrap(payload, request.headers);\n```\n\nVerification follows Standard Webhooks (24–64 byte decoded secrets,\ninteger timestamps, bounded tolerance). This verifies the signature; it is\nnot runtime schema validation of the payload shape.\n",
                wh_env = api.webhook_env
            )
        };
        let env_comment = if matches!(api.auth, Auth::None) {
            "// This API requires no authentication.".to_string()
        } else {
            format!(
                "// Reads {api_env}{ws_env_note} from the environment when\n// options are omitted. Explicit blank values are configuration errors."
            )
        };
        format!(
            r#"# {name} TypeScript SDK

The official TypeScript client for the {name} API. Generated by redwood.
Dependency-free ESM built on native `fetch`.

## Install

```sh
npm install {package}
```

## Getting started

```ts
import {name} from '{package}';

{env_comment}
const client = new {name}();

{getting_started_call}
```

## Errors

Non-2xx responses throw `APIError` (google.rpc Status: `status`, `code`,
`message`, `details`). Connection failures throw `APIConnectionError`;
user aborts throw `APIUserAbortError`; a request that cannot be
constructed locally (unserializable body) throws `APIRequestError` with
no network attempt and no retry.

## Timeouts

Ordinary requests have a 60s deadline (through body decode) that throws
`APITimeoutError`; configure client-wide with `timeout` or per request
with `options.timeout` (<= 0 disables). Streams bound only response-header
acquisition — body lifetime stays under your `AbortSignal`.

## Retries

Automatic retries apply only to idempotent methods (GET/HEAD/PUT/DELETE)
and default to 0. Enable client-wide with `maxRetries`; opt a single
mutation in per request with `options.maxRetries`. Retry counts normalize
to a bounded 0–10 integer; `Retry-After` (seconds or HTTP-date) is honored
and backoff sleeps wake on abort.
{pagination_section}{raw_response_section}{logging_section}{streaming_section}{webhooks_section}
## Reference

See [api.md](api.md) for every method signature.
"#,
        )
    }

    fn emit_package_json(&self, api: &Api) -> String {
        let name = self
            .config
            .package_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase());
        let version = self
            .config
            .package_version
            .clone()
            .unwrap_or_else(|| "0.1.0".to_string());
        let repository = self
            .config
            .repository
            .as_deref()
            .map(|url| format!("\n  \"repository\": {{ \"type\": \"git\", \"url\": \"{url}\" }},"))
            .unwrap_or_default();
        let license = self
            .config
            .license
            .clone()
            .unwrap_or_else(|| "UNLICENSED".to_string());
        // Internal packages: npm refuses to publish `"private": true`, and
        // publishConfig would be an attractive nuisance on one.
        let publish = if self.config.private {
            "\"private\": true,"
        } else {
            "\"publishConfig\": { \"access\": \"public\" },"
        };
        format!(
            r#"{{
  "name": "{name}",
  "version": "{version}",
  "description": "The official TypeScript SDK for the {api_name} API",
  "license": "{license}",
  "type": "module",
  "main": "dist/index.js",
  "types": "dist/index.d.ts",
  "exports": {{
    ".": {{
      "types": "./dist/index.d.ts",
      "import": "./dist/index.js"
    }}
  }},
  "sideEffects": false,
  {publish}{repository}
  "engines": {{
    "node": ">=18"
  }},
  "files": ["dist", "src", "api.md"],
  "scripts": {{
    "build": "tsc",
    "typecheck": "tsc --noEmit"
  }},
  "dependencies": {{}},
  "devDependencies": {{
    "typescript": "^5.5.0"
  }}
}}
"#,
            api_name = api.name,
        )
    }
}

const TSCONFIG: &str = r#"{
  "compilerOptions": {
    "target": "ES2022",
    "lib": ["ES2022", "DOM", "DOM.AsyncIterable"],
    "module": "NodeNext",
    "moduleResolution": "NodeNext",
    "strict": true,
    "declaration": true,
    "declarationMap": true,
    "sourceMap": true,
    "inlineSources": true,
    "outDir": "dist",
    "rootDir": "src",
    "skipLibCheck": true,
    "forceConsistentCasingInFileNames": true
  },
  "include": ["src"]
}
"#;

// ---- type expressions ----------------------------------------------------

fn ts_ty(ty: &Ty) -> String {
    match ty {
        Ty::String | Ty::Timestamp | Ty::Bytes => "string".into(),
        Ty::Bool => "boolean".into(),
        Ty::Int32 | Ty::Int64 | Ty::Float | Ty::Double => "number".into(),
        Ty::Json => "unknown".into(),
        Ty::Literal(v) => format!("'{}'", v.replace('\'', "\\'")),
        Ty::Named(n) => type_name(n),
        Ty::List(inner) => format!("Array<{}>", ts_ty(inner)),
        Ty::Map(inner) => format!("Record<string, {}>", ts_ty(inner)),
    }
}

/// Request-direction annotation: a type whose input and output views differ
/// (itself or transitively) is referenced through its generated `XParam`
/// input interface; everything else shares the response type.
fn ts_input_ty(api: &Api, divergent: &std::collections::BTreeSet<String>, ty: &Ty) -> String {
    match ty {
        Ty::Named(n) if divergent.contains(n) => match api.types.get(n).map(|d| &d.shape) {
            Some(Shape::Struct(_) | Shape::Union(_) | Shape::Alias(_)) => {
                format!("{}Param", type_name(n))
            }
            _ => ts_ty(ty),
        },
        Ty::List(inner) => format!("Array<{}>", ts_input_ty(api, divergent, inner)),
        Ty::Map(inner) => format!("Record<string, {}>", ts_input_ty(api, divergent, inner)),
        _ => ts_ty(ty),
    }
}

/// Input-view (`XParam`) declarations for request-reachable types whose
/// views differ. readOnly fields are omitted entirely — a server-owned
/// field must never be accepted (or required) as request input.
fn emit_input_decls(api: &Api, out: &mut String) {
    let divergent = api.divergent_types();
    let request_types = api.request_reachable();
    let needed: Vec<&TypeDecl> = api
        .types
        .values()
        .filter(|d| divergent.contains(&d.name) && request_types.contains(&d.name))
        .collect();
    if needed.is_empty() {
        return;
    }
    writeln!(
        out,
        "
// ---- request input shapes (direction-aware) ----
"
    )
    .unwrap();
    for decl in needed {
        let name = format!("{}Param", type_name(&decl.name));
        match &decl.shape {
            Shape::Struct(st) => {
                writeln!(out, "export interface {name} {{").unwrap();
                for field in st.input_fields() {
                    let ty = ts_input_ty(api, &divergent, &field.ty);
                    emit_field(out, "  ", field, Some(ty));
                }
                writeln!(out, "}}").unwrap();
            }
            Shape::Union(u) => {
                let members: Vec<String> = u
                    .variants
                    .iter()
                    .map(|v| {
                        let base = ts_input_ty(api, &divergent, &v.ty);
                        graft_tag(api, u, v, base)
                    })
                    .collect();
                writeln!(out, "export type {name} = {};", members.join(" | ")).unwrap();
            }
            Shape::Alias(ty) => {
                writeln!(
                    out,
                    "export type {name} = {};",
                    ts_input_ty(api, &divergent, ty)
                )
                .unwrap();
            }
            Shape::Enum(_) => {}
        }
        out.push('\n');
    }
}

/// Types that transitively CONTAIN a readOnly field in input position:
/// exactly these need a runtime wire projection. The XParam interfaces are
/// compile-time only — TypeScript's structural assignability (and plain
/// JavaScript callers) let output-shaped values reach the serializer, so
/// fetched-modify-resubmit values must be projected at runtime.
fn readonly_tainted(api: &Api) -> std::collections::BTreeSet<String> {
    use std::collections::BTreeSet;
    fn ty_refs(ty: &Ty, out: &mut Vec<String>) {
        match ty {
            Ty::Named(n) => out.push(n.clone()),
            Ty::List(inner) | Ty::Map(inner) => ty_refs(inner, out),
            _ => {}
        }
    }
    let mut tainted: BTreeSet<String> = BTreeSet::new();
    let mut edges: Vec<(String, String)> = Vec::new();
    for decl in api.types.values() {
        let mut refs: Vec<String> = Vec::new();
        match &decl.shape {
            Shape::Struct(st) => {
                if st.fields.iter().any(|f| f.read_only) {
                    tainted.insert(decl.name.clone());
                }
                for f in &st.fields {
                    ty_refs(&f.ty, &mut refs);
                }
                if let Some(additional) = &st.additional {
                    ty_refs(additional, &mut refs);
                }
            }
            Shape::Union(u) => {
                for v in &u.variants {
                    ty_refs(&v.ty, &mut refs);
                }
            }
            Shape::Alias(ty) => ty_refs(ty, &mut refs),
            Shape::Enum(_) => {}
        }
        for r in refs {
            edges.push((decl.name.clone(), r));
        }
    }
    loop {
        let before = tainted.len();
        for (referrer, referenced) in &edges {
            if tainted.contains(referenced) {
                tainted.insert(referrer.clone());
            }
        }
        if tainted.len() == before {
            return tainted;
        }
    }
}

/// Wire-projection function names a resource file must import (value
/// imports — projectors run at request time).
fn wire_fns_for_resource(api: &Api, resource: &Resource) -> std::collections::BTreeSet<String> {
    use std::collections::BTreeSet;
    let tainted = readonly_tainted(api);
    let mut fns: BTreeSet<String> = BTreeSet::new();
    fn walk(
        api: &Api,
        tainted: &BTreeSet<String>,
        ty: &Ty,
        fns: &mut BTreeSet<String>,
        _depth: usize,
    ) {
        match ty {
            Ty::Named(n) if tainted.contains(n) => {
                if matches!(
                    api.types.get(n).map(|d| &d.shape),
                    Some(Shape::Struct(_) | Shape::Union(_) | Shape::Alias(_))
                ) {
                    fns.insert(wire_fn_name(n));
                }
            }
            Ty::List(inner) => {
                if ts_wire_expr(api, tainted, inner, "_v").is_some() {
                    fns.insert("wireArray".to_string());
                    walk(api, tainted, inner, fns, _depth + 1);
                }
            }
            Ty::Map(inner) if ts_wire_expr(api, tainted, inner, "_v").is_some() => {
                fns.insert("wireMap".to_string());
                walk(api, tainted, inner, fns, _depth + 1);
            }
            _ => {}
        }
    }
    for op in &resource.operations {
        for f in &op.body_fields {
            walk(api, &tainted, &f.ty, &mut fns, 0);
        }
        if let Some(ty) = &op.whole_body {
            walk(api, &tainted, ty, &mut fns, 0);
        }
    }
    fns
}

fn wire_fn_name(n: &str) -> String {
    format!("wire{}", type_name(n))
}

/// Member access on a projector's `value` for a wire property.
fn member(owner: &str, wire: &str) -> String {
    if is_ident(wire) {
        format!("{owner}.{wire}")
    } else {
        format!("{owner}[{}]", serde_json::to_string(wire).unwrap())
    }
}

/// Runtime projection expression for a request-position value of `ty`, or
/// None when the type carries no readOnly content anywhere.
fn ts_wire_expr(
    api: &Api,
    tainted: &std::collections::BTreeSet<String>,
    ty: &Ty,
    expr: &str,
) -> Option<String> {
    match ty {
        Ty::Named(n) if tainted.contains(n) => match api.types.get(n).map(|d| &d.shape) {
            Some(Shape::Struct(_) | Shape::Union(_) | Shape::Alias(_)) => {
                Some(format!("{}({expr})", wire_fn_name(n)))
            }
            _ => None,
        },
        Ty::List(inner) => ts_wire_expr(api, tainted, inner, "_v")
            .map(|ie| format!("wireArray({expr}, (_v) => {ie})")),
        Ty::Map(inner) => ts_wire_expr(api, tainted, inner, "_v")
            .map(|ie| format!("wireMap({expr}, (_v) => {ie})")),
        _ => None,
    }
}

/// Wire projectors: runtime input serializers for every request-reachable
/// type that transitively contains a readOnly field. They strip readOnly
/// keys recursively WITHOUT mutating the caller's objects, retain writeOnly
/// input fields, preserve undefined omission and wire casing, and keep
/// unknown keys only on genuinely open (additionalProperties) structs.
fn emit_wire_projectors(api: &Api, out: &mut String) {
    let tainted = readonly_tainted(api);
    let request_types = api.request_reachable();
    let needed: Vec<&TypeDecl> = api
        .types
        .values()
        .filter(|d| tainted.contains(&d.name) && request_types.contains(&d.name))
        .collect();
    if needed.is_empty() {
        return;
    }
    writeln!(
        out,
        "\n// ---- request wire projections (strip readOnly at runtime) ----\n"
    )
    .unwrap();
    writeln!(out, "export function wireArray<T>(value: Array<T> | null | undefined, fn: (v: T) => unknown): unknown {{").unwrap();
    writeln!(out, "  return value == null ? value : value.map(fn);").unwrap();
    writeln!(out, "}}\n").unwrap();
    writeln!(out, "export function wireMap<T>(value: Record<string, T> | null | undefined, fn: (v: T) => unknown): unknown {{").unwrap();
    writeln!(out, "  return value == null ? value : Object.fromEntries(Object.entries(value).map(([k, v]) => [k, fn(v)]));").unwrap();
    writeln!(out, "}}\n").unwrap();
    for decl in needed {
        let fn_name = wire_fn_name(&decl.name);
        let param_ty = format!("{}Param", type_name(&decl.name));
        match &decl.shape {
            Shape::Struct(st) => {
                writeln!(
                    out,
                    "export function {fn_name}(value: {param_ty} | null | undefined): unknown {{"
                )
                .unwrap();
                writeln!(
                    out,
                    "  if (value == null || typeof value !== 'object') return value;"
                )
                .unwrap();
                if st.additional.is_some() {
                    // Open struct: unknown keys are part of the contract —
                    // keep them, remove readOnly keys, project known fields.
                    writeln!(out, "  const out: Record<string, unknown> = {{ ...(value as never as Record<string, unknown>) }};").unwrap();
                    for f in st.fields.iter().filter(|f| f.read_only) {
                        writeln!(
                            out,
                            "  delete out[{}];",
                            serde_json::to_string(&f.wire_name).unwrap()
                        )
                        .unwrap();
                    }
                    for f in st.fields.iter().filter(|f| !f.read_only) {
                        if let Some(e) = ts_wire_expr(
                            api,
                            &readonly_tainted(api),
                            &f.ty,
                            &member("value", &f.wire_name),
                        ) {
                            let key = serde_json::to_string(&f.wire_name).unwrap();
                            writeln!(
                                out,
                                "  if ({m} !== undefined) out[{key}] = {e};",
                                m = member("value", &f.wire_name)
                            )
                            .unwrap();
                        }
                    }
                } else {
                    // Closed struct: copy known input fields only.
                    writeln!(out, "  const out: Record<string, unknown> = {{}};").unwrap();
                    for f in st.input_fields() {
                        let m = member("value", &f.wire_name);
                        let key = serde_json::to_string(&f.wire_name).unwrap();
                        let value = ts_wire_expr(api, &readonly_tainted(api), &f.ty, &m)
                            .unwrap_or_else(|| m.clone());
                        writeln!(out, "  if ({m} !== undefined) out[{key}] = {value};").unwrap();
                    }
                }
                writeln!(out, "  return out;").unwrap();
                writeln!(out, "}}\n").unwrap();
            }
            Shape::Union(u) => {
                writeln!(
                    out,
                    "export function {fn_name}(value: {param_ty} | null | undefined): unknown {{"
                )
                .unwrap();
                writeln!(
                    out,
                    "  if (value == null || typeof value !== 'object') return value;"
                )
                .unwrap();
                match &u.discriminator {
                    Some(disc) => {
                        writeln!(
                            out,
                            "  switch ((value as never as Record<string, unknown>)[{}]) {{",
                            serde_json::to_string(&disc.property).unwrap()
                        )
                        .unwrap();
                        for v in &u.variants {
                            let Some(tag) = &v.tag else { continue };
                            if let Some(e) =
                                ts_wire_expr(api, &readonly_tainted(api), &v.ty, "(value as never)")
                            {
                                writeln!(
                                    out,
                                    "    case {}: return {e};",
                                    serde_json::to_string(tag).unwrap()
                                )
                                .unwrap();
                            }
                        }
                        writeln!(out, "    default: return value;").unwrap();
                        writeln!(out, "  }}").unwrap();
                    }
                    None => {
                        // Undiscriminated: copy the union of every struct
                        // variant's input fields (readOnly keys drop out).
                        writeln!(out, "  const out: Record<string, unknown> = {{}};").unwrap();
                        let mut seen: std::collections::BTreeSet<String> =
                            std::collections::BTreeSet::new();
                        for v in &u.variants {
                            let Ty::Named(vn) = &v.ty else { continue };
                            let Some(Shape::Struct(vs)) = api.types.get(vn).map(|d| &d.shape)
                            else {
                                continue;
                            };
                            for f in vs.input_fields() {
                                if !seen.insert(f.wire_name.clone()) {
                                    continue;
                                }
                                let m = member(
                                    "(value as never as Record<string, unknown>)",
                                    &f.wire_name,
                                );
                                let key = serde_json::to_string(&f.wire_name).unwrap();
                                let value = ts_wire_expr(
                                    api,
                                    &readonly_tainted(api),
                                    &f.ty,
                                    &format!("({m} as never)"),
                                )
                                .unwrap_or_else(|| m.clone());
                                writeln!(out, "  if ({m} !== undefined) out[{key}] = {value};")
                                    .unwrap();
                            }
                        }
                        writeln!(out, "  return out;").unwrap();
                    }
                }
                writeln!(out, "}}\n").unwrap();
            }
            Shape::Alias(ty) => {
                let body = ts_wire_expr(api, &readonly_tainted(api), ty, "value")
                    .unwrap_or_else(|| "value".to_string());
                writeln!(
                    out,
                    "export function {fn_name}(value: {param_ty} | null | undefined): unknown {{"
                )
                .unwrap();
                writeln!(out, "  return value == null ? value : {body};").unwrap();
                writeln!(out, "}}\n").unwrap();
            }
            Shape::Enum(_) => {}
        }
    }
}

/// Spec names are already PascalCase-ish; sanitize anything that would not
/// be a valid TS identifier.
fn type_name(name: &str) -> String {
    if is_ident(name) {
        name.to_string()
    } else {
        name.to_upper_camel_case()
    }
}

fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

fn prop_key(name: &str) -> String {
    if is_ident(name) {
        name.to_string()
    } else {
        format!("'{}'", name.replace('\'', "\\'"))
    }
}

fn prop_access(target: &str, name: &str, optional: bool) -> String {
    let dot = if optional { "?." } else { "." };
    if is_ident(name) {
        format!("{target}{dot}{name}")
    } else if optional {
        format!("{target}?.['{name}']")
    } else {
        format!("{target}['{name}']")
    }
}

fn doc(out: &mut String, indent: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let safe = text.replace("*/", "*\\/");
    writeln!(out, "{indent}/**").unwrap();
    for line in safe.lines() {
        writeln!(out, "{indent} * {}", line.trim_end()).unwrap();
    }
    writeln!(out, "{indent} */").unwrap();
}

// ---- types.ts --------------------------------------------------------------

fn emit_types(api: &Api) -> String {
    let mut out = String::from("// Generated by redwood. Do not edit by hand.\n\n");
    for decl in api.types.values() {
        emit_type_decl(api, &mut out, decl);
        out.push('\n');
    }
    emit_input_decls(api, &mut out);
    emit_wire_projectors(api, &mut out);
    out
}

fn emit_type_decl(api: &Api, out: &mut String, decl: &TypeDecl) {
    if let Some(d) = &decl.description {
        doc(out, "", d);
    }
    let name = type_name(&decl.name);
    match &decl.shape {
        Shape::Struct(s) => {
            if s.fields.is_empty() {
                if let Some(additional) = &s.additional {
                    writeln!(
                        out,
                        "export type {name} = Record<string, {}>;",
                        ts_ty(additional)
                    )
                    .unwrap();
                    return;
                }
            }
            writeln!(out, "export interface {name} {{").unwrap();
            // Response direction: writeOnly fields are input secrets and are
            // never promised (or exposed) in output models.
            for field in s.output_fields() {
                emit_field(out, "  ", field, None);
            }
            writeln!(out, "}}").unwrap();
        }
        Shape::Enum(e) => {
            let variants: Vec<String> = e
                .values
                .iter()
                .map(|v| format!("'{}'", v.replace('\'', "\\'")))
                .collect();
            writeln!(out, "export type {name} = {};", variants.join(" | ")).unwrap();
        }
        Shape::Union(u) => {
            let members: Vec<String> = u.variants.iter().map(|v| union_member(api, u, v)).collect();
            writeln!(out, "export type {name} =").unwrap();
            for (i, member) in members.iter().enumerate() {
                let end = if i + 1 == members.len() { ";" } else { "" };
                writeln!(out, "  | {member}{end}").unwrap();
            }
        }
        Shape::Alias(ty) => {
            writeln!(out, "export type {name} = {};", ts_ty(ty)).unwrap();
        }
    }
}

/// A discriminated variant whose struct does not declare the tag property
/// gets it grafted on, so the union still narrows.
fn union_member(api: &Api, union: &UnionShape, variant: &UnionVariant) -> String {
    let base = ts_ty(&variant.ty);
    let (Some(disc), Some(tag)) = (&union.discriminator, &variant.tag) else {
        return base;
    };
    let declares_tag = match &variant.ty {
        Ty::Named(n) => match api.types.get(n).map(|d| &d.shape) {
            Some(Shape::Struct(s)) => s.fields.iter().any(|f| f.wire_name == disc.property),
            _ => true,
        },
        _ => true,
    };
    if declares_tag {
        base
    } else {
        format!("({base} & {{ {}: '{tag}' }})", prop_key(&disc.property))
    }
}

/// Same tag-grafting rule, applied to an already-chosen base annotation
/// (used by the input view, where the base may be an `XParam` type).
fn graft_tag(api: &Api, union: &UnionShape, variant: &UnionVariant, base: String) -> String {
    let (Some(disc), Some(tag)) = (&union.discriminator, &variant.tag) else {
        return base;
    };
    let declares_tag = match &variant.ty {
        Ty::Named(n) => match api.types.get(n).map(|d| &d.shape) {
            Some(Shape::Struct(s)) => s.input_fields().any(|f| f.wire_name == disc.property),
            _ => true,
        },
        _ => true,
    };
    if declares_tag {
        base
    } else {
        format!("({base} & {{ {}: '{tag}' }})", prop_key(&disc.property))
    }
}

fn emit_field(out: &mut String, indent: &str, field: &Field, input_ty: Option<String>) {
    if let Some(d) = &field.description {
        doc(out, indent, d);
    }
    let optional = if field.required { "" } else { "?" };
    let nullable = if field.nullable { " | null" } else { "" };
    writeln!(
        out,
        "{indent}{}{optional}: {}{nullable};",
        prop_key(&field.wire_name),
        input_ty.unwrap_or_else(|| ts_ty(&field.ty)),
    )
    .unwrap();
}

// ---- resources -------------------------------------------------------------

/// Resource class name, collision-safe against the model namespace: a
/// schema named `Product` alongside a `products` resource is ordinary
/// OpenAPI naming, and both live in the package root — the resource class
/// yields (`ProductResource`) so the model keeps the natural name.
fn class_name(api: &Api, resource: &Resource) -> String {
    let candidate = resource.ident.to_upper_camel_case();
    if api
        .types
        .keys()
        .any(|t| t.to_upper_camel_case() == candidate)
    {
        format!("{candidate}Resource")
    } else {
        candidate
    }
}

fn params_type_name(api_resource: &Resource, op: &Operation) -> String {
    let singular: String = api_resource
        .ident
        .split('_')
        .map(singularize)
        .collect::<Vec<_>>()
        .join("_")
        .to_upper_camel_case();
    format!("{singular}{}Params", op.name.to_upper_camel_case())
}

fn singularize(word: &str) -> String {
    if word.ends_with("ics") {
        // Uncountable: analytics, metrics, statistics.
        word.to_string()
    } else if let Some(stem) = word.strip_suffix("ies") {
        format!("{stem}y")
    } else if word.len() > 1 && word.ends_with('s') && !word.ends_with("ss") {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

/// Collect every Named type referenced by a resource's surface.
fn named_refs(api: &Api, resource: &Resource) -> BTreeSet<String> {
    let divergent = api.divergent_types();
    let mut names = BTreeSet::new();
    // Request positions reference the INPUT view name for divergent types.
    fn visit_input(divergent: &BTreeSet<String>, names: &mut BTreeSet<String>, ty: &Ty) {
        let mut raw = BTreeSet::new();
        collect_raw_named(ty, &mut raw);
        for n in raw {
            if divergent.contains(&n) {
                names.insert(format!("{}Param", type_name(&n)));
            } else {
                names.insert(type_name(&n));
            }
        }
    }
    for op in &resource.operations {
        for p in &op.positionals {
            collect_named(&p.ty, &mut names);
        }
        for p in op.path_params.iter().chain(op.query_params.iter()) {
            collect_named(&p.ty, &mut names);
        }
        for f in &op.body_fields {
            visit_input(&divergent, &mut names, &f.ty);
        }
        if let Some(ty) = &op.whole_body {
            visit_input(&divergent, &mut names, ty);
        }
        match &op.response {
            ResponseKind::Json(ty) | ResponseKind::Sse(ty) => collect_named(ty, &mut names),
            ResponseKind::Empty => {}
        }
        if let Some(page) = &op.pagination {
            collect_named(&page.item_ty, &mut names);
        }
    }
    names
}

fn collect_named(ty: &Ty, out: &mut BTreeSet<String>) {
    match ty {
        Ty::Named(n) => {
            out.insert(type_name(n));
        }
        Ty::List(inner) | Ty::Map(inner) => collect_named(inner, out),
        _ => {}
    }
}

/// Raw IR names (pre-sanitization), for divergence lookups keyed by IR name.
fn collect_raw_named(ty: &Ty, out: &mut BTreeSet<String>) {
    match ty {
        Ty::Named(n) => {
            out.insert(n.clone());
        }
        Ty::List(inner) | Ty::Map(inner) => collect_raw_named(inner, out),
        _ => {}
    }
}

fn client_param<'a>(api: &'a Api, wire_name: &str) -> Option<&'a ClientParam> {
    api.client_params.iter().find(|c| c.wire_name == wire_name)
}

/// Params stay required in the signature only when some member has no
/// client-level fallback.
fn params_arg_required(api: &Api, op: &Operation) -> bool {
    op.path_params
        .iter()
        .chain(op.query_params.iter())
        .any(|p| p.required && client_param(api, &p.wire_name).is_none())
        || op.body_fields.iter().any(|f| f.required)
        || op.whole_body.is_some()
}

fn emit_resource(api: &Api, resource: &Resource) -> String {
    let mut out = String::from("// Generated by redwood. Do not edit by hand.\n\n");
    let has_sse = resource
        .operations
        .iter()
        .any(|o| matches!(o.response, ResponseKind::Sse(_)));
    let has_pages = resource.operations.iter().any(|o| o.pagination.is_some());
    // Nested resources hang off this one as readonly properties.
    let children: Vec<&Resource> = api
        .resources
        .iter()
        .filter(|r| r.parent.as_deref() == Some(resource.name.as_str()))
        .collect();

    let has_path_params = resource.operations.iter().any(|o| o.path.contains('{'));
    let needs_snapshot = resource
        .operations
        .iter()
        .any(|o| o.pagination.is_some() && o.has_params());
    let mut http_imports = vec!["HttpClient", "RequestOptions"];
    if resource
        .operations
        .iter()
        .any(|o| matches!((&o.pagination, &o.response), (None, ResponseKind::Sse(_))))
    {
        http_imports.push("RequestSpec");
    }
    let has_api_promise = resource.operations.iter().any(|o| {
        matches!(
            (&o.pagination, &o.response),
            (None, ResponseKind::Json(_)) | (None, ResponseKind::Empty)
        )
    });
    if has_api_promise {
        http_imports.push("APIPromise");
    }
    if has_path_params {
        http_imports.push("pathSegment");
    }
    if needs_snapshot {
        http_imports.push("snapshotParams");
    }
    writeln!(
        out,
        "import {{ {} }} from '../core/http.js';",
        http_imports.join(", ")
    )
    .unwrap();
    if has_pages {
        writeln!(out, "import {{ Page }} from '../core/pagination.js';").unwrap();
    }
    if has_sse {
        writeln!(out, "import {{ Stream }} from '../core/sse.js';").unwrap();
    }
    for child in &children {
        writeln!(
            out,
            "import {{ {} }} from './{}.js';",
            class_name(api, child),
            child.ident.to_kebab_case()
        )
        .unwrap();
    }
    let refs = named_refs(api, resource);
    if !refs.is_empty() {
        let list = refs.into_iter().collect::<Vec<_>>().join(", ");
        writeln!(out, "import type {{ {list} }} from '../types.js';").unwrap();
    }
    let wire_fns = wire_fns_for_resource(api, resource);
    if !wire_fns.is_empty() {
        let list = wire_fns.into_iter().collect::<Vec<_>>().join(", ");
        writeln!(out, "import {{ {list} }} from '../types.js';").unwrap();
    }
    out.push('\n');

    // Params interfaces first, so the class reads top-down.
    for op in &resource.operations {
        if op.has_params() {
            emit_params_interface(api, &mut out, resource, op);
            out.push('\n');
        }
    }

    if let Some(d) = &resource.description {
        doc(&mut out, "", d);
    }
    writeln!(out, "export class {} {{", class_name(api, resource)).unwrap();
    for child in &children {
        writeln!(
            out,
            "  readonly {}: {};",
            child.name.to_lower_camel_case(),
            class_name(api, child)
        )
        .unwrap();
    }
    if children.is_empty() {
        writeln!(
            out,
            "  constructor(private readonly _client: HttpClient) {{}}"
        )
        .unwrap();
    } else {
        writeln!(
            out,
            "  constructor(private readonly _client: HttpClient) {{"
        )
        .unwrap();
        for child in &children {
            writeln!(
                out,
                "    this.{} = new {}(_client);",
                child.name.to_lower_camel_case(),
                class_name(api, child)
            )
            .unwrap();
        }
        writeln!(out, "  }}").unwrap();
    }
    for op in &resource.operations {
        out.push('\n');
        emit_method(api, &mut out, resource, op);
    }
    writeln!(out, "}}").unwrap();
    out
}

fn emit_params_interface(api: &Api, out: &mut String, resource: &Resource, op: &Operation) {
    writeln!(
        out,
        "export interface {} {{",
        params_type_name(resource, op)
    )
    .unwrap();
    // Required data reads first: two passes so docs/IDE hover lead with what
    // the caller must supply (property order carries no runtime meaning).
    for required_pass in [true, false] {
        for p in op.path_params.iter().chain(op.query_params.iter()) {
            let fallback = client_param(api, &p.wire_name);
            let is_required = p.required && fallback.is_none();
            if is_required != required_pass {
                continue;
            }
            let mut description = p.description.clone().unwrap_or_default();
            if let Some(c) = fallback {
                if !description.is_empty() {
                    description.push_str("\n\n");
                }
                description.push_str(&format!(
                    "Defaults to the client-level `{}` option or the {} environment variable.",
                    c.wire_name.to_lower_camel_case(),
                    c.env_var
                ));
            }
            if !description.is_empty() {
                doc(out, "  ", &description);
            }
            let optional = if is_required { "" } else { "?" };
            writeln!(
                out,
                "  {}{optional}: {};",
                prop_key(&p.wire_name),
                ts_ty(&p.ty)
            )
            .unwrap();
        }
        for f in &op.body_fields {
            if f.required != required_pass {
                continue;
            }
            // Request direction: divergent types reference their input view.
            let input = ts_input_ty(api, &api.divergent_types(), &f.ty);
            emit_field(out, "  ", f, Some(input));
        }
        if required_pass {
            if let Some(ty) = &op.whole_body {
                doc(out, "  ", "The request body, sent as-is.");
                writeln!(
                    out,
                    "  body: {};",
                    ts_input_ty(api, &api.divergent_types(), ty)
                )
                .unwrap();
            }
        }
    }
    writeln!(out, "}}").unwrap();
}

/// Compact TS literal for a sample value: identifier keys unquoted,
/// strings single-quoted — reads like handwritten code in IDE hovers.
fn ts_example_value(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => {
            format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
        }
        serde_json::Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(ts_example_value)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        serde_json::Value::Object(map) => {
            let entries: Vec<String> = map
                .iter()
                .map(|(k, val)| {
                    let key = if is_ident(k) {
                        k.clone()
                    } else {
                        format!("'{k}'")
                    };
                    format!("{key}: {}", ts_example_value(val))
                })
                .collect();
            format!("{{ {} }}", entries.join(", "))
        }
        other => other.to_string(),
    }
}

/// A runnable usage snippet for the method, embedded as @example JSDoc.
/// Arguments mirror the real signature: positional sample ids, then a params
/// object holding only the REQUIRED non-client fields (client defaults stay
/// invisible, like idiomatic call sites).
fn ts_doc_example(api: &Api, resource: &Resource, op: &Operation) -> String {
    use super::openapi_export::path_sample;
    let accessor = resource
        .path()
        .split('.')
        .map(|p| p.to_lower_camel_case())
        .collect::<Vec<_>>()
        .join(".");
    let method = op.name.to_lower_camel_case();
    let mut args: Vec<String> = Vec::new();
    for p in &op.positionals {
        args.push(ts_example_value(&path_sample(api, p)));
    }
    let mut entries: Vec<String> = Vec::new();
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        if !p.required || client_param(api, &p.wire_name).is_some() {
            continue;
        }
        entries.push(format!(
            "{}: {}",
            prop_key(&p.wire_name),
            ts_example_value(&path_sample(api, p))
        ));
    }
    for f in op.body_fields.iter().filter(|f| f.required) {
        entries.push(format!(
            "{}: {}",
            prop_key(&f.wire_name),
            ts_example_value(&super::manifest_sample(api, &f.ty))
        ));
    }
    if let Some(ty) = &op.whole_body {
        entries.push(format!(
            "body: {}",
            ts_example_value(&super::manifest_sample(api, ty))
        ));
    }
    if !entries.is_empty() {
        args.push(format!("{{ {} }}", entries.join(", ")));
    }
    let call = format!("client.{accessor}.{method}({})", args.join(", "));
    let result_var = match (&op.pagination, &op.response) {
        (Some(_), _) => "page".to_string(),
        (None, ResponseKind::Json(Ty::Named(n))) => {
            let v = type_name(n).to_lower_camel_case();
            if v.is_empty() {
                "result".to_string()
            } else {
                v
            }
        }
        (None, ResponseKind::Json(_)) => "result".to_string(),
        (None, ResponseKind::Sse(_)) => "stream".to_string(),
        (None, ResponseKind::Empty) => String::new(),
    };
    match (&op.pagination, &op.response) {
        (Some(_), _) => format!(
            "```ts\nconst page = await {call};\nfor await (const item of page) {{\n  // auto-fetches every page\n}}\n```"
        ),
        (None, ResponseKind::Sse(_)) => format!(
            "```ts\nconst stream = await {call};\nfor await (const event of stream) {{\n  // typed event payloads; housekeeping frames are skipped\n}}\n```"
        ),
        (None, ResponseKind::Empty) => format!("```ts\nawait {call};\n```"),
        _ => format!("```ts\nconst {result_var} = await {call};\n```"),
    }
}

fn emit_method(api: &Api, out: &mut String, resource: &Resource, op: &Operation) {
    let mut docs = String::new();
    if let Some(s) = op.summary.as_deref().or(op.description.as_deref()) {
        docs.push_str(s);
    }
    if !docs.is_empty() {
        docs.push_str("\n\n");
    }
    docs.push_str("@example\n");
    docs.push_str(&ts_doc_example(api, resource, op));
    if op.deprecated {
        docs.push_str("\n\n@deprecated");
    }
    doc(out, "  ", &docs);

    let method = op.name.to_lower_camel_case();
    let params_ty = params_type_name(resource, op);
    let params_optional = op.has_params() && !params_arg_required(api, op);
    let params_target = if params_optional { "params?" } else { "params" };

    let mut args: Vec<String> = Vec::new();
    for p in &op.positionals {
        args.push(format!(
            "{}: {}",
            p.wire_name.to_lower_camel_case(),
            ts_ty(&p.ty)
        ));
    }
    if op.has_params() {
        args.push(format!("{}: {params_ty}", params_target));
    }
    args.push("options?: RequestOptions".to_string());

    let return_ty = match (&op.pagination, &op.response) {
        (Some(page), _) => format!("Page<{}>", ts_ty(&page.item_ty)),
        (None, ResponseKind::Json(ty)) => ts_ty(ty),
        (None, ResponseKind::Sse(ty)) => format!("Stream<{}>", ts_ty(ty)),
        (None, ResponseKind::Empty) => "void".to_string(),
    };

    // Plain JSON and void operations return APIPromise (lazily parsed, with
    // asResponse/withResponse raw access); pagination and SSE stay async.
    let api_promise = matches!(
        (&op.pagination, &op.response),
        (None, ResponseKind::Json(_)) | (None, ResponseKind::Empty)
    );
    if api_promise {
        writeln!(
            out,
            "  {method}({}): APIPromise<{return_ty}> {{",
            args.join(", ")
        )
        .unwrap();
    } else {
        writeln!(
            out,
            "  async {method}({}): Promise<{return_ty}> {{",
            args.join(", ")
        )
        .unwrap();
    }

    // Resolve client-level defaults into locals used by path/query below.
    // Captured separately: APIPromise methods run this inside the request
    // thunk so setup failures reject rather than throw synchronously.
    let preamble = |indent: &str| -> String {
        let mut lines = String::new();
        for p in op.path_params.iter().chain(op.query_params.iter()) {
            let Some(c) = client_param(api, &p.wire_name) else {
                continue;
            };
            let local = p.wire_name.to_lower_camel_case();
            // Trimmed truthiness, not `??`: empty or whitespace strings are
            // configuration mistakes, never values to serialize into a path.
            writeln!(
                lines,
                "{indent}const {local} = String({} ?? this._client.defaults['{}'] ?? '').trim() || undefined;",
                prop_access("params", &p.wire_name, params_optional),
                c.wire_name,
            )
            .unwrap();
            if p.required {
                writeln!(
                    lines,
                    "{indent}if ({local} === undefined) throw new Error(\"Missing '{}': pass it in params, set it on the client, or set the {} environment variable.\");",
                    c.wire_name, c.env_var,
                )
                .unwrap();
            }
        }
        lines
    };

    let path_expr = build_path_expr(api, op, params_optional);
    let mut spec_lines = vec![
        format!("method: '{}'", op.http_method.as_str()),
        format!("path: {path_expr}"),
    ];
    if !op.query_params.is_empty() {
        let entries: Vec<String> = op
            .query_params
            .iter()
            .map(|p| {
                let value = if client_param(api, &p.wire_name).is_some() {
                    p.wire_name.to_lower_camel_case()
                } else {
                    prop_access("params", &p.wire_name, params_optional)
                };
                format!("{}: {}", prop_key(&p.wire_name), value)
            })
            .collect();
        spec_lines.push(format!("query: {{ {} }}", entries.join(", ")));
    }
    if !op.body_fields.is_empty() {
        let tainted = readonly_tainted(api);
        let entries: Vec<String> = op
            .body_fields
            .iter()
            .map(|f| {
                let access = prop_access("params", &f.wire_name, params_optional);
                // Runtime wire projection: readOnly content must not reach
                // the serializer even from output-shaped or untyped values.
                let value = ts_wire_expr(api, &tainted, &f.ty, &access).unwrap_or(access);
                format!("{}: {}", prop_key(&f.wire_name), value)
            })
            .collect();
        spec_lines.push(format!("body: {{ {} }}", entries.join(", ")));
    }
    if let Some(ty) = &op.whole_body {
        // A whole-body params member is required, so params is never optional.
        let value = ts_wire_expr(api, &readonly_tainted(api), ty, "params.body")
            .unwrap_or_else(|| "params.body".to_string());
        spec_lines.push(format!("body: {value}"));
    }

    match (&op.pagination, &op.response) {
        (Some(page), ResponseKind::Json(response_ty)) => {
            out.push_str(&preamble("    "));
            let response_ts = ts_ty(response_ty);
            if op.has_params() {
                // Snapshot params NOW (arrays copied): later caller mutation
                // must not leak into page-2+ fetches and mix result sets.
                writeln!(out, "    const _base = snapshotParams(params);").unwrap();
            }
            writeln!(
                out,
                "    const response = await this._client.request<{response_ts}>({{ {} }}, options);",
                spec_lines.join(", ")
            )
            .unwrap();
            let items = format!("response.{} ?? []", page.items_field);
            let cursor = cursor_access(&page.next_cursor_path);
            let refetch_params = if op.has_params() {
                format!("{{ ..._base, {}: cursor }}", prop_key(&page.cursor_param))
            } else {
                format!("{{ {}: cursor }}", prop_key(&page.cursor_param))
            };
            let mut refetch_args: Vec<String> = Vec::new();
            for p in &op.positionals {
                refetch_args.push(p.wire_name.to_lower_camel_case());
            }
            refetch_args.push(refetch_params);
            refetch_args.push("options".to_string());
            writeln!(
                out,
                "    return new Page({items}, {cursor}, (cursor) => this.{method}({}));",
                refetch_args.join(", ")
            )
            .unwrap();
        }
        (None, ResponseKind::Json(ty)) => {
            writeln!(
                out,
                "    return this._client.requestAPI<{}>(() => {{",
                ts_ty(ty)
            )
            .unwrap();
            out.push_str(&preamble("      "));
            writeln!(out, "      return {{ {} }};", spec_lines.join(", ")).unwrap();
            writeln!(out, "    }}, options);").unwrap();
        }
        (None, ResponseKind::Sse(ty)) => {
            out.push_str(&preamble("    "));
            spec_lines.push("stream: true".to_string());
            writeln!(
                out,
                "    const _spec: RequestSpec = {{ {} }};",
                spec_lines.join(", ")
            )
            .unwrap();
            writeln!(
                out,
                "    const response = await this._client.rawRequest(_spec, options);"
            )
            .unwrap();
            // Auto-reconnect (EventSource semantics) unless opted out: the
            // closure re-issues the SAME request with the resume checkpoint.
            let skip_list = if api.sse_skip_events.is_empty() {
                "[]".to_string()
            } else {
                format!(
                    "[{}]",
                    api.sse_skip_events
                        .iter()
                        .map(|e| format!("'{e}'"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            writeln!(
                out,
                "    return new Stream<{}>(response, options?.signal, options?.lastEventId, {skip_list}, options?.reconnect === false ? undefined : (lastEventId, signal) => this._client.rawRequest(_spec, {{ ...options, lastEventId, signal }}));",
                ts_ty(ty)
            )
            .unwrap();
        }
        (None, ResponseKind::Empty) => {
            // Declared-void: 204/empty succeed (output-bearing ops reject them).
            let mut void_spec = spec_lines.clone();
            void_spec.push("void: true".to_string());
            writeln!(out, "    return this._client.requestAPI<void>(() => {{").unwrap();
            out.push_str(&preamble("      "));
            writeln!(out, "      return {{ {} }};", void_spec.join(", ")).unwrap();
            writeln!(out, "    }}, options);").unwrap();
        }
        (Some(_), _) => unreachable!("pagination implies a JSON response"),
    }
    writeln!(out, "  }}").unwrap();
}

/// `/v1/parents/{parentId}/children/{childId}` -> a template
/// literal interpolating encoded params.
fn build_path_expr(api: &Api, op: &Operation, params_optional: bool) -> String {
    let mut expr = String::from("`");
    let mut rest = op.path.as_str();
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|e| start + e)
            .expect("balanced braces");
        expr.push_str(&rest[..start]);
        let param = &rest[start + 1..end];
        let is_positional = op.positionals.iter().any(|p| p.wire_name == param);
        let access = if is_positional || client_param(api, param).is_some() {
            param.to_lower_camel_case()
        } else {
            prop_access("params", param, params_optional)
        };
        // pathSegment validates non-empty at the boundary: JavaScript callers
        // can pass "" or whitespace despite the types, and an empty segment
        // silently rewrites the route (/parents//children).
        write!(expr, "${{pathSegment('{param}', {access})}}").unwrap();
        rest = &rest[end + 1..];
    }
    expr.push_str(rest);
    expr.push('`');
    expr
}

/// "pagination.nextCursor" -> `response.pagination?.nextCursor`
fn cursor_access(path: &str) -> String {
    let mut out = String::from("response");
    for (i, segment) in path.split('.').enumerate() {
        if i == 0 {
            out = prop_access(&out, segment, false);
        } else {
            out = prop_access(&out, segment, true);
        }
    }
    out
}

// ---- webhooks ----------------------------------------------------------------

fn webhook_event_type_name(name: &str) -> String {
    format!("{}WebhookEvent", name.to_upper_camel_case())
}

fn webhook_env_var(api: &Api) -> String {
    api.webhook_env.clone()
}

fn emit_webhooks(api: &Api) -> String {
    let mut out = String::from("// Generated by redwood. Do not edit by hand.\n\n");
    writeln!(
        out,
        "import {{ WebhookVerifier, WebhookHeaders, WebhookVerifyOptions }} from './core/webhooks.js';"
    )
    .unwrap();
    let mut refs = BTreeSet::new();
    for webhook in &api.webhooks {
        collect_named(&webhook.payload, &mut refs);
    }
    if !refs.is_empty() {
        let list = refs.into_iter().collect::<Vec<_>>().join(", ");
        writeln!(out, "import type {{ {list} }} from './types.js';").unwrap();
    }
    out.push('\n');

    for webhook in &api.webhooks {
        let docs = webhook
            .summary
            .clone()
            .or_else(|| webhook.description.clone())
            .unwrap_or_default();
        doc(&mut out, "", &docs);
        let payload = ts_ty(&webhook.payload);
        match &webhook.discriminator_field {
            Some(field) => writeln!(
                out,
                "export type {} = {payload} & {{ {}: '{}' }};",
                webhook_event_type_name(&webhook.name),
                prop_key(field),
                webhook.name,
            )
            .unwrap(),
            None => writeln!(
                out,
                "export type {} = {payload};",
                webhook_event_type_name(&webhook.name),
            )
            .unwrap(),
        }
        out.push('\n');
    }

    writeln!(out, "export type UnwrapWebhookEvent =").unwrap();
    for (i, webhook) in api.webhooks.iter().enumerate() {
        let end = if i + 1 == api.webhooks.len() { ";" } else { "" };
        writeln!(out, "  | {}{end}", webhook_event_type_name(&webhook.name)).unwrap();
    }

    write!(
        out,
        r#"
export class Webhooks {{
  constructor(private readonly secret: string | undefined) {{}}

  /**
   * Verify a Standard Webhooks delivery (webhook-id / webhook-timestamp /
   * webhook-signature headers) and return its typed payload.
   */
  async unwrap(
    payload: string,
    headers: WebhookHeaders,
    options?: WebhookVerifyOptions & {{ secret?: string }},
  ): Promise<UnwrapWebhookEvent> {{
    const secret = (options?.secret ?? this.secret)?.trim();
    if (!secret) {{
      throw new Error(
        "Missing webhook secret. Pass {{ secret }}, construct the client with webhookSecret, or set the {env_var} environment variable.",
      );
    }}
    await new WebhookVerifier(secret).verify(payload, headers, options);
    return JSON.parse(payload) as UnwrapWebhookEvent;
  }}
}}
"#,
        env_var = webhook_env_var(api),
    )
    .unwrap();
    out
}

// ---- client.ts / index.ts --------------------------------------------------

fn env_var_name(api: &Api) -> String {
    api.api_key_env.clone()
}

fn client_extra_options(api: &Api) -> String {
    let mut out = String::new();
    for c in &api.client_params {
        writeln!(
            out,
            "  /** Default `{}` for every call that takes one. Defaults to the {} environment variable. */",
            c.wire_name.to_lower_camel_case(),
            c.env_var
        )
        .unwrap();
        writeln!(out, "  {}?: string;", c.wire_name.to_lower_camel_case()).unwrap();
    }
    if !api.webhooks.is_empty() {
        writeln!(
            out,
            "  /** Secret for webhook signature verification. Defaults to the {} environment variable. */",
            webhook_env_var(api)
        )
        .unwrap();
        writeln!(out, "  webhookSecret?: string;").unwrap();
    }
    out
}

fn client_defaults_entries(api: &Api) -> String {
    api.client_params
        .iter()
        .map(|c| {
            format!(
                "{}: resolveOption('{}', options.{}, readEnv('{}'))",
                c.wire_name,
                c.wire_name.to_lower_camel_case(),
                c.wire_name.to_lower_camel_case(),
                c.env_var
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn emit_client(api: &Api, config: &crate::config::TypeScriptConfig) -> String {
    let mut out = String::from("// Generated by redwood. Do not edit by hand.\n\n");
    writeln!(out, "import {{ HttpClient }} from './core/http.js';").unwrap();
    writeln!(
        out,
        "import type {{ Logger, LogLevel }} from './core/http.js';"
    )
    .unwrap();
    if !api.webhooks.is_empty() {
        writeln!(out, "import {{ Webhooks }} from './webhooks.js';").unwrap();
    }
    for resource in api.resources.iter().filter(|r| r.parent.is_none()) {
        writeln!(
            out,
            "import {{ {} }} from './resources/{}.js';",
            class_name(api, resource),
            resource.ident.to_kebab_case()
        )
        .unwrap();
    }
    let base_env = format!("{}_BASE_URL", api.name.to_uppercase());
    write!(
        out,
        r#"
export interface ClientOptions {{
{api_key_option}  /** Override the API base URL. Defaults to {base_url}. */
  baseURL?: string;
  /** Max automatic retries for retryable failures. Defaults to {max_retries}. */
  maxRetries?: number;
  /**
   * Deadline for ordinary (non-streaming) requests in milliseconds; override
   * per request with `options.timeout`. Streams bound only response-header
   * acquisition — body lifetime stays under the caller's AbortSignal.
   * Defaults to 60000; a non-finite or <= 0 value disables the deadline.
   */
  timeout?: number;
  /** Headers sent with every request. */
  defaultHeaders?: Record<string, string>;
  /** Custom fetch implementation. */
  fetch?: typeof fetch;
  /** Destination for SDK logs. Defaults to `console`. */
  logger?: Logger;
  /** 'debug' | 'warn' (default) | 'off'. Never logs headers or bodies. */
  logLevel?: LogLevel;
{extra_options}}}

export class {name} {{
"#,
        base_url = api.base_url,
        name = api.name,
        extra_options = client_extra_options(api),
        max_retries = api.max_retries,
        api_key_option = if matches!(api.auth, Auth::None) {
            String::new()
        } else {
            format!(
                "  /** API key. Defaults to the {env_var} environment variable. */\n  apiKey?: string;\n",
                env_var = env_var_name(api)
            )
        },
    )
    .unwrap();
    for resource in api.resources.iter().filter(|r| r.parent.is_none()) {
        writeln!(
            out,
            "  readonly {}: {};",
            resource.name.to_lower_camel_case(),
            class_name(api, resource)
        )
        .unwrap();
    }
    if !api.webhooks.is_empty() {
        writeln!(out, "  readonly webhooks: Webhooks;").unwrap();
    }
    // Optional-auth arrows carry a return-type annotation: without it the
    // ternary's branches infer as a union whose empty arm is incompatible
    // with Record<string, string>.
    let auth_header = match (&api.auth, api.auth_optional) {
        (Auth::Bearer, false) => "() => ({ Authorization: `Bearer ${apiKey}` })".to_string(),
        (Auth::Bearer, true) => {
            "(): Record<string, string> => (apiKey ? { Authorization: `Bearer ${apiKey}` } : {})"
                .to_string()
        }
        (Auth::ApiKeyHeader(header), false) => format!("() => ({{ '{header}': apiKey }})"),
        (Auth::ApiKeyHeader(header), true) => {
            format!("(): Record<string, string> => (apiKey ? {{ '{header}': apiKey }} : {{}})")
        }
        (Auth::None, _) => "() => ({})".to_string(),
    };
    write!(
        out,
        r#"
  private readonly _client: HttpClient;

  constructor(options: ClientOptions = {{}}) {{
{api_key_resolve}    this._client = new HttpClient({{
      baseURL: resolveOption('baseURL', options.baseURL, readEnv('{base_env}')) ?? '{base_url}',
      authHeader: {auth_header},
      maxRetries: options.maxRetries ?? {max_retries},
      timeout: options.timeout,
      defaultHeaders: {{ 'User-Agent': '{user_agent}', ...options.defaultHeaders }},
      fetch: options.fetch,
      logger: options.logger,
      logLevel: options.logLevel,
      defaults: {{ {defaults} }},
    }});
"#,
        base_url = api.base_url,
        defaults = client_defaults_entries(api),
        max_retries = api.max_retries,
        api_key_resolve = if matches!(api.auth, Auth::None) {
            // No security scheme: the client constructs unauthenticated and
            // demands no credential (a required-but-unused key would make
            // every public-API SDK unusable).
            String::new()
        } else if api.auth_optional {
            // Mixed public/private surface: a missing key is a legal state
            // (public endpoints), so nothing throws here — protected
            // endpoints answer 401 instead. An explicit '' opts out of the
            // environment fallback: "anonymous on purpose", not a mistake.
            format!(
                "    const apiKey = (options.apiKey ?? readEnv('{env_var}') ?? '').trim();\n",
                env_var = env_var_name(api)
            )
        } else {
            format!(
                "    // Presence is not validity: empty strings from options or environment\n    // are configuration mistakes, not credentials.\n    const apiKey = resolveOption('apiKey', options.apiKey, readEnv('{env_var}'));\n    if (!apiKey) {{\n      throw new Error(\n        \"Missing API key. Pass it with `new {name}({{ apiKey }})` or set the {env_var} environment variable.\",\n      );\n    }}\n",
                env_var = env_var_name(api),
                name = api.name
            )
        },
        user_agent = format!(
            "{}-typescript/{} (api {})",
            api.name.to_lowercase(),
            config.package_version.as_deref().unwrap_or("0.1.0"),
            api.version
        ),
    )
    .unwrap();
    if !api.webhooks.is_empty() {
        writeln!(
            out,
            "    this.webhooks = new Webhooks(resolveOption('webhookSecret', options.webhookSecret, readEnv('{}')));",
            webhook_env_var(api)
        )
        .unwrap();
    }
    for resource in api.resources.iter().filter(|r| r.parent.is_none()) {
        writeln!(
            out,
            "    this.{} = new {}(this._client);",
            resource.name.to_lower_camel_case(),
            class_name(api, resource)
        )
        .unwrap();
    }
    write!(
        out,
        r#"  }}
}}

function readEnv(name: string): string | undefined {{
  const env = (globalThis as {{ process?: {{ env?: Record<string, string | undefined> }} }})
    .process?.env;
  return env?.[name];
}}

// Presence is not validity: an explicitly provided option must be usable —
// a blank value is a configuration mistake and never silently falls back to
// the environment or a default. Only an OMITTED option reads the env.
function resolveOption(
  label: string,
  explicit: string | undefined,
  env: string | undefined,
): string | undefined {{
  if (explicit !== undefined) {{
    const value = explicit.trim();
    if (!value) {{
      throw new Error(
        `${{label}} must not be blank when provided explicitly; omit it to use the environment instead.`,
      );
    }}
    return value;
  }}
  return env?.trim() || undefined;
}}
"#
    )
    .unwrap();
    out
}

/// Native reference: dotted accessor + real TS signature per operation.
fn emit_api_md(api: &Api) -> String {
    let mut out = format!(
        "# {} TypeScript SDK reference\n\nPlain methods return an awaitable APIPromise (with `.withResponse()` and\n`.asResponse()` for raw Response access); pagination and streaming methods\nreturn a Promise of a Page or Stream. See README.md for usage patterns.\n",
        api.name
    );
    for resource in &api.resources {
        let accessor: String = resource
            .path()
            .split('.')
            .map(|part| part.to_lower_camel_case())
            .collect::<Vec<_>>()
            .join(".");
        writeln!(out, "\n## {accessor}\n").unwrap();
        for op in &resource.operations {
            if let Some(s) = &op.summary {
                writeln!(out, "{}\n", s.trim().lines().next().unwrap_or("")).unwrap();
            }
            let mut args: Vec<String> = Vec::new();
            for p in &op.positionals {
                args.push(format!(
                    "{}: {}",
                    p.wire_name.to_lower_camel_case(),
                    ts_ty(&p.ty)
                ));
            }
            if op.has_params() {
                let optional = if params_arg_required(api, op) {
                    ""
                } else {
                    "?"
                };
                args.push(format!(
                    "params{optional}: {}",
                    params_type_name(resource, op)
                ));
            }
            args.push("options?: RequestOptions".to_string());
            let return_ty = match (&op.pagination, &op.response) {
                (Some(page), _) => format!("Page<{}>", ts_ty(&page.item_ty)),
                (None, ResponseKind::Json(ty)) => ts_ty(ty),
                (None, ResponseKind::Sse(ty)) => format!("Stream<{}>", ts_ty(ty)),
                (None, ResponseKind::Empty) => "void".to_string(),
            };
            let wrapper = match (&op.pagination, &op.response) {
                (None, ResponseKind::Json(_)) | (None, ResponseKind::Empty) => "APIPromise",
                _ => "Promise",
            };
            writeln!(
                out,
                "```ts\nclient.{accessor}.{}({}): {wrapper}<{return_ty}>\n```",
                op.name.to_lower_camel_case(),
                args.join(", ")
            )
            .unwrap();
        }
    }
    out
}

fn emit_index(api: &Api) -> String {
    let mut out = String::from("// Generated by redwood. Do not edit by hand.\n\n");
    writeln!(
        out,
        "export {{ {name}, {name} as default }} from './client.js';",
        name = api.name
    )
    .unwrap();
    writeln!(out, "export type {{ ClientOptions }} from './client.js';").unwrap();
    writeln!(out, "export {{ APIError, APIConnectionError, APIUserAbortError, APIRequestError, APIResponseError, APITimeoutError }} from './core/error.js';").unwrap();
    writeln!(
        out,
        "export type {{ RequestOptions }} from './core/http.js';"
    )
    .unwrap();
    writeln!(out, "export {{ APIPromise }} from './core/http.js';\nexport {{ Page }} from './core/pagination.js';").unwrap();
    writeln!(out, "export {{ Stream }} from './core/sse.js';").unwrap();
    writeln!(
        out,
        "export type {{ ServerSentEvent }} from './core/sse.js';"
    )
    .unwrap();
    writeln!(out, "export * from './types.js';").unwrap();
    if !api.webhooks.is_empty() {
        writeln!(out, "export * from './webhooks.js';").unwrap();
        writeln!(
            out,
            "export {{ WebhookVerifier, WebhookVerificationError }} from './core/webhooks.js';"
        )
        .unwrap();
        writeln!(
            out,
            "export type {{ WebhookHeaders, WebhookVerifyOptions }} from './core/webhooks.js';"
        )
        .unwrap();
    }
    for resource in &api.resources {
        writeln!(
            out,
            "export * from './resources/{}.js';",
            resource.ident.to_kebab_case()
        )
        .unwrap();
    }
    out
}
