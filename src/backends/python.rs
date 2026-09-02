//! Python backend: emits an httpx-based SDK in the house style of the major
//! Python API clients — a `Cadenya` client with resource attributes, keyword
//! arguments per method, dataclass response models with generated decoders,
//! TypedDict request shapes, auto-iterating pages, and SSE streams.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use heck::{ToSnakeCase, ToUpperCamelCase};

use crate::backends::{Backend, FileSet};
use crate::config::PythonConfig;
use crate::ir::*;

const RT_CORE: &str = include_str!("../../runtime/python/_core.py");
const RT_PAGINATION: &str = include_str!("../../runtime/python/_pagination.py");
const RT_SSE: &str = include_str!("../../runtime/python/_sse.py");
const RT_WEBHOOKS: &str = include_str!("../../runtime/python/_webhooks.py");

pub struct PythonBackend {
    pub config: PythonConfig,
}

impl Backend for PythonBackend {
    fn name(&self) -> &'static str {
        "python"
    }

    fn generate(&self, api: &Api) -> anyhow::Result<FileSet> {
        let pkg = self.package_name(api);
        let mut files = FileSet::new();
        files.insert(format!("{pkg}/_core.py"), RT_CORE.to_string());
        files.insert(format!("{pkg}/_pagination.py"), RT_PAGINATION.to_string());
        files.insert(format!("{pkg}/_sse.py"), RT_SSE.to_string());
        if !api.webhooks.is_empty() {
            files.insert(format!("{pkg}/_webhooks.py"), RT_WEBHOOKS.to_string());
        }
        files.insert(format!("{pkg}/types.py"), emit_types(api));
        files.insert(format!("{pkg}/py.typed"), String::new());
        files.insert(
            format!("{pkg}/resources/__init__.py"),
            emit_resources_init(api),
        );
        for resource in &api.resources {
            files.insert(
                format!("{pkg}/resources/{}.py", resource.ident),
                emit_resource(api, resource),
            );
        }
        files.insert(
            format!("{pkg}/_client.py"),
            emit_client(api, &pkg, &self.config),
        );
        files.insert(format!("{pkg}/__init__.py"), emit_init(api));
        files.insert(
            "pyproject.toml".into(),
            emit_pyproject(api, &pkg, &self.config),
        );
        files.insert("conformance.py".into(), emit_conformance(api, &pkg));
        files.insert("api.md".into(), emit_api_md(api));
        files.insert("README.md".into(), emit_readme(api, &pkg));
        Ok(files)
    }
}

// ---- docs --------------------------------------------------------------------

/// Native reference: dotted accessor + real keyword signature per operation.
fn emit_api_md(api: &Api) -> String {
    let mut out = format!(
        "# {} Python SDK reference\n\nKeyword arguments are snake_case (nested request dicts too); see README.md for usage patterns.\n",
        api.name
    );
    for resource in &api.resources {
        writeln!(out, "\n## client.{}\n", resource.path()).unwrap();
        for op in &resource.operations {
            if let Some(s) = &op.summary {
                writeln!(out, "{}\n", s.trim().lines().next().unwrap_or("")).unwrap();
            }
            let mut args: Vec<String> = Vec::new();
            for p in &op.positionals {
                args.push(format!(
                    "{}: {}",
                    py_name(&p.wire_name),
                    py_param_ty(api, &p.ty, "types.", false)
                ));
            }
            let mut kwargs: Vec<String> = Vec::new();
            for p in op.path_params.iter().chain(op.query_params.iter()) {
                let required = p.required && client_param(api, &p.wire_name).is_none();
                let suffix = if required { "" } else { "=None" };
                kwargs.push(format!("{}{suffix}", py_name(&p.wire_name)));
            }
            for f in &op.body_fields {
                let suffix = if f.required { "" } else { "=None" };
                kwargs.push(format!("{}{suffix}", py_name(&f.wire_name)));
            }
            if op.whole_body.is_some() {
                kwargs.push("body".to_string());
            }
            if matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_))) {
                kwargs.push("last_event_id=None".to_string());
            }
            if !kwargs.is_empty() {
                args.push("*".to_string());
                args.extend(kwargs);
            }
            writeln!(
                out,
                "```python\nclient.{}.{}({}) -> {}\n```",
                resource.path(),
                py_name(&op.name),
                args.join(", "),
                return_annotation(api, op).replace("types.", "")
            )
            .unwrap();
        }
    }
    out
}

fn emit_readme(api: &Api, pkg: &str) -> String {
    let name = &api.name;
    let ws_env_note = api
        .client_params
        .first()
        .map(|c| format!(" (and {})", c.env_var))
        .unwrap_or_default();
    // Doc anchors are chosen STRUCTURALLY from the IR (never by matching
    // spec-specific names): a no-argument JSON read, a paginated list, and
    // an SSE stream.
    let ops = || {
        api.resources
            .iter()
            .flat_map(|r| r.operations.iter().map(move |o| (r, o)))
    };
    let getting_started_call = ops()
        .find(|(_, o)| {
            o.positionals.is_empty()
                && o.body_fields.is_empty()
                && o.whole_body.is_none()
                && o.pagination.is_none()
                && o.query_params.iter().all(|q| !q.required)
                && matches!(o.response, ResponseKind::Json(_))
        })
        .map(|(r, o)| format!("result = client.{}.{}()", r.path(), o.name))
        .unwrap_or_else(|| "...  # see api.md for every method signature".to_string());
    let pagination_section = ops()
        .find(|(_, o)| {
            o.pagination.is_some()
                && o.positionals.is_empty()
                && o.query_params.iter().all(|q| !q.required)
        })
        .map(|(r, o)| {
            format!(
                "\n## Pagination\n\n```python\nfor item in client.{}.{}():\n    ...  # iterating a page auto-fetches every page\n```\n",
                r.path(),
                o.name
            )
        })
        .unwrap_or_default();
    let streaming_section = ops()
        .find(|(_, o)| matches!(o.response, ResponseKind::Sse(_)) && o.pagination.is_none())
        .map(|(r, o)| {
            let pos = o
                .positionals
                .iter()
                .map(|p| py_name(&p.wire_name))
                .collect::<Vec<_>>()
                .join(", ");
            let comma = if pos.is_empty() { "" } else { ", " };
            format!(
                "\n## Streaming (SSE)\n\nStreams are context managers — early exits close the response\ndeterministically:\n\n```python\nwith client.{path}.{m}({pos}) as stream:\n    for event in stream:\n        ...\n\n# Resume after a disconnect: the checkpoint persists per the SSE spec.\nwith client.{path}.{m}({pos}{comma}last_event_id=stream.last_event_id) as resumed:\n    for event in resumed:\n        ...\n```\n",
                path = r.path(),
                m = o.name
            )
        })
        .unwrap_or_default();
    let webhooks_section = if api.webhooks.is_empty() {
        String::new()
    } else {
        format!(
            "\n## Webhooks (no API key required)\n\n```python\nimport os\n\nfrom {pkg} import unwrap_webhook\n\nevent = unwrap_webhook(payload, headers, secret=os.environ[\"{wh}\"])\n```\n\nVerification follows Standard Webhooks (24–64 byte decoded secrets,\ninteger timestamps, bounded tolerance).\n",
            wh = api.webhook_env
        )
    };
    let env_comment = if matches!(api.auth, Auth::None) {
        "# This API requires no authentication.".to_string()
    } else {
        format!(
            "# Reads {api_env}{ws_env_note} from the environment when\n# arguments are omitted. Explicit blank values raise ValueError and never\n# fall back to the environment.",
            api_env = api.api_key_env
        )
    };
    // The nested-request example is derived from the IR and the SAME sampler
    // the conformance manifest uses — backends know nothing about any
    // particular spec, and sampled enum values are real members by
    // construction.
    let create_example = crate::backends::doc_example_op(api)
        .map(|(resource, op)| {
            let mut lines = vec![format!("client.{}.{}(", resource.path(), op.name)];
            for f in op.body_fields.iter().filter(|f| f.required) {
                let sample = crate::backends::trim_doc_sample(
                    &crate::backends::snake_sample(
                        api,
                        &f.ty,
                        crate::backends::manifest_sample(api, &f.ty),
                    ),
                    2,
                );
                lines.push(format!(
                    "    {}={},",
                    py_name(&f.wire_name),
                    py_literal(&sample)
                ));
            }
            lines.push(")".to_string());
            lines.join("\n")
        })
        .unwrap_or_else(|| "# (no request-bearing operation in this API)".to_string());
    format!(
        r#"# {name} Python SDK

The official Python client for the {name} API. Generated by redwood.
Built on httpx; typed with dataclass models, TypedDict request shapes, and
a `py.typed` marker.

## Install

```sh
pip install {pkg}
```

## Getting started

```python
from {pkg} import {name}

{env_comment}
with {name}() as client:
    {getting_started_call}
```

`{name}` is a context manager; `close()` releases the internally owned
httpx client (a caller-supplied `http_client` is never closed).

## Requests are snake_case throughout

Nested request objects take snake_case keys and are translated to wire
names at the HTTP boundary. Wire-format keys also pass through for raw
payloads; your own data inside JSON/map fields is never rewritten.

```python
{create_example}
```

## Errors

Non-2xx responses raise `APIError` (google.rpc Status: `status_code`,
`code`, `message`, `details`). Protocol surprises (unfollowed redirects,
non-JSON success bodies) raise `APIResponseError`; connection failures
raise `APIConnectionError`.

## Retries

Automatic retries apply only to idempotent methods (GET/HEAD/PUT/DELETE)
and default to 0; counts normalize to a bounded 0–10 integer. Streaming
requests never auto-retry and their read timeout is unlimited (connect/
write/pool stay bounded).

{pagination_section}{streaming_section}{webhooks_section}
## Reference

The full method reference ships with the package as `{pkg}/api.md`
(importlib.resources reads it; it is also in the repository).
"#,
    )
}

