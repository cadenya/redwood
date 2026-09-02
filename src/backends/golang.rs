//! Go backend: emits a dependency-less, interface-per-resource SDK.
//! Every resource group is an accessor method on Client returning an
//! interface (`client.Objectives() ObjectiveResources`), so consumers can
//! mock by implementing the interface.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use heck::{ToSnakeCase, ToUpperCamelCase};

use crate::backends::{Backend, FileSet};
use crate::config::GoConfig;
use crate::ir::*;

const RT_CORE: &str = include_str!("../../runtime/go/core.go");
const RT_PAGINATION: &str = include_str!("../../runtime/go/pagination.go");
const RT_SSE: &str = include_str!("../../runtime/go/sse.go");
const RT_WEBHOOKS: &str = include_str!("../../runtime/go/webhooks.go");

pub struct GoBackend {
    pub config: GoConfig,
}

const GO_SUM: &str = "\
github.com/davecgh/go-spew v1.1.1 h1:vj9j/u1bqnvCEfJOwUhtlOARqs3+rkHYY13jYWTU97c=\n\
github.com/davecgh/go-spew v1.1.1/go.mod h1:J7Y8YcW2NihsgmVo/mv3lAwl/skON4iLHjSsI+c5H38=\n\
github.com/pmezard/go-difflib v1.0.0 h1:4DBwDE0NGyQoBHbLQYPwSUPoCMWR5BEzIk/f1lZbAQM=\n\
github.com/pmezard/go-difflib v1.0.0/go.mod h1:iKH77koFhYxTK1pcRnkKkqfTogsbg7gZNVY4sRDYZ/4=\n\
github.com/stretchr/testify v1.11.1 h1:7s2iGBzp5EwR7/aIZr8ao5+dra3wiQyKjjFuvgVKu7U=\n\
github.com/stretchr/testify v1.11.1/go.mod h1:wZwfW3scLgRK+23gO65QZefKpKQRnfz6sD981Nm4B6U=\n\
github.com/tmaxmax/go-sse v0.11.0 h1:nogmJM6rJUoOLoAwEKeQe5XlVpt9l7N82SS1jI7lWFg=\n\
github.com/tmaxmax/go-sse v0.11.0/go.mod h1:u/2kZQR1tyngo1lKaNCj1mJmhXGZWS1Zs5yiSOD+Eg8=\n\
gopkg.in/yaml.v3 v3.0.1 h1:fxVm/GzAzEWqLHuvctI91KS9hhNmmWOoWu0XTYJS7CA=\n\
gopkg.in/yaml.v3 v3.0.1/go.mod h1:K4uyk7z7BCEPqu6E+C64Yfv1cQ7kz7rIZviUmN+EgEM=\n";

impl Backend for GoBackend {
    fn name(&self) -> &'static str {
        "go"
    }

    fn generate(&self, api: &Api) -> anyhow::Result<FileSet> {
        install_config_casings(&self.config.special_casings);
        let pkg = self.package_name(api);
        let module = self.module_path(api);
        let mut files = FileSet::new();
        let vendored = |body: &str| format!("package {pkg}\n\n{body}");
        files.insert("core.go".into(), vendored(RT_CORE));
        files.insert("pagination.go".into(), vendored(RT_PAGINATION));
        files.insert("sse.go".into(), vendored(RT_SSE));
        if !api.webhooks.is_empty() {
            files.insert("webhooks_verify.go".into(), vendored(RT_WEBHOOKS));
        }
        files.insert("types.go".into(), emit_types(api, &pkg));
        for resource in &api.resources {
            files.insert(
                format!("resource_{}.go", resource.ident),
                emit_resource(api, resource, &pkg),
            );
        }
        files.insert("client.go".into(), emit_client(api, &pkg, &self.config));
        files.insert(
            "go.mod".into(),
            format!(
                "module {module}\n\ngo 1.22\n\nrequire github.com/tmaxmax/go-sse v0.11.0\n\nrequire github.com/stretchr/testify v1.11.1\n\nrequire (\n\tgithub.com/davecgh/go-spew v1.1.1 // indirect\n\tgithub.com/pmezard/go-difflib v1.0.0 // indirect\n\tgopkg.in/yaml.v3 v3.0.1 // indirect\n)\n"
            ),
        );
        // Pinned hashes so `go test` needs no network for module verification.
        files.insert("go.sum".into(), GO_SUM.to_string());
        files.insert(
            "conformance/main.go".into(),
            emit_conformance(api, &module, &pkg),
        );
        files.insert("api.md".into(), emit_api_md(api));
        files.insert("README.md".into(), emit_readme(api, &module, &pkg));
        super::golang_testsuite::emit(api, &pkg, &module, &mut files);
        // Error prefixes in the vendored runtime and emitted code carry the
        // GENERATED package identity, not the primary fixture's name.
        let prefix = format!("{pkg}: ");
        for contents in files.values_mut() {
            *contents = contents.replace("cadenya: ", &prefix);
        }
        Ok(files)
    }
}

// ---- docs --------------------------------------------------------------------

/// Accessor expression for a resource, e.g. `client.Agents().Variations()`.
fn accessor_chain(resource: &Resource) -> String {
    match &resource.parent {
        Some(parent) => format!("client.{}().{}()", go_name(parent), go_name(&resource.name)),
        None => format!("client.{}()", go_name(&resource.name)),
    }
}

fn emit_api_md(api: &Api) -> String {
    let mut out = format!(
        "# {} Go SDK reference\n\nEvery call takes a `context.Context` and returns an error; see README.md for usage patterns.\n",
        api.name
    );
    for resource in &api.resources {
        writeln!(out, "\n## {}\n", resource.path()).unwrap();
        for op in &resource.operations {
            if let Some(s) = &op.summary {
                writeln!(out, "{}\n", s.trim().lines().next().unwrap_or("")).unwrap();
            }
            writeln!(
                out,
                "```go\n{}.{}\n```",
                accessor_chain(resource),
                method_signature(api, resource, op)
            )
            .unwrap();
        }
    }
    if !api.webhooks.is_empty() {
        writeln!(out, "\n## webhooks\n").unwrap();
        writeln!(
            out,
            "```go\nVerifyWebhook(secret string, payload []byte, headers http.Header) error\nUnwrapWebhook(secret string, payload []byte, headers http.Header) (*{}, error)\nclient.Webhooks().Unwrap(payload []byte, headers http.Header) (*{}, error)\n```",
            webhook_payload_ty(api),
            webhook_payload_ty(api)
        )
        .unwrap();
    }
    out
}

fn emit_readme(api: &Api, module: &str, pkg: &str) -> String {
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
    let retrieve_call = ops()
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
                "{}.{}(context.Background())",
                accessor_chain(r),
                go_name(&o.name)
            )
        });
    let getting_started_call = retrieve_call
        .map(|e| {
            format!(
                "\tresult, err := {e}\n\tif err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n\t_ = result"
            )
        })
        .unwrap_or_else(|| "\t// See api.md for every method signature.\n\t_ = client".to_string());
    let pagination_section = ops()
        .find(|(_, o)| o.pagination.is_some() && o.positionals.is_empty())
        .map(|(r, o)| {
            format!(
                "\n## Pagination\n\n```go\npage, err := {}.{}(ctx, nil)\nitems, err := page.All(ctx) // fetches every page\n// or step manually: page.HasNextPage(), page.GetNextPage(ctx)\n```\n",
                accessor_chain(r),
                go_name(&o.name)
            )
        })
        .unwrap_or_default();
    let streaming_section = ops()
        .find(|(_, o)| matches!(o.response, ResponseKind::Sse(_)) && o.pagination.is_none())
        .map(|(r, o)| {
            let pos: String = o
                .positionals
                .iter()
                .map(|p| format!("{}, ", go_local(&p.wire_name)))
                .collect();
            let acc = format!("{}.{}", accessor_chain(r), go_name(&o.name));
            format!(
                "\n## Streaming (SSE)\n\nEvery successfully constructed stream must be consumed and closed —\n`defer` captures the receiver at statement time, so a REPLAY stream needs\nits own name, error check, and Close:\n\n```go\nstream, err := {acc}(ctx, {pos}params)\nif err != nil {{ return err }}\ndefer stream.Close()\nfor stream.Next() {{\n\tevent := stream.Current()\n\t_ = event\n}}\nif err := stream.Err(); err != nil {{ return err }}\n\n// Resume after a disconnect: pass the last checkpoint back — as a NEW,\n// separately closed stream (reassigning the first variable would leak the\n// replay: the earlier defer still points at the old stream).\nresumed, err := {acc}(ctx, {pos}params, {pkg}.WithLastEventID(stream.LastEventID()))\nif err != nil {{ return err }}\ndefer resumed.Close()\nfor resumed.Next() {{\n\t_ = resumed.Current()\n}}\nif err := resumed.Err(); err != nil {{ return err }}\n```\n"
            )
        })
        .unwrap_or_default();
    let webhooks_section = if api.webhooks.is_empty() {
        String::new()
    } else {
        format!(
            "\n## Webhooks (no API key required)\n\n```go\n// Standard Webhooks verification; secret comes from {wh_env}.\nevent, err := {pkg}.UnwrapWebhook(secret, payload, r.Header)\n```\n",
            wh_env = api.webhook_env
        )
    };
    let env_comment = if matches!(api.auth, Auth::None) {
        "\t// This API requires no authentication.".to_string()
    } else {
        format!(
            "\t// Reads {api_env}{ws_env_note} from the environment\n\t// when the options are omitted. Explicitly supplied blank values are\n\t// errors and never fall back to the environment."
        )
    };
    format!(
        r#"# {name} Go SDK

The official Go client for the {name} API. Generated by redwood.

## Install

```sh
go get {module}
```

## Getting started

```go
import (
	"context"
	"log"

	{pkg} "{module}"
)

func main() {{
{env_comment}
	client, err := {pkg}.NewClient()
	if err != nil {{
		log.Fatal(err)
	}}

{getting_started_call}
}}
```

## Errors

Non-2xx responses return `*APIError` (google.rpc Status: `StatusCode`,
`Code`, `Message`, `Details`), compatible with `errors.As`. Requests that
never reached the server return connection errors.

## Retries

Automatic retries apply only to idempotent methods (GET/HEAD/PUT/DELETE)
and default to 0. Enable with `WithMaxRetries(n)`; opt a single mutation in
with `WithRequestRetries(n)`. `Retry-After` (seconds or HTTP-date) is
honored. Ordinary calls get a 60s context deadline when the caller sets
none; streaming responses never do.

{pagination_section}{streaming_section}{webhooks_section}
## Reference

See [api.md](api.md) for every method signature.
"#
    )
}