impl PythonBackend {
    fn package_name(&self, api: &Api) -> String {
        self.config
            .package_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase())
    }
}

// ---- naming ----------------------------------------------------------------

const PYTHON_KEYWORDS: &[&str] = &[
    "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del", "elif",
    "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda",
    "nonlocal", "not", "or", "pass", "raise", "return", "try", "while", "with", "yield", "True",
    "False", "None",
];

/// snake_case identifier for params/fields, keyword-safe.
pub(crate) fn py_name(wire: &str) -> String {
    let snake = wire.to_snake_case();
    if PYTHON_KEYWORDS.contains(&snake.as_str()) {
        format!("{snake}_")
    } else {
        snake
    }
}

/// Type names keep their spec identity (underscores are valid in Python).
fn py_type_name(name: &str) -> String {
    if name.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        name.to_string()
    } else {
        name.to_upper_camel_case()
    }
}

fn class_name(resource: &Resource) -> String {
    resource.ident.to_upper_camel_case()
}

// ---- type mapping ----------------------------------------------------------

fn resolved_shape<'a>(api: &'a Api, name: &str) -> Option<&'a Shape> {
    let mut current = name;
    for _ in 0..8 {
        match api.types.get(current).map(|d| &d.shape) {
            Some(Shape::Alias(Ty::Named(next))) => current = next,
            other => return other,
        }
    }
    None
}

/// Response-side annotation. `ns` prefixes named types ("" inside types.py,
/// "types." inside resource modules).
fn py_ty(_api: &Api, ty: &Ty, ns: &str) -> String {
    match ty {
        Ty::String => "str".into(),
        Ty::Literal(v) => format!("Literal[\"{v}\"]"),
        Ty::Bool => "bool".into(),
        Ty::Int32 | Ty::Int64 => "int".into(),
        Ty::Float | Ty::Double => "float".into(),
        Ty::Timestamp => "datetime".into(),
        Ty::Bytes => "str".into(),
        Ty::Json => "Any".into(),
        Ty::Named(n) => format!("{ns}{}", py_type_name(n)),
        Ty::List(inner) => format!("List[{}]", py_ty(_api, inner, ns)),
        Ty::Map(inner) => format!("Dict[str, {}]", py_ty(_api, inner, ns)),
    }
}

/// Request-side annotation: named structs/unions use their TypedDict
/// `...Param` mirror; timestamps accept a preformatted string too.
fn py_param_ty(api: &Api, ty: &Ty, ns: &str, quote_refs: bool) -> String {
    let named = |base: String| {
        if quote_refs {
            format!("\"{base}\"")
        } else {
            format!("{ns}{base}")
        }
    };
    match ty {
        Ty::Timestamp => "Union[str, datetime]".into(),
        Ty::Named(n) => match resolved_shape(api, n) {
            Some(Shape::Struct(_)) | Some(Shape::Union(_)) => {
                named(format!("{}Param", py_type_name(n)))
            }
            Some(Shape::Enum(_)) => named(py_type_name(n)),
            Some(Shape::Alias(inner)) => {
                let inner = inner.clone();
                py_param_ty(api, &inner, ns, quote_refs)
            }
            None => "Any".into(),
        },
        Ty::List(inner) => format!("List[{}]", py_param_ty(api, inner, ns, quote_refs)),
        Ty::Map(inner) => format!("Dict[str, {}]", py_param_ty(api, inner, ns, quote_refs)),
        _ => py_ty(api, ty, ns),
    }
}

/// Expression decoding `value` (raw JSON) into the typed model, or `value`
/// itself when decoding is the identity.
fn decode_expr(api: &Api, ty: &Ty, value: &str, ns: &str) -> String {
    match ty {
        Ty::Timestamp => format!("parse_datetime({value})"),
        Ty::Named(n) => match resolved_shape(api, n) {
            Some(Shape::Struct(s)) => {
                if s.fields.is_empty() {
                    match &s.additional {
                        Some(inner) => {
                            let inner_expr = decode_expr(api, inner, "v", ns);
                            if inner_expr == "v" {
                                value.to_string()
                            } else {
                                format!("{{k: {inner_expr} for k, v in ({value}).items()}}")
                            }
                        }
                        None => format!("{ns}{}._from_json({value})", py_type_name(n)),
                    }
                } else {
                    format!("{ns}{}._from_json({value})", py_type_name(n))
                }
            }
            Some(Shape::Union(_)) => format!("{ns}_decode_{}({value})", py_type_name(n)),
            Some(Shape::Enum(_)) | None => value.to_string(),
            Some(Shape::Alias(inner)) => {
                let inner = inner.clone();
                decode_expr(api, &inner, value, ns)
            }
        },
        Ty::List(inner) => {
            let item = decode_expr(api, inner, "item", ns);
            if item == "item" {
                value.to_string()
            } else {
                format!("[{item} for item in ({value})]")
            }
        }
        Ty::Map(inner) => {
            let item = decode_expr(api, inner, "v", ns);
            if item == "v" {
                value.to_string()
            } else {
                format!("{{k: {item} for k, v in ({value}).items()}}")
            }
        }
        _ => value.to_string(),
    }
}

/// Decode with a None guard, for optional/absent values.
fn guarded_decode(api: &Api, ty: &Ty, value: &str, ns: &str) -> String {
    let expr = decode_expr(api, ty, value, ns);
    if expr == value {
        value.to_string()
    } else {
        format!("None if {value} is None else {expr}")
    }
}

// ---- types.py ---------------------------------------------------------------

fn emit_types(api: &Api) -> String {
    let mut out = String::from(
        "# Code generated by redwood. DO NOT EDIT.\nfrom __future__ import annotations\n\n\
         from dataclasses import dataclass\nfrom datetime import datetime\n\
         from typing import Any, Dict, List, Literal, Optional, Union\n\n\
         # TypedDict and Required must come from the SAME provider: Python\n\
         # 3.9's stdlib TypedDict ignores typing_extensions.Required when\n\
         # computing __required_keys__ (runtime metadata would report every\n\
         # key optional to consumers like Pydantic).\n\
         from typing_extensions import Required, TypedDict\n\n\
         from ._core import APIResponseError, parse_datetime\n\n",
    );
    out.push_str(
        "\ndef _req(data: Any, model: str, key: str) -> Any:\n    \"\"\"Fetch a REQUIRED response field: absence or null violates the API\n    contract and is the SDK's stable protocol error, never a silently\n    partial model or a raw KeyError.\"\"\"\n    value = data.get(key)\n    if value is None:\n        raise APIResponseError(0, f\"response missing required field {model}.{key}\")\n    return value\n\n",
    );

    for decl in api.types.values() {
        emit_type_decl(api, &mut out, decl);
        out.push('\n');
    }

    // Request-side TypedDict mirrors for every type reachable from a request.
    let request_types = api.request_reachable();
    writeln!(out, "\n# ---- request shapes ----\n").unwrap();
    for decl in api.types.values() {
        if !request_types.contains(&decl.name) {
            continue;
        }
        emit_param_decl(api, &mut out, decl);
    }
    emit_request_encoders(api, &mut out, &request_types);
    out
}

fn doc_line(text: &str) -> String {
    text.trim()
        .replace('\n', " ")
        .replace("\\\"", "\"")
        .replace('"', "'")
}

fn emit_type_decl(api: &Api, out: &mut String, decl: &TypeDecl) {
    let name = py_type_name(&decl.name);
    match &decl.shape {
        Shape::Struct(s) => {
            if s.fields.is_empty() {
                if let Some(additional) = &s.additional {
                    writeln!(out, "{name} = Dict[str, {}]", py_ty(api, additional, "")).unwrap();
                    return;
                }
            }
            writeln!(out, "\n@dataclass").unwrap();
            writeln!(out, "class {name}:").unwrap();
            if let Some(d) = &decl.description {
                writeln!(out, "    \"\"\"{}\"\"\"\n", doc_line(d)).unwrap();
            }
            // Response models expose the OUTPUT view: writeOnly fields are
            // input secrets and never promised (or leaked via repr) here.
            // Requiredness is part of the contract: required fields are
            // plain `T` (declared FIRST — dataclass ordering), optional
            // fields default to None for forward compatibility.
            let out_fields: Vec<&Field> = s.output_fields().collect();
            for f in out_fields.iter().filter(|f| f.required) {
                writeln!(
                    out,
                    "    {}: {}",
                    py_name(&f.wire_name),
                    py_ty(api, &f.ty, "")
                )
                .unwrap();
            }
            for f in out_fields.iter().filter(|f| !f.required) {
                writeln!(
                    out,
                    "    {}: Optional[{}] = None",
                    py_name(&f.wire_name),
                    py_ty(api, &f.ty, "")
                )
                .unwrap();
            }
            if out_fields.is_empty() {
                writeln!(out, "    pass").unwrap();
            }
            writeln!(out, "\n    @staticmethod").unwrap();
            writeln!(out, "    def _from_json(data: Any) -> \"{name}\":").unwrap();
            if out_fields.is_empty() {
                writeln!(out, "        return {name}()").unwrap();
            } else {
                writeln!(out, "        return {name}(").unwrap();
                for f in &out_fields {
                    let access = if f.required {
                        format!("_req(data, \"{name}\", \"{}\")", f.wire_name)
                    } else {
                        format!("data.get(\"{}\")", f.wire_name)
                    };
                    writeln!(
                        out,
                        "            {}={},",
                        py_name(&f.wire_name),
                        guarded_decode(api, &f.ty, &access, "")
                    )
                    .unwrap();
                }
                writeln!(out, "        )").unwrap();
            }
        }
        Shape::Enum(e) => {
            let values: Vec<String> = e.values.iter().map(|v| format!("\"{v}\"")).collect();
            writeln!(out, "{name} = Literal[{}]", values.join(", ")).unwrap();
        }
        Shape::Union(u) => {
            let variants: Vec<String> = u
                .variants
                .iter()
                .map(|v| format!("\"{}\"", py_ty(api, &v.ty, "")))
                .collect();
            writeln!(out, "{name} = Union[{}]", variants.join(", ")).unwrap();
            emit_union_decoder(api, out, &name, u);
        }
        Shape::Alias(ty) => {
            writeln!(out, "{name} = {}", py_ty(api, ty, "")).unwrap();
        }
    }
}

fn emit_union_decoder(api: &Api, out: &mut String, name: &str, u: &UnionShape) {
    writeln!(out, "\n\ndef _decode_{name}(data: Any) -> Any:").unwrap();
    match &u.discriminator {
        Some(disc) => {
            // Protobuf JSON gateways encode an unset optional union as null,
            // {}, or a default-value discriminator (""); all three decode as
            // absent. Unknown NON-empty tags still raise.
            writeln!(out, "    if data is None:").unwrap();
            writeln!(out, "        return None").unwrap();
            writeln!(out, "    tag = data.get(\"{}\")", disc.property).unwrap();
            writeln!(out, "    if not tag:").unwrap();
            writeln!(out, "        return None").unwrap();
            for v in &u.variants {
                let Some(tag) = &v.tag else { continue };
                writeln!(out, "    if tag == \"{tag}\":").unwrap();
                writeln!(
                    out,
                    "        return {}",
                    decode_expr(api, &v.ty, "data", "")
                )
                .unwrap();
            }
            writeln!(
                out,
                "    raise ValueError(f\"{name}: unknown {} {{tag!r}}\")",
                disc.property
            )
            .unwrap();
        }
        None => {
            for v in &u.variants {
                let check = variant_check(api, &v.ty);
                writeln!(out, "    if {check}:").unwrap();
                writeln!(
                    out,
                    "        return {}",
                    decode_expr(api, &v.ty, "data", "")
                )
                .unwrap();
            }
            writeln!(out, "    raise ValueError(\"{name}: no variant matched\")").unwrap();
        }
    }
}