impl GoBackend {
    fn package_name(&self, api: &Api) -> String {
        self.config
            .package_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase())
    }
    fn module_path(&self, api: &Api) -> String {
        self.config
            .module_path
            .clone()
            .unwrap_or_else(|| format!("example.com/{}-go", api.name.to_lowercase()))
    }
}

// ---- naming ----------------------------------------------------------------

const INITIALISMS: &[&str] = &[
    "id", "url", "uri", "api", "http", "https", "json", "sse", "hmac", "mcp", "ai", "mime",
];

/// Words whose canonical Go casing is mixed rather than all-caps. Only
/// GENERIC vocabulary lives here; vendor/product words come from the target
/// config (`special_casings` in go.config.toml) — the generator must know
/// nothing about any particular schema.
const SPECIAL_CASINGS: &[(&str, &str)] = &[("openapi", "OpenAPI")];

// Config-supplied vendor casings. Thread-local and REPLACED on every
// generate() call, so output is a pure function of (API, backend config):
// a process generating two differently configured SDKs — sequentially or
// on concurrent threads — never sees another generation's casing policy.
thread_local! {
    static CONFIG_CASINGS: std::cell::RefCell<Vec<(String, String)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install vendor casings from target config (Go SDK and CLI share Go
/// naming, so both backends call this at the start of every generate()).
pub(crate) fn install_config_casings(casings: &indexmap::IndexMap<String, String>) {
    CONFIG_CASINGS.with(|cell| {
        *cell.borrow_mut() = casings
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect();
    });
}

fn special_casing(word: &str) -> Option<String> {
    if let Some((_, canonical)) = SPECIAL_CASINGS.iter().find(|(from, _)| *from == word) {
        return Some((*canonical).to_string());
    }
    CONFIG_CASINGS.with(|cell| {
        cell.borrow()
            .iter()
            .find(|(from, _)| from == word)
            .map(|(_, canonical)| canonical.clone())
    })
}

/// Go-style exported identifier from a wire/snake name, honoring initialisms:
/// workspaceId -> WorkspaceID, apiKey -> APIKey, openai -> OpenAI.
pub(crate) fn go_name(name: &str) -> String {
    name.to_snake_case()
        .split('_')
        .filter(|w| !w.is_empty())
        .map(|w| {
            if let Some(canonical) = special_casing(w) {
                canonical
            } else if INITIALISMS.contains(&w) {
                w.to_uppercase()
            } else {
                let mut c = w.chars();
                match c.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
                    None => String::new(),
                }
            }
        })
        .collect()
}

/// Unexported variant of go_name. A leading initialism lowercases as a unit
/// so "ID" becomes "id" (not "iD") and "APIKeys" becomes "apiKeys".
fn go_local(name: &str) -> String {
    let n = go_name(name);
    let chars: Vec<char> = n.chars().collect();
    let upper_run = chars.iter().take_while(|c| c.is_ascii_uppercase()).count();
    match upper_run {
        0 => n,
        // Single capital: plain first-letter lowercase.
        1 => chars[0].to_ascii_lowercase().to_string() + &n[1..],
        // "APIKeys": the last capital starts the next word — lowercase "API".
        // "ID" (all caps): lowercase the whole run.
        run if run == chars.len() => n.to_lowercase(),
        run => {
            let head: String = chars[..run - 1]
                .iter()
                .map(|c| c.to_ascii_lowercase())
                .collect();
            let tail: String = chars[run - 1..].iter().collect();
            head + &tail
        }
    }
}

/// Names claimed by the vendored runtime; colliding models get a suffix.
const RESERVED_TYPE_NAMES: &[&str] = &[
    "Page",
    "Stream",
    "Client",
    "Option",
    "RequestOption",
    "APIError",
];

/// Type names keep their spec structure (underscores allowed in Go) but each
/// segment is normalized to Go initialism casing: AiProviderConfig_Openai
/// becomes AIProviderConfig_OpenAI.
pub(crate) fn go_type_name(name: &str) -> String {
    let base: String = name.split('_').map(go_name).collect::<Vec<_>>().join("_");
    let base = if base.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
        base
    } else {
        name.to_upper_camel_case()
    };
    if RESERVED_TYPE_NAMES.contains(&base.as_str()) {
        format!("{base}Model")
    } else {
        base
    }
}

fn singular(word: &str) -> String {
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

fn interface_name(resource: &Resource) -> String {
    let singular_name: String = resource
        .ident
        .split('_')
        .map(singular)
        .collect::<Vec<_>>()
        .join("_");
    format!("{}Resources", go_name(&singular_name))
}

fn service_name(resource: &Resource) -> String {
    format!("{}Service", go_local(&resource.ident))
}

/// Exported-name rules, shared with the CLI backend.
pub fn exported_name(name: &str) -> String {
    go_name(name)
}

/// Params-struct naming, shared with the CLI backend.
pub fn params_type_name_pub(resource: &Resource, op: &Operation) -> String {
    params_type_name(resource, op)
}

pub(crate) fn params_type_name(resource: &Resource, op: &Operation) -> String {
    let singular_name: String = resource
        .ident
        .split('_')
        .map(singular)
        .collect::<Vec<_>>()
        .join("_");
    format!("{}{}Params", go_name(&singular_name), go_name(&op.name))
}

// ---- type mapping ----------------------------------------------------------

/// Resolve aliases down to the underlying shape category.
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

/// True when the type maps to a Go scalar-ish value (no pointer needed for
/// required fields, comparable with zero values).
pub(crate) fn is_scalarish(api: &Api, ty: &Ty) -> bool {
    match ty {
        Ty::String
        | Ty::Bool
        | Ty::Int32
        | Ty::Int64
        | Ty::Float
        | Ty::Double
        | Ty::Timestamp
        | Ty::Bytes
        | Ty::Literal(_) => true,
        Ty::Json | Ty::List(_) | Ty::Map(_) => true, // any/slice/map: nil-able already
        Ty::Named(n) => matches!(
            resolved_shape(api, n),
            Some(Shape::Enum(_)) | Some(Shape::Alias(_)) | None
        ),
    }
}

pub(crate) fn go_ty(_api: &Api, ty: &Ty) -> String {
    match ty {
        Ty::String | Ty::Literal(_) => "string".into(),
        Ty::Bool => "bool".into(),
        Ty::Int32 => "int32".into(),
        Ty::Int64 => "int64".into(),
        Ty::Float => "float32".into(),
        Ty::Double => "float64".into(),
        Ty::Timestamp => "time.Time".into(),
        Ty::Bytes => "[]byte".into(),
        Ty::Json => "any".into(),
        Ty::Named(n) => go_type_name(n),
        // Elements are value types: the slice/map header already makes the
        // field nil-able, and json decoding addresses elements directly.
        Ty::List(inner) => format!("[]{}", go_ty(_api, inner)),
        Ty::Map(inner) => format!("map[string]{}", go_ty(_api, inner)),
    }
}

/// Integer width through component aliases. OpenAPI commonly models IDs as
/// a named `integer` schema, and path methods must preserve that width rather
/// than silently turning the ID back into a string.
fn integer_width(api: &Api, ty: &Ty) -> Option<u8> {
    let mut current = ty;
    for _ in 0..8 {
        match current {
            Ty::Int32 => return Some(32),
            Ty::Int64 => return Some(64),
            Ty::Named(name) => match api.types.get(name).map(|decl| &decl.shape) {
                Some(Shape::Alias(inner)) => current = inner,
                _ => return None,
            },
            _ => return None,
        }
    }
    None
}

/// A compile-time sample for the public positional type. String path tokens
/// retain the traditional sample while integer IDs exercise their width.
pub(crate) fn positional_sample(api: &Api, ty: &Ty) -> String {
    match integer_width(api, ty) {
        Some(64) => "4294967296".to_string(),
        Some(32) => "1".to_string(),
        _ => "\"sample\"".to_string(),
    }
}

/// Type expression for a field: structs/unions are always pointers (nil =
/// absent, and it keeps recursive types finite); scalars are pointers only
/// when optional.
fn field_ty(api: &Api, ty: &Ty, required: bool) -> String {
    let base = go_ty(api, ty);
    let needs_pointer = match ty {
        Ty::Named(_) if !is_scalarish(api, ty) => true,
        Ty::List(_) | Ty::Map(_) | Ty::Json | Ty::Bytes => false,
        _ => !required,
    };
    if needs_pointer {
        format!("*{base}")
    } else {
        base
    }
}

/// Fluent builder companion for a params struct: zero value is usable,
/// setters chain, ToParams() hands the SDK method its input.
fn emit_builder(api: &Api, resource: &Resource, op: &Operation, out: &mut String) {
    let params_name = params_type_name(resource, op);
    let builder_name = params_name
        .strip_suffix("Params")
        .map(|b| format!("{b}Builder"))
        .unwrap_or_else(|| format!("{params_name}Builder"));

    writeln!(out, "// {builder_name} builds a {params_name} fluently.").unwrap();
    writeln!(out, "type {builder_name} struct {{").unwrap();
    writeln!(out, "\tparams {params_name}").unwrap();
    writeln!(out, "}}\n").unwrap();

    let divergent = api.divergent_types();
    let mut setter = |wire: &str, ty: &Ty, required: bool| {
        let field = go_name(wire);
        // Setters mirror the params struct exactly, so request-position
        // fields use the input view type.
        let (arg, assign) = match ty {
            Ty::List(inner) => (
                format!("v ...{}", go_input_ty(api, &divergent, inner)),
                format!("b.params.{field} = v"),
            ),
            Ty::Map(_) | Ty::Json | Ty::Bytes => (
                format!("v {}", go_input_ty(api, &divergent, ty)),
                format!("b.params.{field} = v"),
            ),
            Ty::Named(_) if !is_scalarish(api, ty) => (
                format!("v *{}", go_input_ty(api, &divergent, ty)),
                format!("b.params.{field} = v"),
            ),
            _ if required => (
                format!("v {}", go_input_ty(api, &divergent, ty)),
                format!("b.params.{field} = v"),
            ),
            _ => (
                format!("v {}", go_input_ty(api, &divergent, ty)),
                format!("b.params.{field} = &v"),
            ),
        };
        writeln!(
            out,
            "func (b *{builder_name}) {field}({arg}) *{builder_name} {{\n\t{assign}\n\treturn b\n}}\n"
        )
        .unwrap();
    };
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        let required = p.required && client_param(api, &p.wire_name).is_none();
        setter(&p.wire_name, &p.ty, required);
    }
    for f in &op.body_fields {
        setter(&f.wire_name, &f.ty, f.required);
    }
    if let Some(ty) = &op.whole_body {
        setter("body", ty, true);
    }

    writeln!(
        out,
        "// ToParams returns the built params, ready to pass to the SDK method."
    )
    .unwrap();
    writeln!(
        out,
        "func (b *{builder_name}) ToParams() *{params_name} {{\n\tp := b.params\n\treturn &p\n}}\n"
    )
    .unwrap();
}

// ---- imports ---------------------------------------------------------------

#[derive(Default)]
struct Imports(BTreeSet<&'static str>);

impl Imports {
    fn add(&mut self, path: &'static str) {
        self.0.insert(path);
    }
    fn render(&self) -> String {
        if self.0.is_empty() {
            return String::new();
        }
        let mut out = String::from("import (\n");
        for path in &self.0 {
            writeln!(out, "\t\"{path}\"").unwrap();
        }
        out.push_str(")\n\n");
        out
    }
}

fn doc_comment(out: &mut String, indent: &str, text: &str) {
    for line in text.trim().lines() {
        writeln!(out, "{indent}// {}", line.trim_end()).unwrap();
    }
}

// ---- types.go ----------------------------------------------------------------

fn emit_types(api: &Api, pkg: &str) -> String {
    let mut body = String::new();
    let mut imports = Imports::default();
    for decl in api.types.values() {
        emit_type_decl(api, &mut body, &mut imports, decl);
        body.push('\n');
    }
    emit_input_decls(api, &mut body, &mut imports);
    if body.contains("time.Time") {
        imports.add("time");
    }
    format!(
        "// Code generated by redwood. DO NOT EDIT.\n\npackage {pkg}\n\n{}{}",
        imports.render(),
        body
    )
}

fn emit_type_decl(api: &Api, out: &mut String, imports: &mut Imports, decl: &TypeDecl) {
    if let Some(d) = &decl.description {
        doc_comment(out, "", d);
    }
    let name = go_type_name(&decl.name);
    match &decl.shape {
        Shape::Struct(s) => {
            if s.fields.is_empty() {
                if let Some(additional) = &s.additional {
                    writeln!(
                        out,
                        "type {name} map[string]{}",
                        field_ty(api, additional, true)
                    )
                    .unwrap();
                    return;
                }
            }
            writeln!(out, "type {name} struct {{").unwrap();
            // Response direction: writeOnly fields are input secrets and are
            // never promised (or exposed) in output models.
            for f in s.output_fields() {
                if let Some(d) = &f.description {
                    doc_comment(out, "\t", d);
                }
                let omit = if f.required { "" } else { ",omitempty" };
                writeln!(
                    out,
                    "\t{} {} `json:\"{}{omit}\"`",
                    go_name(&f.wire_name),
                    field_ty(api, &f.ty, f.required),
                    f.wire_name,
                )
                .unwrap();
            }
            writeln!(out, "}}").unwrap();
        }
        Shape::Enum(e) => {
            writeln!(out, "type {name} string").unwrap();
            writeln!(out, "const (").unwrap();
            for v in &e.values {
                writeln!(
                    out,
                    "\t{name}{} {name} = \"{v}\"",
                    go_name(&v.to_lowercase())
                )
                .unwrap();
            }
            writeln!(out, ")").unwrap();
        }
        Shape::Union(u) => emit_union(api, out, imports, &name, u),
        Shape::Alias(ty) => {
            writeln!(out, "type {name} = {}", go_ty(api, ty)).unwrap();
        }
    }
}

/// Request-direction type name: a direction-divergent type is referenced
/// through its generated `XParam` input type; everything else is shared.
pub(crate) fn go_input_type_name(
    api: &Api,
    divergent: &std::collections::BTreeSet<String>,
    n: &str,
) -> String {
    if divergent.contains(n) && !matches!(resolved_shape(api, n), Some(Shape::Enum(_)) | None) {
        format!("{}Param", go_type_name(n))
    } else {
        go_type_name(n)
    }
}

fn go_input_ty(api: &Api, divergent: &std::collections::BTreeSet<String>, ty: &Ty) -> String {
    match ty {
        Ty::Named(n) => go_input_type_name(api, divergent, n),
        Ty::List(inner) => format!("[]{}", go_input_ty(api, divergent, inner)),
        Ty::Map(inner) => format!("map[string]{}", go_input_ty(api, divergent, inner)),
        _ => go_ty(api, ty),
    }
}

fn input_field_ty(
    api: &Api,
    divergent: &std::collections::BTreeSet<String>,
    ty: &Ty,
    required: bool,
) -> String {
    let base = go_input_ty(api, divergent, ty);
    let needs_pointer = match ty {
        Ty::Named(_) if !is_scalarish(api, ty) => true,
        Ty::List(_) | Ty::Map(_) | Ty::Json | Ty::Bytes => false,
        _ => !required,
    };
    if needs_pointer {
        format!("*{base}")
    } else {
        base
    }
}

/// Input-view (`XParam`) declarations for request-reachable types whose
/// input and output views differ. readOnly fields are omitted entirely, so
/// a server-owned field can never be set (or required) as request input.
/// Input types are never decoded, so unions carry Marshal but no Unmarshal.
fn emit_input_decls(api: &Api, out: &mut String, imports: &mut Imports) {
    let divergent = api.divergent_types();
    let request_types = api.request_reachable();
    for decl in api.types.values() {
        if !divergent.contains(&decl.name) || !request_types.contains(&decl.name) {
            continue;
        }
        let name = format!("{}Param", go_type_name(&decl.name));
        match &decl.shape {
            Shape::Struct(st) => {
                writeln!(
                    out,
                    "// {name} is the request-direction view of {orig}:",
                    orig = go_type_name(&decl.name)
                )
                .unwrap();
                writeln!(
                    out,
                    "// server-owned (readOnly) fields are not accepted as input."
                )
                .unwrap();
                writeln!(out, "type {name} struct {{").unwrap();
                for f in st.input_fields() {
                    if let Some(d) = &f.description {
                        doc_comment(out, "\t", d);
                    }
                    let omit = if f.required { "" } else { ",omitempty" };
                    writeln!(
                        out,
                        "\t{} {} `json:\"{}{omit}\"`",
                        go_name(&f.wire_name),
                        input_field_ty(api, &divergent, &f.ty, f.required),
                        f.wire_name,
                    )
                    .unwrap();
                }
                writeln!(out, "}}\n").unwrap();
            }
            Shape::Union(u) => {
                imports.add("encoding/json");
                imports.add("fmt");
                writeln!(out, "// {name} is the request-direction oneOf view; at most one variant is non-nil.").unwrap();
                writeln!(out, "type {name} struct {{").unwrap();
                for (i, v) in u.variants.iter().enumerate() {
                    let base = go_input_ty(api, &divergent, &v.ty);
                    writeln!(out, "\t{} *{base} `json:\"-\"`", variant_field_name(v, i)).unwrap();
                }
                writeln!(out, "}}\n").unwrap();
                writeln!(out, "func (u {name}) MarshalJSON() ([]byte, error) {{").unwrap();
                writeln!(out, "\tvar chosen any").unwrap();
                writeln!(out, "\tcount := 0").unwrap();
                for (i, v) in u.variants.iter().enumerate() {
                    writeln!(
                        out,
                        "\tif u.{f} != nil {{\n\t\tchosen = u.{f}\n\t\tcount++\n\t}}",
                        f = variant_field_name(v, i)
                    )
                    .unwrap();
                }
                writeln!(
                    out,
                    "\tif count == 0 {{\n\t\treturn []byte(\"{{}}\"), nil\n\t}}"
                )
                .unwrap();
                writeln!(
                    out,
                    "\tif count > 1 {{\n\t\treturn nil, fmt.Errorf(\"{name}: exactly one variant must be set, got %d\", count)\n\t}}"
                )
                .unwrap();
                writeln!(out, "\treturn json.Marshal(chosen)\n}}\n").unwrap();
                for (i, v) in u.variants.iter().enumerate() {
                    let field = variant_field_name(v, i);
                    let base = go_input_ty(api, &divergent, &v.ty);
                    writeln!(
                        out,
                        "// New{name}{field} returns a {name} with the {field} variant selected."
                    )
                    .unwrap();
                    writeln!(out, "func New{name}{field}(v {base}) {name} {{").unwrap();
                    if let (Some(disc), Some(tag)) = (&u.discriminator, &v.tag) {
                        if input_variant_has_discriminator_field(api, &v.ty, &disc.property) {
                            writeln!(out, "\tv.{} = \"{tag}\"", go_name(&disc.property)).unwrap();
                        }
                    }
                    writeln!(out, "\treturn {name}{{{field}: &v}}\n}}\n").unwrap();
                }
                // Input unions ARE decoded: CLI JSON flags and params-file
                // workflows unmarshal user JSON into the input view. Same
                // discriminator policy as the response union.
                writeln!(out, "func (u *{name}) UnmarshalJSON(data []byte) error {{").unwrap();
                writeln!(out, "\t*u = {name}{{}}").unwrap();
                match &u.discriminator {
                    Some(disc) => {
                        writeln!(
                            out,
                            "\tvar probe struct {{\n\t\tTag string `json:\"{}\"`\n\t}}",
                            disc.property
                        )
                        .unwrap();
                        writeln!(out, "\tif err := json.Unmarshal(data, &probe); err != nil {{\n\t\treturn err\n\t}}").unwrap();
                        writeln!(out, "\tif probe.Tag == \"\" {{\n\t\treturn nil\n\t}}").unwrap();
                        writeln!(out, "\tswitch probe.Tag {{").unwrap();
                        for (i, v) in u.variants.iter().enumerate() {
                            let Some(tag) = &v.tag else { continue };
                            let field = variant_field_name(v, i);
                            let base = go_input_ty(api, &divergent, &v.ty);
                            writeln!(out, "\tcase \"{tag}\":").unwrap();
                            writeln!(out, "\t\tu.{field} = new({base})").unwrap();
                            writeln!(out, "\t\treturn json.Unmarshal(data, u.{field})").unwrap();
                        }
                        writeln!(out, "\t}}").unwrap();
                        writeln!(
                            out,
                            "\treturn fmt.Errorf(\"{name}: unknown {} %q\", probe.Tag)",
                            disc.property
                        )
                        .unwrap();
                    }
                    None => {
                        for (i, v) in u.variants.iter().enumerate() {
                            let field = variant_field_name(v, i);
                            let base = go_input_ty(api, &divergent, &v.ty);
                            writeln!(out, "\t{{").unwrap();
                            writeln!(out, "\t\tvalue := new({base})").unwrap();
                            writeln!(
                                out,
                                "\t\tif err := json.Unmarshal(data, value); err == nil {{"
                            )
                            .unwrap();
                            writeln!(out, "\t\t\tu.{field} = value\n\t\t\treturn nil\n\t\t}}")
                                .unwrap();
                            writeln!(out, "\t}}").unwrap();
                        }
                        writeln!(out, "\treturn fmt.Errorf(\"{name}: no variant matched\")")
                            .unwrap();
                    }
                }
                writeln!(out, "}}\n").unwrap();
            }
            Shape::Alias(ty) => {
                writeln!(out, "type {name} = {}\n", go_input_ty(api, &divergent, ty)).unwrap();
            }
            Shape::Enum(_) => {}
        }
    }
}

/// Input-view twin of variant_has_discriminator_field: consults the INPUT
/// fields, since a readOnly discriminator would not survive into the view.
fn input_variant_has_discriminator_field(api: &Api, ty: &Ty, property: &str) -> bool {
    let Ty::Named(n) = ty else { return false };
    match resolved_shape(api, n) {
        Some(Shape::Struct(s)) => s
            .input_fields()
            .any(|f| f.wire_name == property && matches!(f.ty, Ty::Literal(_) | Ty::String)),
        _ => false,
    }
}

/// Whether a union variant's struct carries the discriminator property (as a
/// settable string field) so its constructor can stamp the tag.
fn variant_has_discriminator_field(api: &Api, ty: &Ty, property: &str) -> bool {
    let Ty::Named(n) = ty else { return false };
    match resolved_shape(api, n) {
        Some(Shape::Struct(s)) => s
            .fields
            .iter()
            .any(|f| f.wire_name == property && matches!(f.ty, Ty::Literal(_) | Ty::String)),
        _ => false,
    }
}

pub(crate) fn variant_field_name(variant: &UnionVariant, index: usize) -> String {
    if let Some(tag) = &variant.tag {
        return go_name(tag);
    }
    match &variant.ty {
        Ty::Named(n) => {
            let last = n.rsplit('_').next().unwrap_or(n);
            go_name(&last.to_snake_case())
        }
        Ty::String => "String".into(),
        Ty::Bool => "Bool".into(),
        Ty::Int32 => "Int32".into(),
        Ty::Int64 => "Int64".into(),
        Ty::Float | Ty::Double => "Number".into(),
        _ => format!("Variant{index}"),
    }
}

fn emit_union(api: &Api, out: &mut String, imports: &mut Imports, name: &str, u: &UnionShape) {
    imports.add("encoding/json");
    writeln!(
        out,
        "// {name} is a oneOf union; at most one variant is non-nil. All variants"
    )
    .unwrap();
    writeln!(
        out,
        "// nil means the union was unset (protobuf empty/default) in the response."
    )
    .unwrap();
    writeln!(out, "type {name} struct {{").unwrap();
    for (i, v) in u.variants.iter().enumerate() {
        let base = go_ty(api, &v.ty);
        writeln!(out, "\t{} *{base} `json:\"-\"`", variant_field_name(v, i)).unwrap();
    }
    writeln!(out, "}}\n").unwrap();

    // MarshalJSON: several set variants are a caller bug surfaced as an
    // error. ZERO set variants round-trip as {} — the protobuf-unset form
    // this union may legitimately hold after decoding a live response.
    imports.add("fmt");
    writeln!(out, "func (u {name}) MarshalJSON() ([]byte, error) {{").unwrap();
    writeln!(out, "\tvar chosen any").unwrap();
    writeln!(out, "\tcount := 0").unwrap();
    for (i, v) in u.variants.iter().enumerate() {
        writeln!(
            out,
            "\tif u.{f} != nil {{\n\t\tchosen = u.{f}\n\t\tcount++\n\t}}",
            f = variant_field_name(v, i)
        )
        .unwrap();
    }
    writeln!(
        out,
        "\tif count == 0 {{\n\t\treturn []byte(\"{{}}\"), nil\n\t}}"
    )
    .unwrap();
    writeln!(
        out,
        "\tif count > 1 {{\n\t\treturn nil, fmt.Errorf(\"{name}: exactly one variant must be set, got %d\", count)\n\t}}"
    )
    .unwrap();
    writeln!(out, "\treturn json.Marshal(chosen)\n}}\n").unwrap();

    // Per-variant constructors: selecting a branch by hand is awkward in Go,
    // and setting the discriminator manually invites contradictory values —
    // the constructor stamps it.
    for (i, v) in u.variants.iter().enumerate() {
        let field = variant_field_name(v, i);
        let base = go_ty(api, &v.ty);
        writeln!(
            out,
            "// New{name}{field} returns a {name} with the {field} variant selected."
        )
        .unwrap();
        writeln!(out, "func New{name}{field}(v {base}) {name} {{").unwrap();
        if let (Some(disc), Some(tag)) = (&u.discriminator, &v.tag) {
            if variant_has_discriminator_field(api, &v.ty, &disc.property) {
                writeln!(out, "\tv.{} = \"{tag}\"", go_name(&disc.property)).unwrap();
            }
        }
        writeln!(out, "\treturn {name}{{{field}: &v}}\n}}\n").unwrap();
    }

    writeln!(out, "func (u *{name}) UnmarshalJSON(data []byte) error {{").unwrap();
    // Reset first: reusing a value for a second unmarshal must not keep a
    // stale variant from the previous decode.
    writeln!(out, "\t*u = {name}{{}}").unwrap();
    match &u.discriminator {
        Some(disc) => {
            imports.add("fmt");
            writeln!(
                out,
                "\tvar probe struct {{\n\t\tTag string `json:\"{}\"`\n\t}}",
                disc.property
            )
            .unwrap();
            writeln!(
                out,
                "\tif err := json.Unmarshal(data, &probe); err != nil {{\n\t\treturn err\n\t}}"
            )
            .unwrap();
            // Protobuf JSON gateways encode an unset optional union as null,
            // {}, or a default-value discriminator (""); all three decode as
            // the zero-value union (no variant set) instead of rejecting an
            // otherwise valid response. Unknown NON-empty tags still error.
            writeln!(out, "\tif probe.Tag == \"\" {{\n\t\treturn nil\n\t}}").unwrap();
            writeln!(out, "\tswitch probe.Tag {{").unwrap();
            for (i, v) in u.variants.iter().enumerate() {
                let Some(tag) = &v.tag else { continue };
                let field = variant_field_name(v, i);
                let base = go_ty(api, &v.ty);
                writeln!(out, "\tcase \"{tag}\":").unwrap();
                writeln!(out, "\t\tu.{field} = new({base})").unwrap();
                writeln!(out, "\t\treturn json.Unmarshal(data, u.{field})").unwrap();
            }
            writeln!(out, "\t}}").unwrap();
            writeln!(
                out,
                "\treturn fmt.Errorf(\"{name}: unknown {} %q\", probe.Tag)",
                disc.property
            )
            .unwrap();
        }
        None => {
            // Undiscriminated: first variant that decodes wins.
            for (i, v) in u.variants.iter().enumerate() {
                let field = variant_field_name(v, i);
                let base = go_ty(api, &v.ty);
                writeln!(out, "\t{{").unwrap();
                writeln!(out, "\t\tvalue := new({base})").unwrap();
                writeln!(
                    out,
                    "\t\tif err := json.Unmarshal(data, value); err == nil {{"
                )
                .unwrap();
                writeln!(out, "\t\t\tu.{field} = value\n\t\t\treturn nil\n\t\t}}").unwrap();
                writeln!(out, "\t}}").unwrap();
            }
            imports.add("fmt");
            writeln!(out, "\treturn fmt.Errorf(\"{name}: no variant matched\")").unwrap();
        }
    }
    writeln!(out, "}}").unwrap();
}

// ---- resources ---------------------------------------------------------------

pub(crate) fn client_param<'a>(api: &'a Api, wire_name: &str) -> Option<&'a ClientParam> {
    api.client_params.iter().find(|c| c.wire_name == wire_name)
}