/// A structural predicate for undiscriminated unions: first variant whose
/// shape matches wins (mirrors the Go decoder's first-success semantics).
fn variant_check(api: &Api, ty: &Ty) -> String {
    match ty {
        Ty::String | Ty::Timestamp | Ty::Bytes | Ty::Literal(_) => "isinstance(data, str)".into(),
        Ty::Bool => "isinstance(data, bool)".into(),
        Ty::Int32 | Ty::Int64 => "isinstance(data, int)".into(),
        Ty::Float | Ty::Double => "isinstance(data, (int, float))".into(),
        Ty::List(_) => "isinstance(data, list)".into(),
        Ty::Map(_) | Ty::Json => "isinstance(data, dict)".into(),
        Ty::Named(n) => match resolved_shape(api, n) {
            Some(Shape::Struct(s)) => {
                let required: Vec<String> = s
                    .fields
                    .iter()
                    .filter(|f| f.required && !f.nullable)
                    .map(|f| format!("\"{}\"", f.wire_name))
                    .collect();
                if required.is_empty() {
                    "isinstance(data, dict)".into()
                } else {
                    format!(
                        "isinstance(data, dict) and all(k in data for k in ({},))",
                        required.join(", ")
                    )
                }
            }
            Some(Shape::Enum(_)) => "isinstance(data, str)".into(),
            _ => "True".into(),
        },
    }
}

// ---- request TypedDicts ------------------------------------------------------

fn emit_param_decl(api: &Api, out: &mut String, decl: &TypeDecl) {
    let name = py_type_name(&decl.name);
    match &decl.shape {
        Shape::Struct(s) => {
            if s.fields.is_empty() {
                if let Some(additional) = &s.additional {
                    writeln!(
                        out,
                        "{name}Param = Dict[str, {}]",
                        py_param_ty(api, additional, "", true)
                    )
                    .unwrap();
                    return;
                }
            }
            writeln!(out, "{name}Param = TypedDict(\"{name}Param\", {{").unwrap();
            // INPUT view only: readOnly fields are server-owned and never
            // accepted as request input.
            for f in s.input_fields() {
                // Keys are snake_case like the rest of the API surface; the
                // generated request encoders translate them to wire names.
                // total=False + Required[...] keeps schema requiredness
                // visible to type checkers.
                let annotation = py_param_ty(api, &f.ty, "", true);
                let annotation = if f.required && !f.nullable {
                    format!("Required[{annotation}]")
                } else {
                    annotation
                };
                writeln!(out, "    \"{}\": {annotation},", py_name(&f.wire_name)).unwrap();
            }
            writeln!(out, "}}, total=False)").unwrap();
        }
        Shape::Union(u) => {
            let variants: Vec<String> = u
                .variants
                .iter()
                .map(|v| py_param_ty(api, &v.ty, "", true))
                .collect();
            writeln!(out, "{name}Param = Union[{}]", variants.join(", ")).unwrap();
        }
        // Enums/aliases are shared verbatim between request and response.
        Shape::Enum(_) | Shape::Alias(_) => {}
    }
}

// ---- request encoders ----------------------------------------------------------

/// Python expression evaluating to a callable that encodes a request value of
/// this type (translating snake_case keys to wire names, recursively), or
/// None when the value needs no translation (scalars, enums, raw JSON/maps).
/// Callables are wrapped in lambdas so tables can reference encoders defined
/// later in the module.
fn py_value_encoder(api: &Api, ty: &Ty, ns: &str) -> Option<String> {
    match ty {
        Ty::Named(n) => match resolved_shape(api, n) {
            Some(Shape::Struct(s)) => {
                if s.fields.is_empty() {
                    match &s.additional {
                        Some(inner) => {
                            let inner = inner.clone();
                            py_value_encoder(api, &inner, ns).map(|e| {
                                format!(
                                    "(lambda _v: {{_k: ({e})(_i) for _k, _i in _v.items()}} if isinstance(_v, dict) else _v)"
                                )
                            })
                        }
                        None => None,
                    }
                } else {
                    Some(format!("(lambda _v: {ns}_encode_{}(_v))", py_type_name(n)))
                }
            }
            Some(Shape::Union(_)) => Some(format!("(lambda _v: {ns}_encode_{}(_v))", py_type_name(n))),
            Some(Shape::Alias(inner)) => {
                let inner = inner.clone();
                py_value_encoder(api, &inner, ns)
            }
            Some(Shape::Enum(_)) | None => None,
        },
        Ty::List(inner) => py_value_encoder(api, inner, ns).map(|e| {
            format!("(lambda _v: [({e})(_i) for _i in _v] if isinstance(_v, list) else _v)")
        }),
        Ty::Map(inner) => py_value_encoder(api, inner, ns).map(|e| {
            format!(
                "(lambda _v: {{_k: ({e})(_i) for _k, _i in _v.items()}} if isinstance(_v, dict) else _v)"
            )
        }),
        _ => None,
    }
}

/// Emit `_FIELDS_*` translation tables and `_encode_*` functions for every
/// named type reachable from a request position.
fn emit_request_encoders(api: &Api, out: &mut String, request_types: &BTreeSet<String>) {
    writeln!(
        out,
        "\n# ---- request encoders (snake_case -> wire keys) ----\n"
    )
    .unwrap();
    write!(
        out,
        r#"
def _encode_fields(fields: Dict[str, Any], data: Any, drop: Any = None) -> Any:
    """Translate known snake_case keys to wire names, encoding nested typed
    values. Wire-format keys and unknown keys pass through untouched, so raw
    payloads and forward-compatible fields keep working. Keys in `drop` are
    server-owned (readOnly) and are REMOVED, so a fetched resource can be
    modified and resubmitted without echoing server state."""
    if not isinstance(data, dict):
        return data
    out: Dict[str, Any] = {{}}
    claimed: Dict[str, str] = {{}}
    for key, value in data.items():
        if drop is not None and key in drop:
            continue
        entry = fields.get(key)
        if entry is None:
            wire, encode = key, None
        else:
            wire, encode = entry
        # Two DISTINCT input keys normalizing to one wire key (external_id +
        # externalId) are a caller bug surfaced pre-transport — dict order
        # must never silently pick a winner, even for equal values.
        if wire in claimed and claimed[wire] != key:
            raise ValueError(
                f"conflicting request keys {{claimed[wire]!r}} and {{key!r}} both "
                f"map to the wire field {{wire!r}}; supply exactly one spelling"
            )
        claimed[wire] = key
        out[wire] = encode(value) if encode is not None and value is not None else value
    return out

"#
    )
    .unwrap();

    for decl in api.types.values() {
        if !request_types.contains(&decl.name) {
            continue;
        }
        let name = py_type_name(&decl.name);
        match &decl.shape {
            Shape::Struct(s) => {
                if s.fields.is_empty() {
                    continue;
                }
                let refs: Vec<&Field> = s.input_fields().collect();
                let dropped: Vec<&Field> = s.fields.iter().filter(|f| f.read_only).collect();
                emit_py_field_table(api, out, &name, &refs);
                emit_py_drop_set(out, &name, &dropped);
                let drop_arg = if dropped.is_empty() {
                    String::new()
                } else {
                    format!(", _DROP_{name}")
                };
                writeln!(
                    out,
                    "def _encode_{name}(data: Any) -> Any:\n    return _encode_fields(_FIELDS_{name}, data{drop_arg})\n"
                )
                .unwrap();
            }
            Shape::Union(u) => match &u.discriminator {
                Some(disc) => {
                    writeln!(out, "def _encode_{name}(data: Any) -> Any:").unwrap();
                    writeln!(out, "    if not isinstance(data, dict):").unwrap();
                    writeln!(out, "        return data").unwrap();
                    writeln!(out, "    tag = data.get(\"{}\")", disc.property).unwrap();
                    for v in &u.variants {
                        let Some(tag) = &v.tag else { continue };
                        if let Some(enc) = py_value_encoder(api, &v.ty, "") {
                            writeln!(out, "    if tag == \"{tag}\":").unwrap();
                            writeln!(out, "        return {enc}(data)").unwrap();
                        }
                    }
                    writeln!(out, "    return data\n").unwrap();
                }
                None => {
                    // Undiscriminated: translate through the union of every
                    // struct variant's key table.
                    let mut merged: Vec<&Field> = Vec::new();
                    let mut dropped: Vec<&Field> = Vec::new();
                    for v in &u.variants {
                        if let Ty::Named(vn) = &v.ty {
                            if let Some(Shape::Struct(vs)) = resolved_shape(api, vn) {
                                merged.extend(vs.input_fields());
                                dropped.extend(vs.fields.iter().filter(|f| f.read_only));
                            }
                        }
                    }
                    emit_py_field_table(api, out, &name, &merged);
                    emit_py_drop_set(out, &name, &dropped);
                    let drop_arg = if dropped.is_empty() {
                        String::new()
                    } else {
                        format!(", _DROP_{name}")
                    };
                    writeln!(
                        out,
                        "def _encode_{name}(data: Any) -> Any:\n    return _encode_fields(_FIELDS_{name}, data{drop_arg})\n"
                    )
                    .unwrap();
                }
            },
            Shape::Enum(_) | Shape::Alias(_) => {}
        }
    }
}

/// Server-owned (readOnly) keys the request encoder silently drops, in both
/// snake_case and wire spellings.
fn emit_py_drop_set(out: &mut String, name: &str, dropped: &[&Field]) {
    if dropped.is_empty() {
        return;
    }
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for f in dropped {
        keys.insert(py_name(&f.wire_name));
        keys.insert(f.wire_name.clone());
    }
    let list = keys
        .iter()
        .map(|k| format!("\"{k}\""))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "_DROP_{name} = frozenset(({list},))").unwrap();
}

fn emit_py_field_table(api: &Api, out: &mut String, name: &str, fields: &[&Field]) {
    writeln!(out, "_FIELDS_{name}: Dict[str, Any] = {{").unwrap();
    for f in fields {
        let snake = py_name(&f.wire_name);
        let encoder = py_value_encoder(api, &f.ty, "").unwrap_or_else(|| "None".into());
        writeln!(out, "    \"{snake}\": (\"{}\", {encoder}),", f.wire_name).unwrap();
        if snake != f.wire_name {
            writeln!(
                out,
                "    \"{}\": (\"{}\", {encoder}),",
                f.wire_name, f.wire_name
            )
            .unwrap();
        }
    }
    writeln!(out, "}}\n").unwrap();
}

// ---- resources ---------------------------------------------------------------

fn client_param<'a>(api: &'a Api, wire_name: &str) -> Option<&'a ClientParam> {
    api.client_params.iter().find(|c| c.wire_name == wire_name)
}

fn return_annotation(api: &Api, op: &Operation) -> String {
    match (&op.pagination, &op.response) {
        (Some(page), _) => format!("SyncPage[{}]", py_ty(api, &page.item_ty, "types.")),
        (None, ResponseKind::Json(ty)) => py_ty(api, ty, "types."),
        (None, ResponseKind::Sse(ty)) => format!("Stream[{}]", py_ty(api, ty, "types.")),
        (None, ResponseKind::Empty) => "None".into(),
    }
}

fn emit_resource(api: &Api, resource: &Resource) -> String {
    let mut out = String::from(
        "# Code generated by redwood. DO NOT EDIT.\nfrom __future__ import annotations\n\n\
         from datetime import datetime\nfrom typing import Any, Dict, List, Literal, Optional, Union\n\n\
         from .. import types\nfrom .._core import Core, RequestOptions, decode_response, parse_datetime, path_param\n\
         from .._pagination import SyncPage\nfrom .._sse import Stream\n\n\n",
    );
    // Nested resources hang off this one as attributes.
    let children: Vec<&Resource> = api
        .resources
        .iter()
        .filter(|r| r.parent.as_deref() == Some(resource.name.as_str()))
        .collect();
    for child in &children {
        writeln!(out, "from .{} import {}", child.ident, class_name(child)).unwrap();
    }
    if !children.is_empty() {
        out.push('\n');
    }

    let class = class_name(resource);
    writeln!(out, "class {class}:").unwrap();
    if let Some(d) = &resource.description {
        writeln!(out, "    \"\"\"{}\"\"\"\n", doc_line(d)).unwrap();
    }
    writeln!(out, "    def __init__(self, core: Core) -> None:").unwrap();
    writeln!(out, "        self._core = core").unwrap();
    for child in &children {
        writeln!(
            out,
            "        self.{} = {}(core)",
            child.name,
            class_name(child)
        )
        .unwrap();
    }
    for op in &resource.operations {
        out.push('\n');
        emit_method(api, resource, op, &mut out);
    }
    out
}