fn return_ty(api: &Api, op: &Operation) -> Option<String> {
    match (&op.pagination, &op.response) {
        (Some(page), _) => Some(format!("*Page[{}]", go_ty(api, &page.item_ty))),
        (None, ResponseKind::Json(ty)) => Some(match ty {
            Ty::Named(_) => format!("*{}", go_ty(api, ty)),
            _ => go_ty(api, ty),
        }),
        (None, ResponseKind::Sse(ty)) => Some(format!("*Stream[{}]", go_ty(api, ty))),
        (None, ResponseKind::Empty) => None,
    }
}

fn method_signature(api: &Api, resource: &Resource, op: &Operation) -> String {
    let mut args = vec!["ctx context.Context".to_string()];
    for p in &op.positionals {
        let ty = if integer_width(api, &p.ty).is_some() {
            go_ty(api, &p.ty)
        } else {
            "string".to_string()
        };
        args.push(format!("{} {ty}", go_local(&p.wire_name)));
    }
    if op.has_params() {
        args.push(format!("params *{}", params_type_name(resource, op)));
    }
    args.push("opts ...RequestOption".to_string());
    let ret = match return_ty(api, op) {
        Some(t) => format!("({t}, error)"),
        None => "error".to_string(),
    };
    format!("{}({}) {}", go_name(&op.name), args.join(", "), ret)
}