/// Optional trailing skip-events argument for stream construction.
fn py_skip_arg(api: &Api) -> String {
    if api.sse_skip_events.is_empty() {
        return String::new();
    }
    let list = api
        .sse_skip_events
        .iter()
        .map(|e| format!("\"{e}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(", skip_events=({list},)")
}

fn emit_method(api: &Api, resource: &Resource, op: &Operation, out: &mut String) {
    let ret = return_annotation(api, op);

    // Signature.
    writeln!(out, "    def {}(", py_name(&op.name)).unwrap();
    writeln!(out, "        self,").unwrap();
    for p in &op.positionals {
        writeln!(
            out,
            "        {}: {},",
            py_name(&p.wire_name),
            py_param_ty(api, &p.ty, "types.", false)
        )
        .unwrap();
    }
    let mut kwargs: Vec<(&str, String, bool)> = Vec::new(); // (wire, annotation, has_default)
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        let is_client = client_param(api, &p.wire_name).is_some();
        let required = p.required && !is_client;
        kwargs.push((
            &p.wire_name,
            py_param_ty(api, &p.ty, "types.", false),
            !required,
        ));
    }
    for f in &op.body_fields {
        kwargs.push((
            &f.wire_name,
            py_param_ty(api, &f.ty, "types.", false),
            !f.required,
        ));
    }
    if let Some(ty) = &op.whole_body {
        kwargs.push(("body", py_param_ty(api, ty, "types.", false), false));
    }
    // Required data reads first in the declaration: binding is keyword-only,
    // so this orders docs/IDE help for callers without changing any call.
    kwargs.sort_by_key(|(_, _, has_default)| *has_default);
    let is_sse = matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_)));
    writeln!(out, "        *,").unwrap();
    for (wire, annotation, has_default) in &kwargs {
        if *has_default {
            writeln!(
                out,
                "        {}: Optional[{annotation}] = None,",
                py_name(wire)
            )
            .unwrap();
        } else {
            writeln!(out, "        {}: {annotation},", py_name(wire)).unwrap();
        }
    }
    if is_sse {
        writeln!(out, "        last_event_id: Optional[str] = None,").unwrap();
    }
    // Transport controls stay a single visibly-separate kwarg, always last.
    writeln!(
        out,
        "        request_options: Optional[RequestOptions] = None,"
    )
    .unwrap();
    writeln!(out, "    ) -> {ret}:").unwrap();
    if let Some(s) = &op.summary {
        writeln!(out, "        \"\"\"{}\"\"\"", doc_line(s)).unwrap();
    }

    // Client-level defaults.
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        let Some(c) = client_param(api, &p.wire_name) else {
            continue;
        };
        writeln!(
            out,
            "        {name} = self._core.resolve_default(\"{wire}\", \"{env}\", {name})",
            name = py_name(&p.wire_name),
            wire = c.wire_name,
            env = c.env_var,
        )
        .unwrap();
    }

    // Path.
    let mut path_expr = String::new();
    let mut has_placeholder = false;
    let mut rest = op.path.as_str();
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|e| start + e)
            .expect("balanced");
        path_expr.push_str(&rest[..start].replace('{', "{{").replace('}', "}}"));
        let param = &rest[start + 1..end];
        path_expr.push_str(&format!("{{path_param('{param}', {})}}", py_name(param)));
        has_placeholder = true;
        rest = &rest[end + 1..];
    }
    path_expr.push_str(&rest.replace('{', "{{").replace('}', "}}"));
    if has_placeholder {
        writeln!(out, "        _path = f\"{path_expr}\"").unwrap();
    } else {
        writeln!(out, "        _path = \"{path_expr}\"").unwrap();
    }

    // Pagination closes over these params for later page fetches: snapshot
    // list-valued filters NOW so caller mutation after page 1 cannot change
    // the query mid-traversal and mix result sets.
    if op.pagination.is_some() {
        for p in &op.query_params {
            if matches!(p.ty, Ty::List(_)) {
                let n = py_name(&p.wire_name);
                writeln!(out, "        {n} = list({n}) if {n} is not None else None").unwrap();
            }
        }
    }

    // Query / body dicts (wire names; None values dropped by the core).
    let has_query = !op.query_params.is_empty();
    if has_query {
        writeln!(out, "        _query = {{").unwrap();
        for p in &op.query_params {
            writeln!(
                out,
                "            \"{}\": {},",
                p.wire_name,
                py_name(&p.wire_name)
            )
            .unwrap();
        }
        writeln!(out, "        }}").unwrap();
    }
    let has_body = !op.body_fields.is_empty();
    if has_body {
        writeln!(out, "        _body = {{").unwrap();
        for f in &op.body_fields {
            // Typed values run through the request encoder so users write
            // snake_case keys in nested objects too.
            let value = match py_value_encoder(api, &f.ty, "types.") {
                Some(enc) => format!("{enc}({})", py_name(&f.wire_name)),
                None => py_name(&f.wire_name),
            };
            writeln!(out, "            \"{}\": {value},", f.wire_name).unwrap();
        }
        writeln!(out, "        }}").unwrap();
        // Flattened-object omission policy lives HERE: unset optional kwargs
        // drop out; the core serializes whatever body it receives exactly.
        writeln!(
            out,
            "        _body = {{_k: _v for _k, _v in _body.items() if _v is not None}}"
        )
        .unwrap();
    }
    let query_arg = if has_query { ", query=_query" } else { "" };
    // The whole-body case passes the user's `body` kwarg straight through;
    // the flattened case passes the locally built `_body` dict.
    let body_arg = if has_body {
        ", body=_body".to_string()
    } else if let Some(ty) = &op.whole_body {
        match py_value_encoder(api, ty, "types.") {
            Some(enc) => format!(", body={enc}(body)"),
            None => ", body=body".to_string(),
        }
    } else {
        String::new()
    };

    match (&op.pagination, &op.response) {
        (Some(page), _) => {
            let item_model = py_ty(api, &page.item_ty, "types.");
            writeln!(
                out,
                "        _data = self._core.request(\"{}\", _path{query_arg}{body_arg}, request_options=request_options) or {{}}",
                op.http_method.as_str()
            )
            .unwrap();
            let item_decode = decode_expr(api, &page.item_ty, "item", "types.");
            writeln!(
                out,
                "        _items = decode_response(\"{}.{}\", _data, lambda _d: [{item_decode} for item in (_d.get(\"{}\") or [])])",
                resource.path(),
                py_name(&op.name),
                page.items_field
            )
            .unwrap();
            // Walk the dotted next-cursor path with or-{} guards.
            let mut cursor_access = String::from("_data");
            let segments: Vec<&str> = page.next_cursor_path.split('.').collect();
            for (i, segment) in segments.iter().enumerate() {
                if i + 1 == segments.len() {
                    cursor_access = format!("({cursor_access}).get(\"{segment}\") or \"\"");
                } else {
                    cursor_access = format!("(({cursor_access}).get(\"{segment}\") or {{}})");
                }
            }
            writeln!(out, "        _next_cursor = {cursor_access}").unwrap();
            // Refetch re-enters the method with the cursor swapped in.
            let mut refetch_args: Vec<String> = Vec::new();
            for p in &op.positionals {
                refetch_args.push(py_name(&p.wire_name));
            }
            for (wire, _, _) in &kwargs {
                let name = py_name(wire);
                if *wire == page.cursor_param {
                    refetch_args.push(format!("{name}=_cursor"));
                } else {
                    refetch_args.push(format!("{name}={name}"));
                }
            }
            // Page fetches keep the caller's transport options unchanged.
            refetch_args.push("request_options=request_options".to_string());
            writeln!(
                out,
                "\n        def _fetch(_cursor: str) -> SyncPage[{item_model}]:"
            )
            .unwrap();
            writeln!(
                out,
                "            return self.{}({})",
                py_name(&op.name),
                refetch_args.join(", ")
            )
            .unwrap();
            writeln!(
                out,
                "\n        return SyncPage(_items, _next_cursor, _fetch)"
            )
            .unwrap();
        }
        (None, ResponseKind::Json(ty)) => {
            writeln!(
                out,
                "        _data = self._core.request(\"{}\", _path{query_arg}{body_arg}, request_options=request_options)",
                op.http_method.as_str()
            )
            .unwrap();
            writeln!(
                out,
                "        return decode_response(\"{}.{}\", _data, lambda _d: {})",
                resource.path(),
                py_name(&op.name),
                decode_expr(api, ty, "_d", "types.")
            )
            .unwrap();
        }
        (None, ResponseKind::Sse(ty)) => {
            writeln!(
                out,
                "        _headers = {{\"Last-Event-ID\": last_event_id}} if last_event_id else None"
            )
            .unwrap();
            writeln!(
                out,
                "        _response = self._core.raw(\"{}\", _path{query_arg}{body_arg}, headers=_headers, request_options=request_options)",
                op.http_method.as_str()
            )
            .unwrap();
            let decode = format!(
                "decode_response(\"{}.{}\", event, lambda _d: {})",
                resource.path(),
                py_name(&op.name),
                decode_expr(api, ty, "_d", "types.")
            );
            writeln!(
                out,
                "        _reconnect = None if (request_options is not None and request_options.reconnect is False) else (lambda _rid: self._core.raw(\"{http}\", _path{query_arg}{body_arg}, headers=({{\"Last-Event-ID\": _rid}} if _rid else None), request_options=request_options))\n        return Stream(_response, lambda event: {decode}, last_event_id=last_event_id{skip_arg}, reconnect=_reconnect)",
                http = op.http_method.as_str(),
                skip_arg = py_skip_arg(api)
            )
            .unwrap();
        }
        (None, ResponseKind::Empty) => {
            writeln!(
                out,
                "        self._core.request(\"{}\", _path{query_arg}{body_arg}, expects_body=False, request_options=request_options)",
                op.http_method.as_str()
            )
            .unwrap();
            writeln!(out, "        return None").unwrap();
        }
    }
}

fn emit_resources_init(api: &Api) -> String {
    let mut out = String::from("# Code generated by redwood. DO NOT EDIT.\n");
    for resource in &api.resources {
        writeln!(
            out,
            "from .{} import {}",
            resource.ident,
            class_name(resource)
        )
        .unwrap();
    }
    out
}

// ---- client ------------------------------------------------------------------

fn emit_client(api: &Api, pkg: &str, config: &crate::config::PythonConfig) -> String {
    let _ = pkg;
    let mut out = String::from(
        "# Code generated by redwood. DO NOT EDIT.\nfrom __future__ import annotations\n\n\
         import json\nimport os\nfrom typing import Any, Mapping, Optional\n\n\
         import httpx\n\nfrom . import types\nfrom ._core import Core, decode_response\n",
    );
    if !api.webhooks.is_empty() {
        out.push_str("from ._webhooks import verify_webhook\n");
    }
    out.push_str("from .resources import (\n");
    for resource in api.resources.iter().filter(|r| r.parent.is_none()) {
        writeln!(out, "    {},", class_name(resource)).unwrap();
    }
    out.push_str(")\n\n\n");

    let name = &api.name;
    writeln!(out, "class {name}:").unwrap();
    writeln!(
        out,
        "    \"\"\"The {name} API client. Resource groups are attributes; unset\n    options are read from the environment.\"\"\"\n"
    )
    .unwrap();
    writeln!(out, "    def __init__(").unwrap();
    writeln!(out, "        self,").unwrap();
    writeln!(out, "        *,").unwrap();
    writeln!(out, "        api_key: Optional[str] = None,").unwrap();
    writeln!(out, "        base_url: Optional[str] = None,").unwrap();
    if !api.webhooks.is_empty() {
        writeln!(out, "        webhook_secret: Optional[str] = None,").unwrap();
    }
    writeln!(out, "        http_client: Optional[httpx.Client] = None,").unwrap();
    writeln!(out, "        max_retries: int = {},", api.max_retries).unwrap();
    for c in &api.client_params {
        writeln!(
            out,
            "        {}: Optional[str] = None,",
            py_name(&c.wire_name)
        )
        .unwrap();
    }
    writeln!(out, "    ) -> None:").unwrap();
    // Presence is not validity: explicitly supplied blank values are
    // configuration mistakes and never silently fall back to env values.
    // Non-blank explicit values are trimmed once, like Go/Ruby/TS, so
    // "  key  " never reaches the Authorization header verbatim.
    write!(
        out,
        r#"        for _name, _value in (("api_key", api_key), ("base_url", base_url)):
            if _value is not None and not _value.strip():
                raise ValueError(f"{{_name}} must not be blank")
        api_key = api_key.strip() if api_key is not None else None
        base_url = base_url.strip() if base_url is not None else None
"#
    )
    .unwrap();
    for c in &api.client_params {
        let name = py_name(&c.wire_name);
        writeln!(out, "        if {name} is not None and not {name}.strip():").unwrap();
        writeln!(
            out,
            "            raise ValueError(\"{name} must not be blank\")"
        )
        .unwrap();
        writeln!(
            out,
            "        {name} = {name}.strip() if {name} is not None else None"
        )
        .unwrap();
    }
    if !matches!(api.auth, Auth::None) {
        // Authenticated APIs require a credential; a no-security API
        // constructs unauthenticated and demands nothing.
        writeln!(
            out,
            "        api_key = api_key or os.environ.get(\"{}\", \"\").strip() or None",
            api.api_key_env
        )
        .unwrap();
        writeln!(out, "        if not api_key:").unwrap();
        writeln!(
            out,
            "            raise ValueError(\"missing API key: pass api_key or set {}\")",
            api.api_key_env
        )
        .unwrap();
    }
    writeln!(
        out,
        "        base_url = base_url or os.environ.get(\"{}_BASE_URL\", \"\").strip() or \"{}\"",
        api.name.to_uppercase(),
        api.base_url
    )
    .unwrap();
    if !api.webhooks.is_empty() {
        writeln!(
            out,
            "        if webhook_secret is not None and not webhook_secret.strip():"
        )
        .unwrap();
        writeln!(
            out,
            "            raise ValueError(\"webhook_secret must not be blank\")"
        )
        .unwrap();
        writeln!(out, "        webhook_secret = webhook_secret.strip() if webhook_secret is not None else None").unwrap();
        writeln!(
            out,
            "        self._webhook_secret = webhook_secret or os.environ.get(\"{}\", \"\").strip()",
            api.webhook_env
        )
        .unwrap();
    }
    writeln!(out, "        defaults = {{}}").unwrap();
    for c in &api.client_params {
        writeln!(
            out,
            "        defaults[\"{}\"] = {} or os.environ.get(\"{}\", \"\")",
            c.wire_name,
            py_name(&c.wire_name),
            c.env_var
        )
        .unwrap();
    }
    let auth = match &api.auth {
        Auth::Bearer => "(\"Authorization\", \"Bearer \" + api_key)".to_string(),
        Auth::ApiKeyHeader(h) => format!("(\"{h}\", api_key)"),
        Auth::None => "(\"\", \"\")".to_string(),
    };
    writeln!(out, "        core = Core(").unwrap();
    writeln!(out, "            base_url=base_url,").unwrap();
    writeln!(out, "            auth_header={auth},").unwrap();
    writeln!(out, "            http_client=http_client,").unwrap();
    writeln!(out, "            max_retries=max_retries,").unwrap();
    writeln!(out, "            defaults=defaults,").unwrap();
    writeln!(
        out,
        "            user_agent=\"{}-python/{} (api {})\",",
        api.name.to_lowercase(),
        config.package_version.as_deref().unwrap_or("0.1.0"),
        api.version
    )
    .unwrap();
    writeln!(out, "        )").unwrap();
    writeln!(out, "        self._core = core").unwrap();
    for resource in api.resources.iter().filter(|r| r.parent.is_none()) {
        writeln!(
            out,
            "        self.{} = {}(core)",
            resource.name,
            class_name(resource)
        )
        .unwrap();
    }
    write!(
        out,
        r#"
    def close(self) -> None:
        """Release the underlying HTTP transport (only if the client owns it)."""
        self._core.close()

    def __enter__(self) -> "{name}":
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()
"#,
        name = api.name
    )
    .unwrap();

    if !api.webhooks.is_empty() {
        let payload = webhook_payload(api);
        let (annotation, decode) = match &payload {
            Some(ty) => (
                py_ty(api, ty, "types."),
                decode_expr(api, ty, "json.loads(payload)", "types."),
            ),
            None => ("Any".to_string(), "json.loads(payload)".to_string()),
        };
        write!(
            out,
            r#"
    def unwrap_webhook(self, payload: bytes, headers: Mapping[str, str]) -> {annotation}:
        """Verify a Standard Webhooks delivery and return the typed payload."""
        if not self._webhook_secret:
            raise ValueError("missing webhook secret: pass webhook_secret or set {env}")
        return unwrap_webhook(payload, headers, secret=self._webhook_secret)


def unwrap_webhook(
    payload: bytes, headers: Mapping[str, str], *, secret: str
) -> {annotation}:
    """Verify a Standard Webhooks delivery and return the typed payload,
    without constructing a client — webhook-only consumers need no API key."""
    if not secret:
        raise ValueError("missing webhook secret")
    verify_webhook(secret, payload, headers)
    return {decode}
"#,
            env = api.webhook_env,
        )
        .unwrap();
    }
    out
}