fn emit_resource(api: &Api, resource: &Resource, pkg: &str) -> String {
    let mut body = String::new();
    let mut imports = Imports::default();
    imports.add("context");

    // Params structs.
    for op in &resource.operations {
        if !op.has_params() {
            continue;
        }
        writeln!(body, "type {} struct {{", params_type_name(resource, op)).unwrap();
        for p in op.path_params.iter().chain(op.query_params.iter()) {
            let required = p.required && client_param(api, &p.wire_name).is_none();
            let omit = if required { "" } else { ",omitempty" };
            writeln!(
                body,
                "\t{} {} `json:\"{}{omit}\"`",
                go_name(&p.wire_name),
                field_ty(api, &p.ty, required),
                p.wire_name,
            )
            .unwrap();
        }
        for f in &op.body_fields {
            let omit = if f.required { "" } else { ",omitempty" };
            writeln!(
                body,
                "\t{} {} `json:\"{}{omit}\"`",
                go_name(&f.wire_name),
                input_field_ty(api, &api.divergent_types(), &f.ty, f.required),
                f.wire_name,
            )
            .unwrap();
        }
        if let Some(ty) = &op.whole_body {
            writeln!(body, "\t// Body is sent as the entire request body.").unwrap();
            writeln!(
                body,
                "\tBody {} `json:\"body\"`",
                input_field_ty(api, &api.divergent_types(), ty, true),
            )
            .unwrap();
        }
        writeln!(body, "}}\n").unwrap();
        emit_builder(api, resource, op, &mut body);
    }

    // Nested resources hang off this one as accessor methods.
    let children: Vec<&Resource> = api
        .resources
        .iter()
        .filter(|r| r.parent.as_deref() == Some(resource.name.as_str()))
        .collect();

    // Interface.
    let iface = interface_name(resource);
    if let Some(d) = &resource.description {
        doc_comment(&mut body, "", d);
    }
    writeln!(
        body,
        "// {iface} is implemented by the SDK and easy to mock in tests."
    )
    .unwrap();
    writeln!(body, "type {iface} interface {{").unwrap();
    for op in &resource.operations {
        if let Some(s) = op.summary.as_deref() {
            doc_comment(&mut body, "\t", s);
        }
        writeln!(body, "\t{}", method_signature(api, resource, op)).unwrap();
    }
    for child in &children {
        writeln!(
            body,
            "\t// {} returns the nested {} resource group.",
            go_name(&child.name),
            child.name
        )
        .unwrap();
        writeln!(
            body,
            "\t{}() {}",
            go_name(&child.name),
            interface_name(child)
        )
        .unwrap();
    }
    writeln!(body, "}}\n").unwrap();

    // Impl.
    let svc = service_name(resource);
    writeln!(body, "type {svc} struct {{ core *core }}\n").unwrap();
    writeln!(body, "var _ {iface} = (*{svc})(nil)\n").unwrap();
    for child in &children {
        writeln!(
            body,
            "func (s *{svc}) {}() {} {{ return &{}{{core: s.core}} }}\n",
            go_name(&child.name),
            interface_name(child),
            service_name(child),
        )
        .unwrap();
    }
    for op in &resource.operations {
        emit_method(api, resource, op, &svc, &mut body, &mut imports);
        body.push('\n');
    }

    if body.contains("time.Time") {
        imports.add("time");
    }
    format!(
        "// Code generated by redwood. DO NOT EDIT.\n\npackage {pkg}\n\n{}{}",
        imports.render(),
        body
    )
}