/// The single webhook payload type when all events share one; None means
/// heterogeneous payloads (callers get raw JSON).
fn webhook_payload(api: &Api) -> Option<Ty> {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for w in &api.webhooks {
        names.insert(format!("{:?}", w.payload));
    }
    if names.len() == 1 {
        Some(api.webhooks[0].payload.clone())
    } else {
        None
    }
}

fn emit_init(api: &Api) -> String {
    format!(
        "# Code generated by redwood. DO NOT EDIT.\n\
         from . import types\n\
         from ._client import {name}{unwrap_import}\n\
         from ._core import APIConnectionError, APIError, APIResponseError, RequestOptions\n\
         from ._pagination import SyncPage\n\
         from ._sse import ServerSentEvent, Stream\n{wh_import}\n\
         __all__ = [\n    \"{name}\",\n    \"APIError\",\n    \"APIConnectionError\",\n    \"APIResponseError\",\n    \"RequestOptions\",\n    \"SyncPage\",\n    \"Stream\",\n    \"ServerSentEvent\",\n{wh_all}    \"types\",\n]\n",
        name = api.name,
        unwrap_import = if api.webhooks.is_empty() { "" } else { ", unwrap_webhook" },
        wh_import = if api.webhooks.is_empty() {
            ""
        } else {
            "from ._webhooks import WebhookVerificationError, verify_webhook\n"
        },
        wh_all = if api.webhooks.is_empty() {
            "".to_string()
        } else {
            "    \"WebhookVerificationError\",\n    \"verify_webhook\",\n    \"unwrap_webhook\",\n".to_string()
        },
    )
}

fn emit_pyproject(api: &Api, pkg: &str, config: &crate::config::PythonConfig) -> String {
    let version = config.package_version.as_deref().unwrap_or("0.1.0");
    let license = config.license.as_deref().unwrap_or("UNLICENSED");
    let authors = match &config.authors {
        Some(names) if !names.is_empty() => {
            let entries: Vec<String> = names
                .iter()
                .map(|n| format!("{{ name = \"{n}\" }}"))
                .collect();
            format!("authors = [{}]\n", entries.join(", "))
        }
        _ => String::new(),
    };
    let mut urls = String::new();
    for (label, value) in [
        ("Homepage", &config.homepage),
        ("Repository", &config.repository),
        ("Changelog", &config.changelog),
    ] {
        if let Some(url) = value {
            urls.push_str(&format!("{label} = \"{url}\"\n"));
        }
    }
    let urls_section = if urls.is_empty() {
        String::new()
    } else {
        format!("\n[project.urls]\n{urls}")
    };
    format!(
        r#"[project]
name = "{pkg}"
version = "{version}"
description = "The official Python SDK for the {name} API"
readme = "README.md"
license = {{ text = "{license}" }}
{authors}requires-python = ">=3.9"
dependencies = ["httpx>=0.27", "typing_extensions>=4.0"]
classifiers = [
  "Intended Audience :: Developers",
  "Operating System :: OS Independent",
  "Programming Language :: Python :: 3",
  "Typing :: Typed",
]
{urls_section}
[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[tool.hatch.build.targets.wheel]
packages = ["{pkg}"]

# The README (PyPI long description) refers to the full reference; ship it
# inside the package so the pointer resolves for installed users.
[tool.hatch.build.targets.wheel.force-include]
"api.md" = "{pkg}/api.md"
"#,
        name = api.name
    )
}

// ---- conformance driver --------------------------------------------------------

/// Render a manifest sample as a Python literal.
pub(crate) fn py_literal(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "None".into(),
        serde_json::Value::Bool(true) => "True".into(),
        serde_json::Value::Bool(false) => "False".into(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(_) => value.to_string(),
        serde_json::Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(py_literal).collect();
            format!("[{}]", inner.join(", "))
        }
        serde_json::Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{}: {}",
                        serde_json::Value::String(k.clone()),
                        py_literal(v)
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

fn emit_conformance(api: &Api, pkg: &str) -> String {
    let mut out = format!(
        r#"# Code generated by redwood. DO NOT EDIT.
# Conformance driver: calls every operation against the mock at MOCK_URL.
import os
import sys

import {pkg} as _pkg
from {pkg} import {name}

print(f"{pkg} loaded from: {{_pkg.__file__}}", file=sys.stderr)

failures = 0


def run(op_id, fn):
    global failures
    try:
        fn()
        print(f"PASS {{op_id}}")
    except Exception as exc:  # noqa: BLE001
        failures += 1
        print(f"FAIL {{op_id}}: {{exc}}")


def check_page(page):
    # Auto-iterate: the mock serves two pages and rejects the second when
    # any non-cursor query param drifts from the first request.
    total = sum(1 for _ in page)
    assert total == 2, f"expected 2 items across pages, got {{total}}"


def check_stream(stream):
    count = sum(1 for _ in stream)
    assert count == 2, f"expected 2 events, got {{count}}"


client = {name}({conf_auth}base_url=os.environ["MOCK_URL"])

"#,
        name = api.name,
        conf_auth = if matches!(api.auth, Auth::None) {
            ""
        } else {
            "api_key=\"conformance-key\", "
        }
    );

    for resource in &api.resources {
        for op in &resource.operations {
            let mut args: Vec<String> = Vec::new();
            for p in &op.positionals {
                let sample = super::manifest_sample(api, &p.ty);
                args.push(py_literal(&sample));
            }
            for p in op.path_params.iter().chain(op.query_params.iter()) {
                let sample = super::manifest_sample(api, &p.ty);
                args.push(format!("{}={}", py_name(&p.wire_name), py_literal(&sample)));
            }
            for f in &op.body_fields {
                // snake_case nested keys: the SDK's request encoders must
                // translate them back to wire names for the mock to accept.
                let sample = super::snake_sample(api, &f.ty, super::manifest_sample(api, &f.ty));
                args.push(format!("{}={}", py_name(&f.wire_name), py_literal(&sample)));
            }
            if let Some(ty) = &op.whole_body {
                let sample = super::snake_sample(api, ty, super::manifest_sample(api, ty));
                args.push(format!("body={}", py_literal(&sample)));
            }
            let invoke = format!(
                "client.{}.{}({})",
                resource.path(),
                py_name(&op.name),
                args.join(", ")
            );
            let wrapped = match (&op.pagination, &op.response) {
                (Some(_), _) => format!("check_page({invoke})"),
                (None, ResponseKind::Sse(_)) => format!("check_stream({invoke})"),
                _ => invoke,
            };
            writeln!(out, "run(\"{}\", lambda: {})", op.id, wrapped).unwrap();
        }
    }
    writeln!(out, "\nsys.exit(min(failures, 1))").unwrap();
    out
}