fn emit_method(
    api: &Api,
    resource: &Resource,
    op: &Operation,
    svc: &str,
    out: &mut String,
    imports: &mut Imports,
) {
    writeln!(
        out,
        "func (s *{svc}) {} {{",
        method_signature(api, resource, op)
    )
    .unwrap();

    if op.has_params() {
        writeln!(
            out,
            "\tif params == nil {{\n\t\tparams = &{}{{}}\n\t}}",
            params_type_name(resource, op)
        )
        .unwrap();
    }

    let fail = |api: &Api, op: &Operation| -> String {
        match return_ty(api, op) {
            Some(_) => "nil, err".to_string(),
            None => "err".to_string(),
        }
    };

    // Resolve client-level defaults for path/query params.
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        let Some(c) = client_param(api, &p.wire_name) else {
            continue;
        };
        writeln!(
            out,
            "\t{}, err := s.core.resolveDefault(\"{}\", \"{}\", params.{})",
            go_local(&p.wire_name),
            c.wire_name,
            c.env_var,
            go_name(&p.wire_name),
        )
        .unwrap();
        writeln!(
            out,
            "\tif err != nil {{\n\t\treturn {}\n\t}}",
            fail(api, op)
        )
        .unwrap();
    }

    // Path. Every placeholder is validated non-blank at the boundary — a
    // blank identifier would silently rewrite the route — then escaped.
    let mut fmt_str = String::new();
    let mut fmt_args: Vec<String> = Vec::new();
    let mut rest = op.path.as_str();
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|e| start + e)
            .expect("balanced");
        fmt_str.push_str(&rest[..start].replace('%', "%%"));
        fmt_str.push_str("%s");
        let param = &rest[start + 1..end];
        let is_client = client_param(api, param).is_some();
        let expr = if op.positionals.iter().any(|p| p.wire_name == param) || is_client {
            go_local(param)
        } else {
            format!("params.{}", go_name(param))
        };
        // Client parameters are configuration strings. Every other path
        // parameter keeps its schema type at the public method boundary and
        // is rendered only when constructing the URL.
        let expr = if is_client {
            expr
        } else {
            let path_param = op
                .positionals
                .iter()
                .chain(op.path_params.iter())
                .find(|p| p.wire_name == param)
                .expect("path placeholder has an IR parameter");
            if integer_width(api, &path_param.ty).is_some() {
                scalar_to_string(api, imports, &path_param.ty, &expr).0
            } else {
                expr
            }
        };
        let seg = format!("seg{}", go_name(param));
        writeln!(out, "\t{seg}, err := pathSegment(\"{param}\", {expr})").unwrap();
        writeln!(
            out,
            "\tif err != nil {{\n\t\treturn {}\n\t}}",
            fail(api, op)
        )
        .unwrap();
        fmt_args.push(seg);
        rest = &rest[end + 1..];
    }
    fmt_str.push_str(&rest.replace('%', "%%"));
    if fmt_args.is_empty() {
        writeln!(out, "\tpath := \"{fmt_str}\"").unwrap();
    } else {
        imports.add("fmt");
        writeln!(
            out,
            "\tpath := fmt.Sprintf(\"{fmt_str}\", {})",
            fmt_args.join(", ")
        )
        .unwrap();
    }

    // Query.
    let has_query = !op.query_params.is_empty();
    if has_query {
        imports.add("net/url");
        writeln!(out, "\tq := url.Values{{}}").unwrap();
        for p in &op.query_params {
            emit_query_param(api, out, imports, p);
        }
    }
    let q_expr = if has_query { "q" } else { "nil" };

    // Body.
    let has_body = !op.body_fields.is_empty();
    if has_body {
        writeln!(out, "\tbody := map[string]any{{}}").unwrap();
        for f in &op.body_fields {
            let field = go_name(&f.wire_name);
            let is_ptr = field_ty(api, &f.ty, f.required).starts_with('*');
            let nilable = matches!(f.ty, Ty::List(_) | Ty::Map(_) | Ty::Json | Ty::Bytes);
            if is_ptr || nilable {
                writeln!(
                    out,
                    "\tif params.{field} != nil {{\n\t\tbody[\"{}\"] = params.{field}\n\t}}",
                    f.wire_name
                )
                .unwrap();
            } else {
                writeln!(out, "\tbody[\"{}\"] = params.{field}", f.wire_name).unwrap();
            }
        }
    }
    // A whole-body param is marshaled as the entire request body.
    let body_expr = if has_body {
        "body"
    } else if op.whole_body.is_some() {
        "params.Body"
    } else {
        "nil"
    };

    match (&op.pagination, &op.response) {
        (Some(page), ResponseKind::Json(response_ty)) => {
            let response_go = go_ty(api, response_ty);
            writeln!(out, "\tvar out {response_go}").unwrap();
            writeln!(
                out,
                "\tif err := s.core.do(ctx, \"{}\", path, {q_expr}, {body_expr}, &out, opts...); err != nil {{\n\t\treturn nil, err\n\t}}",
                op.http_method.as_str()
            )
            .unwrap();
            emit_cursor_extract(api, out, response_ty, &page.next_cursor_path);
            let item_go = go_ty(api, &page.item_ty);
            let items_field = go_name(&page.items_field);
            // Refetch closure re-enters the method with the cursor set.
            let mut refetch_args = vec!["ctx".to_string()];
            for p in &op.positionals {
                refetch_args.push(go_local(&p.wire_name));
            }
            refetch_args.push("&p".to_string());
            refetch_args.push("opts...".to_string());
            // Snapshot params now: caller mutation after this call must not
            // leak into later page fetches (or race with them).
            writeln!(out, "\tbase := *params").unwrap();
            writeln!(
                out,
                "\tfetch := func(ctx context.Context, cursor string) (*Page[{item_go}], error) {{"
            )
            .unwrap();
            writeln!(out, "\t\tp := base").unwrap();
            writeln!(out, "\t\tp.{} = &cursor", go_name(&page.cursor_param)).unwrap();
            writeln!(
                out,
                "\t\treturn s.{}({})",
                go_name(&op.name),
                refetch_args.join(", ")
            )
            .unwrap();
            writeln!(out, "\t}}").unwrap();
            writeln!(
                out,
                "\treturn newPage(out.{items_field}, nextCursor, fetch), nil"
            )
            .unwrap();
        }
        (None, ResponseKind::Json(ty)) => {
            let go = go_ty(api, ty);
            writeln!(out, "\tvar out {go}").unwrap();
            writeln!(
                out,
                "\tif err := s.core.do(ctx, \"{}\", path, {q_expr}, {body_expr}, &out, opts...); err != nil {{\n\t\treturn nil, err\n\t}}",
                op.http_method.as_str()
            )
            .unwrap();
            match ty {
                Ty::Named(_) => writeln!(out, "\treturn &out, nil").unwrap(),
                _ => writeln!(out, "\treturn out, nil").unwrap(),
            }
        }
        (None, ResponseKind::Sse(ty)) => {
            // Options are applied exactly once; the stream seeds its resume
            // checkpoint from the config that actually went over the wire.
            writeln!(out, "\trc := buildRequestConfig(opts)").unwrap();
            writeln!(
                out,
                "\tresp, err := s.core.rawConfig(ctx, \"{}\", path, {q_expr}, {body_expr}, true, rc)",
                op.http_method.as_str()
            )
            .unwrap();
            writeln!(out, "\tif err != nil {{\n\t\treturn nil, err\n\t}}").unwrap();
            let skip = if api.sse_skip_events.is_empty() {
                "nil".to_string()
            } else {
                format!(
                    "[]string{{{}}}",
                    api.sse_skip_events
                        .iter()
                        .map(|e| format!("\"{e}\""))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            imports.add("net/http");
            // Auto-reconnect closure: re-issues the SAME request with the
            // resume checkpoint; WithReconnect(false) disables.
            writeln!(
                out,
                "\t_reconnect := func(lastEventID string) (*http.Response, error) {{\n\t\trc2 := *rc\n\t\trc2.headers = rc.headers.Clone()\n\t\tif lastEventID != \"\" {{\n\t\t\trc2.headers.Set(\"Last-Event-ID\", lastEventID)\n\t\t}} else {{\n\t\t\trc2.headers.Del(\"Last-Event-ID\")\n\t\t}}\n\t\treturn s.core.rawConfig(ctx, \"{method}\", path, {q_expr}, {body_expr}, true, &rc2)\n\t}}\n\tif rc.reconnect != nil && !*rc.reconnect {{\n\t\t_reconnect = nil\n\t}}",
                method = op.http_method.as_str()
            )
            .unwrap();
            writeln!(
                out,
                "\treturn newStream[{}](ctx, resp, rc.headers.Get(\"Last-Event-ID\"), {skip}, _reconnect), nil",
                go_ty(api, ty)
            )
            .unwrap();
        }
        (None, ResponseKind::Empty) => {
            writeln!(
                out,
                "\treturn s.core.do(ctx, \"{}\", path, {q_expr}, {body_expr}, nil, opts...)",
                op.http_method.as_str()
            )
            .unwrap();
        }
        (Some(_), _) => unreachable!("pagination implies JSON"),
    }
    writeln!(out, "}}").unwrap();
}

/// Serialize one query param into `q`, handling optionality and element type.
fn emit_query_param(api: &Api, out: &mut String, imports: &mut Imports, p: &Param) {
    let field = go_name(&p.wire_name);
    let required = p.required && client_param(api, &p.wire_name).is_none();
    let is_client = client_param(api, &p.wire_name).is_some();
    // Client params were already resolved into a local string.
    if is_client {
        writeln!(
            out,
            "\tq.Set(\"{}\", {})",
            p.wire_name,
            go_local(&p.wire_name)
        )
        .unwrap();
        return;
    }
    match &p.ty {
        Ty::List(inner) => {
            writeln!(out, "\tfor _, item := range params.{field} {{").unwrap();
            let (expr, _) = scalar_to_string(api, imports, inner, "item");
            writeln!(out, "\t\tq.Add(\"{}\", {expr})", p.wire_name).unwrap();
            writeln!(out, "\t}}").unwrap();
        }
        _ if required => {
            let access = format!("params.{field}");
            let (expr, _) = scalar_to_string(api, imports, &p.ty, &access);
            writeln!(out, "\tq.Set(\"{}\", {expr})", p.wire_name).unwrap();
        }
        _ => {
            writeln!(out, "\tif params.{field} != nil {{").unwrap();
            let access = format!("*params.{field}");
            let (expr, _) = scalar_to_string(api, imports, &p.ty, &access);
            writeln!(out, "\t\tq.Set(\"{}\", {expr})", p.wire_name).unwrap();
            writeln!(out, "\t}}").unwrap();
        }
    }
}

/// Expression converting a scalar-ish access to string for query encoding.
fn scalar_to_string(api: &Api, imports: &mut Imports, ty: &Ty, access: &str) -> (String, String) {
    match ty {
        Ty::String | Ty::Literal(_) => (format!("({access})"), "".into()),
        Ty::Bool => {
            imports.add("strconv");
            (format!("strconv.FormatBool({access})"), "".into())
        }
        Ty::Int32 => {
            imports.add("strconv");
            (format!("strconv.FormatInt(int64({access}), 10)"), "".into())
        }
        Ty::Int64 => {
            imports.add("strconv");
            (format!("strconv.FormatInt({access}, 10)"), "".into())
        }
        Ty::Float | Ty::Double => {
            imports.add("strconv");
            (
                format!("strconv.FormatFloat(float64({access}), 'f', -1, 64)"),
                "".into(),
            )
        }
        Ty::Timestamp => {
            imports.add("time");
            // Nano keeps caller-supplied fractional seconds (formats without
            // a fractional part when it is zero).
            (format!("({access}).Format(time.RFC3339Nano)"), "".into())
        }
        Ty::Named(n) => match resolved_shape(api, n) {
            Some(Shape::Enum(_)) => (format!("string({access})"), "".into()),
            _ => {
                imports.add("fmt");
                (format!("fmt.Sprint({access})"), "".into())
            }
        },
        _ => {
            imports.add("fmt");
            (format!("fmt.Sprint({access})"), "".into())
        }
    }
}

/// Emit `nextCursor := ...` walking an optional chain like
/// "pagination.nextCursor" with nil guards derived from field optionality.
fn emit_cursor_extract(api: &Api, out: &mut String, response_ty: &Ty, path: &str) {
    writeln!(out, "\tnextCursor := \"\"").unwrap();
    let mut segments = path.split('.').peekable();
    let mut current_ty = response_ty.clone();
    let mut access = "out".to_string();
    let mut guards: Vec<String> = Vec::new();
    while let Some(segment) = segments.next() {
        let Ty::Named(type_name) = &current_ty else {
            break;
        };
        let Some(Shape::Struct(s)) = resolved_shape(api, type_name) else {
            break;
        };
        let Some(field) = s.fields.iter().find(|f| f.wire_name == segment) else {
            break;
        };
        let is_ptr = field_ty(api, &field.ty, field.required).starts_with('*');
        access = format!("{access}.{}", go_name(segment));
        if is_ptr {
            guards.push(format!("{access} != nil"));
        }
        if segments.peek().is_none() {
            let deref = if is_ptr { "*" } else { "" };
            if guards.is_empty() {
                writeln!(out, "\tnextCursor = {deref}{access}").unwrap();
            } else {
                writeln!(out, "\tif {} {{", guards.join(" && ")).unwrap();
                writeln!(out, "\t\tnextCursor = {deref}{access}").unwrap();
                writeln!(out, "\t}}").unwrap();
            }
        }
        current_ty = field.ty.clone();
    }
}

// ---- client.go -----------------------------------------------------------------

fn emit_client(api: &Api, pkg: &str, config: &crate::config::GoConfig) -> String {
    let mut out = String::new();
    writeln!(out, "// Code generated by redwood. DO NOT EDIT.\n").unwrap();
    writeln!(out, "package {pkg}\n").unwrap();
    writeln!(out, "import (").unwrap();
    if !api.webhooks.is_empty() {
        writeln!(out, "\t\"encoding/json\"").unwrap();
    }
    writeln!(out, "\t\"errors\"").unwrap();
    writeln!(out, "\t\"fmt\"").unwrap();
    writeln!(out, "\t\"io\"").unwrap();
    writeln!(out, "\t\"net/http\"").unwrap();
    writeln!(out, "\t\"net/url\"").unwrap();
    writeln!(out, "\t\"os\"").unwrap();
    writeln!(out, "\t\"strings\"").unwrap();
    writeln!(out, ")\n").unwrap();

    let auth = match &api.auth {
        Auth::Bearer => {
            "func() (string, string) { return \"Authorization\", \"Bearer \" + apiKey }".to_string()
        }
        Auth::ApiKeyHeader(h) => format!("func() (string, string) {{ return \"{h}\", apiKey }}"),
        Auth::None => "func() (string, string) { return \"\", \"\" }".to_string(),
    };

    write!(
        out,
        r#"// Client is the {name} API client. Construct with NewClient; every
// resource group is an accessor method returning a mockable interface.
type Client struct {{
	core       *core
{wh_client_field}}}

// Pointer fields separate "omitted" (nil, may read the environment) from
// "explicitly supplied" (validated: a blank explicit value is an error and
// never falls back to ambient credentials).
type clientOptions struct {{
	apiKey        *string
	baseURL       *string
{wh_opt_field}	httpClient    *http.Client
	debugLog      io.Writer
	maxRetries    int
	defaults      map[string]*string
}}

// Option configures the client.
type Option func(*clientOptions)

{with_api_key}// WithBaseURL overrides the API base URL.
func WithBaseURL(u string) Option {{ return func(o *clientOptions) {{ o.baseURL = &u }} }}

{with_webhook_secret}// WithHTTPClient supplies a custom *http.Client.
func WithHTTPClient(c *http.Client) Option {{ return func(o *clientOptions) {{ o.httpClient = c }} }}

// WithMaxRetries sets automatic retries for retryable failures (default {retries}).
func WithMaxRetries(n int) Option {{ return func(o *clientOptions) {{ o.maxRetries = n }} }}

// WithDebugLog dumps every HTTP exchange to w (request line, headers,
// bodies) for troubleshooting. The credential header is redacted; SSE
// response bodies are not consumed. w is typically os.Stderr.
func WithDebugLog(w io.Writer) Option {{ return func(o *clientOptions) {{ o.debugLog = w }} }}
"#,
        name = api.name,
        wh_client_field = if api.webhooks.is_empty() {
            String::new()
        } else {
            "\twebhookSecret string\n".to_string()
        },
        wh_opt_field = if api.webhooks.is_empty() {
            String::new()
        } else {
            "\twebhookSecret *string\n".to_string()
        },
        with_webhook_secret = if api.webhooks.is_empty() {
            String::new()
        } else {
            format!(
                "// WithWebhookSecret sets the Standard Webhooks signing secret (omit to read the {wh} env var).\nfunc WithWebhookSecret(secret string) Option {{ return func(o *clientOptions) {{ o.webhookSecret = &secret }} }}\n\n",
                wh = api.webhook_env
            )
        },
        with_api_key = if matches!(api.auth, Auth::None) {
            String::new()
        } else {
            format!(
                "// WithAPIKey sets the API key (omit to read the {env} env var).\nfunc WithAPIKey(key string) Option {{ return func(o *clientOptions) {{ o.apiKey = &key }} }}\n\n",
                env = api.api_key_env
            )
        },
        retries = api.max_retries,
    )
    .unwrap();

    for c in &api.client_params {
        write!(
            out,
            r#"
// With{pascal} sets the default {wire} for every call that takes one
// (default: the {env} env var).
func With{pascal}(v string) Option {{
	return func(o *clientOptions) {{ o.defaults["{wire}"] = &v }}
}}
"#,
            pascal = go_name(&c.wire_name),
            wire = c.wire_name,
            env = c.env_var,
        )
        .unwrap();
    }

    write!(
        out,
        r#"
// resolveOption separates presence from validity: nil reads the (trimmed)
// environment; an explicitly supplied value must be non-blank.
func resolveOption(name string, explicit *string, envVar string) (string, error) {{
	if explicit != nil {{
		v := strings.TrimSpace(*explicit)
		if v == "" {{
			return "", fmt.Errorf("cadenya: %s must not be blank", name)
		}}
		return v, nil
	}}
	return strings.TrimSpace(os.Getenv(envVar)), nil
}}

// NewClient builds a Client, reading unset options from the environment.
func NewClient(opts ...Option) (*Client, error) {{
	o := &clientOptions{{maxRetries: {retries}, defaults: map[string]*string{{}}}}
	for _, opt := range opts {{
		opt(o)
	}}
{api_key_resolve}	baseURL, err := resolveOption("base URL", o.baseURL, "{base_env}")
	if err != nil {{
		return nil, err
	}}
	if baseURL == "" {{
		baseURL = "{base_url}"
	}}
	// Validate the STRUCTURE once here: operation paths are appended to
	// this value, so a query/fragment/userinfo would silently swallow the
	// request path (".../prefix?tenant=x" + "/v1/..." becomes query text).
	// An absolute http(s) URL with a host is required; a path prefix is
	// supported and kept.
	parsed, err := url.Parse(baseURL)
	if err != nil {{
		return nil, fmt.Errorf("cadenya: invalid base URL %q: %w", baseURL, err)
	}}
	if parsed.Scheme != "http" && parsed.Scheme != "https" {{
		return nil, fmt.Errorf("cadenya: base URL %q must be absolute http or https", baseURL)
	}}
	// Hostname(), not Host: "https://:443" has a nonempty Host but no
	// hostname and would defer failure to transport time.
	if parsed.Hostname() == "" {{
		return nil, fmt.Errorf("cadenya: base URL %q has no host", baseURL)
	}}
	// ForceQuery covers a bare trailing question mark; the strings check
	// also catches a bare fragment marker (net/url has no ForceFragment) —
	// either literal delimiter would absorb the appended request path.
	if parsed.User != nil || parsed.RawQuery != "" || parsed.ForceQuery || parsed.Fragment != "" || strings.ContainsAny(baseURL, "?#") {{
		return nil, fmt.Errorf("cadenya: base URL %q must not carry userinfo, query, or fragment", baseURL)
	}}
	baseURL = strings.TrimRight(parsed.String(), "/")
{wh_resolve}	defaults := map[string]string{{}}
	if o.httpClient == nil {{
		// No whole-request Timeout: it would cap the lifetime of streaming
		// response bodies. Ordinary JSON calls get a 60s context deadline in
		// the core instead; streams rely on caller contexts.
		o.httpClient = &http.Client{{}}
	}}
	if o.debugLog != nil {{
		// Clone: never mutate a caller-supplied http.Client.
		clone := *o.httpClient
		clone.Transport = &debugTransport{{base: o.httpClient.Transport, w: o.debugLog}}
		o.httpClient = &clone
	}}
"#,
        retries = api.max_retries,
        api_key_resolve = if matches!(api.auth, Auth::None) {
            // No security scheme: construct unauthenticated, demand nothing.
            String::new()
        } else {
            format!(
                "\tapiKey, err := resolveOption(\"api key\", o.apiKey, \"{env}\")\n\tif err != nil {{\n\t\treturn nil, err\n\t}}\n\tif apiKey == \"\" {{\n\t\treturn nil, errors.New(\"cadenya: missing API key: pass WithAPIKey or set {env}\")\n\t}}\n",
                env = api.api_key_env
            )
        },
        wh_resolve = if api.webhooks.is_empty() {
            String::new()
        } else {
            format!(
                "\twebhookSecret, err := resolveOption(\"webhook secret\", o.webhookSecret, \"{wh}\")\n\tif err != nil {{\n\t\treturn nil, err\n\t}}\n",
                wh = api.webhook_env
            )
        },
        base_env = format!("{}_BASE_URL", api.name.to_uppercase()),
        base_url = api.base_url,
    )
    .unwrap();
    for c in &api.client_params {
        writeln!(
            out,
            "\t{{\n\t\tv, err := resolveOption(\"{wire}\", o.defaults[\"{wire}\"], \"{env}\")\n\t\tif err != nil {{\n\t\t\treturn nil, err\n\t\t}}\n\t\tdefaults[\"{wire}\"] = v\n\t}}",
            wire = c.wire_name,
            env = c.env_var,
        )
        .unwrap();
    }
    write!(
        out,
        r#"	authHeader := {auth}
	return &Client{{
		core: &core{{
			baseURL:    baseURL,
			authHeader: authHeader,
			httpClient: o.httpClient,
			maxRetries: o.maxRetries,
			defaults:   defaults,
			userAgent:  "{ua}",
		}},
{wh_init}	}}, nil
}}
"#,
        wh_init = if api.webhooks.is_empty() {
            String::new()
        } else {
            "\t\twebhookSecret: webhookSecret,\n".to_string()
        },
        ua = format!(
            "{}-go/{} (api {})",
            api.name.to_lowercase(),
            config.sdk_version.as_deref().unwrap_or("0.1.0"),
            api.version
        ),
    )
    .unwrap();

    for resource in api.resources.iter().filter(|r| r.parent.is_none()) {
        write!(
            out,
            r#"
// {pascal} returns the {name} resource group.
func (c *Client) {pascal}() {iface} {{ return &{svc}{{core: c.core}} }}
"#,
            pascal = go_name(&resource.name),
            name = resource.name,
            iface = interface_name(resource),
            svc = service_name(resource),
        )
        .unwrap();
    }

    if !api.webhooks.is_empty() {
        let payload = webhook_payload_ty(api);
        write!(
            out,
            r#"
// WebhookResources verifies and decodes Standard Webhooks deliveries.
type WebhookResources interface {{
	// Unwrap verifies the delivery signature and returns the typed payload.
	Unwrap(payload []byte, headers http.Header) (*{payload}, error)
}}

type webhooksService struct{{ secret string }}

var _ WebhookResources = (*webhooksService)(nil)

// Webhooks returns the webhook verification group.
func (c *Client) Webhooks() WebhookResources {{ return &webhooksService{{secret: c.webhookSecret}} }}

func (s *webhooksService) Unwrap(payload []byte, headers http.Header) (*{payload}, error) {{
	if s.secret == "" {{
		return nil, errors.New("cadenya: missing webhook secret: pass WithWebhookSecret or set {wh_env}")
	}}
	if err := verifyWebhook(s.secret, payload, headers); err != nil {{
		return nil, err
	}}
	var event {payload}
	if err := json.Unmarshal(payload, &event); err != nil {{
		return nil, err
	}}
	return &event, nil
}}

// VerifyWebhook verifies a Standard Webhooks delivery signature without
// constructing a Client — webhook-only consumers need no API credential.
func VerifyWebhook(secret string, payload []byte, headers http.Header) error {{
	if secret == "" {{
		return errors.New("cadenya: missing webhook secret")
	}}
	return verifyWebhook(secret, payload, headers)
}}

// UnwrapWebhook verifies a delivery and returns the typed payload without
// constructing a Client.
func UnwrapWebhook(secret string, payload []byte, headers http.Header) (*{payload}, error) {{
	if err := VerifyWebhook(secret, payload, headers); err != nil {{
		return nil, err
	}}
	var event {payload}
	if err := json.Unmarshal(payload, &event); err != nil {{
		return nil, err
	}}
	return &event, nil
}}
"#,
            wh_env = api.webhook_env,
        )
        .unwrap();
    }
    out
}

fn webhook_payload_ty(api: &Api) -> String {
    let mut names: BTreeSet<String> = BTreeSet::new();
    for w in &api.webhooks {
        names.insert(go_ty(api, &w.payload));
    }
    if names.len() == 1 {
        names.into_iter().next().unwrap()
    } else {
        // Heterogeneous payloads: fall back to a raw message the caller
        // switches on.
        "json.RawMessage".to_string()
    }
}

// ---- conformance driver --------------------------------------------------------

fn emit_conformance(api: &Api, module: &str, pkg: &str) -> String {
    let mut out = String::new();
    write!(
        out,
        r#"// Code generated by redwood. DO NOT EDIT.
// Conformance driver: calls every operation against the mock at MOCK_URL.
package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"

	sdk "{module}"
)

var failures int

func run(id string, fn func() error) {{
	if err := fn(); err != nil {{
		failures++
		fmt.Printf("FAIL %s: %v\n", id, err)
		return
	}}
	fmt.Printf("PASS %s\n", id)
}}

func decode[T any](raw string) *T {{
	var v T
	if err := json.Unmarshal([]byte(raw), &v); err != nil {{
		panic(fmt.Sprintf("bad sample: %v", err))
	}}
	return &v
}}

func main() {{
	client, err := sdk.NewClient(
{conf_auth}		sdk.WithBaseURL(os.Getenv("MOCK_URL")),
	)
	if err != nil {{
		fmt.Println("client:", err)
		os.Exit(1)
	}}
	ctx := context.Background()
	_ = ctx
"#,
        conf_auth = if matches!(api.auth, Auth::None) {
            String::new()
        } else {
            "\t\tsdk.WithAPIKey(\"conformance-key\"),\n".to_string()
        },
    )
    .unwrap();
    let _ = pkg;

    for resource in &api.resources {
        for op in &resource.operations {
            emit_conformance_call(api, resource, op, &mut out);
        }
    }
    writeln!(out, "\tos.Exit(min(failures, 1))\n}}").unwrap();
    out
}

fn emit_conformance_call(api: &Api, resource: &Resource, op: &Operation, out: &mut String) {
    let mut call_args = vec!["ctx".to_string()];
    for p in &op.positionals {
        call_args.push(positional_sample(api, &p.ty));
    }
    if op.has_params() {
        // Build the params sample as JSON, decoded via the struct's tags.
        let sample = params_sample_json(api, op);
        call_args.push(format!(
            "decode[sdk.{}](`{}`)",
            params_type_name(resource, op),
            sample
        ));
    }
    let accessor = match &resource.parent {
        Some(parent) => format!("client.{}().{}()", go_name(parent), go_name(&resource.name)),
        None => format!("client.{}()", go_name(&resource.name)),
    };
    let invoke = format!("{accessor}.{}({})", go_name(&op.name), call_args.join(", "));
    writeln!(out, "\trun(\"{}\", func() error {{", op.id).unwrap();
    match (&op.pagination, &op.response) {
        (Some(_), _) => {
            writeln!(out, "\t\tpage, err := {invoke}").unwrap();
            writeln!(out, "\t\tif err != nil {{\n\t\t\treturn err\n\t\t}}").unwrap();
            // The mock serves two pages and rejects the second when any
            // non-cursor query param drifted from the first request.
            writeln!(out, "\t\titems, err := page.All(ctx)").unwrap();
            writeln!(out, "\t\tif err != nil {{\n\t\t\treturn err\n\t\t}}").unwrap();
            writeln!(out, "\t\tif len(items) != 2 {{\n\t\t\treturn fmt.Errorf(\"expected 2 items across pages, got %d\", len(items))\n\t\t}}").unwrap();
            writeln!(out, "\t\treturn nil").unwrap();
        }
        (None, ResponseKind::Json(_)) => {
            writeln!(out, "\t\t_, err := {invoke}").unwrap();
            writeln!(out, "\t\treturn err").unwrap();
        }
        (None, ResponseKind::Sse(_)) => {
            writeln!(out, "\t\tstream, err := {invoke}").unwrap();
            writeln!(out, "\t\tif err != nil {{\n\t\t\treturn err\n\t\t}}").unwrap();
            writeln!(out, "\t\tcount := 0").unwrap();
            writeln!(out, "\t\tfor stream.Next() {{\n\t\t\tcount++\n\t\t}}").unwrap();
            writeln!(
                out,
                "\t\tif err := stream.Err(); err != nil {{\n\t\t\treturn err\n\t\t}}"
            )
            .unwrap();
            writeln!(out, "\t\tif count != 2 {{\n\t\t\treturn fmt.Errorf(\"expected 2 events, got %d\", count)\n\t\t}}").unwrap();
            writeln!(out, "\t\treturn nil").unwrap();
        }
        (None, ResponseKind::Empty) => {
            writeln!(out, "\t\treturn {invoke}").unwrap();
        }
    }
    writeln!(out, "\t}})").unwrap();
}

/// The same sample synthesis the manifest uses, rendered as a JSON object for
/// the op's params struct.
fn params_sample_json(api: &Api, op: &Operation) -> String {
    let mut map = serde_json::Map::new();
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        map.insert(p.wire_name.clone(), super::manifest_sample(api, &p.ty));
    }
    for f in &op.body_fields {
        map.insert(f.wire_name.clone(), super::manifest_sample(api, &f.ty));
    }
    if let Some(ty) = &op.whole_body {
        map.insert("body".to_string(), super::manifest_sample(api, ty));
    }
    serde_json::Value::Object(map).to_string().replace('`', "'")
}
