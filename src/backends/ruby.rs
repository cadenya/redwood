//! Ruby backend: emits a Faraday-based SDK in the house style of the major
//! Ruby API clients — a `Client` with resource readers, keyword arguments per
//! method, typed model classes with generated decoders, auto-iterating pages,
//! and SSE streams.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use heck::{ToSnakeCase, ToUpperCamelCase};

use crate::backends::{Backend, FileSet};
use crate::config::RubyConfig;
use crate::ir::*;

const RT_CORE: &str = include_str!("../../runtime/ruby/core.rb");
const RT_PAGINATION: &str = include_str!("../../runtime/ruby/pagination.rb");
const RT_SSE: &str = include_str!("../../runtime/ruby/sse.rb");
const RT_WEBHOOKS: &str = include_str!("../../runtime/ruby/webhooks.rb");

pub struct RubyBackend {
    pub config: RubyConfig,
}

impl Backend for RubyBackend {
    fn name(&self) -> &'static str {
        "ruby"
    }

    fn generate(&self, api: &Api) -> anyhow::Result<FileSet> {
        let gem = self.gem_name(api);
        let module = module_name(api);
        let mut files = FileSet::new();
        let vendored = |body: &str| body.replace("RedwoodModule", &module);
        files.insert(format!("lib/{gem}/core.rb"), vendored(RT_CORE));
        files.insert(format!("lib/{gem}/pagination.rb"), vendored(RT_PAGINATION));
        files.insert(format!("lib/{gem}/sse.rb"), vendored(RT_SSE));
        if !api.webhooks.is_empty() {
            files.insert(format!("lib/{gem}/webhooks.rb"), vendored(RT_WEBHOOKS));
        }
        files.insert(format!("lib/{gem}/types.rb"), emit_types(api, &module));
        for resource in &api.resources {
            files.insert(
                format!("lib/{gem}/resources/{}.rb", resource.ident),
                emit_resource(api, resource, &module),
            );
        }
        files.insert(
            format!("lib/{gem}/client.rb"),
            emit_client(api, &module, &self.config),
        );
        files.insert(format!("lib/{gem}.rb"), emit_entry(api, &gem));
        files.insert("Gemfile".into(), emit_gemfile());
        files.insert(
            format!("{gem}.gemspec"),
            emit_gemspec(api, &gem, &self.config),
        );
        files.insert(
            "conformance.rb".into(),
            emit_conformance(api, &gem, &module),
        );
        files.insert("api.md".into(), emit_api_md(api));
        files.insert("README.md".into(), emit_readme(api, &gem, &module));
        super::ruby_rspec::emit(api, &gem, &mut files);
        Ok(files)
    }
}

// ---- docs --------------------------------------------------------------------

/// Native reference: dotted accessor + real keyword signature per operation.
fn emit_api_md(api: &Api) -> String {
    let mut out = format!(
        "# {} Ruby SDK reference\n\nKeyword arguments are snake_case (nested request hashes too, symbols or strings); see README.md for usage patterns.\n",
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
                args.push(rb_name(&p.wire_name));
            }
            // Same required-first declaration order as the generated methods.
            let mut entries: Vec<(String, bool)> = Vec::new();
            for p in op.path_params.iter().chain(op.query_params.iter()) {
                let required = p.required && client_param(api, &p.wire_name).is_none();
                let decl = if required {
                    format!("{}:", rb_name(&p.wire_name))
                } else {
                    format!("{}: nil", rb_name(&p.wire_name))
                };
                entries.push((decl, required));
            }
            for f in &op.body_fields {
                let decl = if f.required {
                    format!("{}:", rb_name(&f.wire_name))
                } else {
                    format!("{}: nil", rb_name(&f.wire_name))
                };
                entries.push((decl, f.required));
            }
            if op.whole_body.is_some() {
                entries.push(("body:".to_string(), true));
            }
            entries.sort_by_key(|(_, required)| !required);
            args.extend(entries.into_iter().map(|(decl, _)| decl));
            let is_sse = matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_)));
            if is_sse {
                args.push("last_event_id: nil".to_string());
            }
            // Fully qualified constants: bare `Types::X` does not resolve
            // after `require`, so the reference must say what the user types.
            let module = api.name.to_upper_camel_case();
            let return_label = match (&op.pagination, &op.response) {
                (Some(page), _) => format!(
                    "{module}::Page of {module}::Types::{}",
                    rb_type_name(match &page.item_ty {
                        Ty::Named(n) => n,
                        _ => "item",
                    })
                ),
                (None, ResponseKind::Json(Ty::Named(n))) => {
                    format!("{module}::Types::{}", rb_type_name(n))
                }
                (None, ResponseKind::Json(_)) => "value".to_string(),
                (None, ResponseKind::Sse(_)) => format!("{module}::Stream"),
                (None, ResponseKind::Empty) => "nil".to_string(),
            };
            writeln!(
                out,
                "```ruby\nclient.{}.{}({}) # => {return_label}\n```",
                resource.path(),
                rb_name(&op.name),
                args.join(", ")
            )
            .unwrap();
        }
    }
    out
}

fn emit_readme(api: &Api, gem: &str, module: &str) -> String {
    let name = &api.name;
    let ws_env_note = api
        .client_params
        .first()
        .map(|c| format!(" (and {})", c.env_var))
        .unwrap_or_default();
    // Doc anchors and the nested-request example are derived STRUCTURALLY
    // from the IR and the conformance sampler — backends know nothing about
    // any particular spec, and sampled enum values are real members.
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
        .map(|(r, o)| format!("result = client.{}.{}", r.path(), rb_name(&o.name)))
        .unwrap_or_else(|| "# See api.md for every method signature.".to_string());
    let pagination_section = ops()
        .find(|(_, o)| {
            o.pagination.is_some()
                && o.positionals.is_empty()
                && o.query_params.iter().all(|q| !q.required)
        })
        .map(|(r, o)| {
            format!(
                "\n## Pagination\n\n```ruby\nclient.{}.{}.each do |item|\n  # iterating a page auto-fetches every page (also: auto_paging_each)\nend\n```\n",
                r.path(),
                rb_name(&o.name)
            )
        })
        .unwrap_or_default();
    let streaming_section = ops()
        .find(|(_, o)| matches!(o.response, ResponseKind::Sse(_)) && o.pagination.is_none())
        .map(|(r, o)| {
            let pos = o
                .positionals
                .iter()
                .map(|p| rb_name(&p.wire_name))
                .collect::<Vec<_>>()
                .join(", ");
            let acc = format!("client.{}.{}", r.path(), rb_name(&o.name));
            let comma = if pos.is_empty() { "" } else { ", " };
            format!(
                "\n## Streaming (SSE)\n\n```ruby\nstream = {acc}({pos})\nstream.each {{ |event| ... }}\n\n# Stop deterministically — from the consuming block or another thread.\n# Iteration ends cleanly at the next received chunk (keep-alives included)\n# and the HTTP response/socket is released. Streams are single-use:\n# re-enumerating raises IOError.\nstream.close\n\n# Resume after a disconnect: the checkpoint persists per the SSE spec.\nresumed = {acc}({pos}{comma}last_event_id: stream.last_event_id)\nresumed.each {{ |event| ... }}\n```\n\n`close` from another thread interrupts even a read blocked on a silent\nsocket: the stream's cancel handle tears the transport down immediately.\n"
            )
        })
        .unwrap_or_default();
    let webhooks_section = if api.webhooks.is_empty() {
        String::new()
    } else {
        format!(
            "\n## Webhooks (no API key required)\n\n```ruby\n{module}::Webhooks.verify(ENV.fetch(\"{wh}\"), payload, headers)\n# or, with typed decoding via a client: client.unwrap_webhook(payload, headers)\n```\n\nVerification follows Standard Webhooks (24–64 byte decoded secrets,\ninteger timestamps, bounded tolerance).\n",
            wh = api.webhook_env
        )
    };
    let env_comment = if matches!(api.auth, Auth::None) {
        "# This API requires no authentication.".to_string()
    } else {
        format!(
            "# Reads {api_env}{ws_env_note} from the environment when\n# arguments are omitted. Explicit blank values raise ArgumentError and\n# never fall back to the environment.",
            api_env = api.api_key_env
        )
    };
    let create_example = crate::backends::doc_example_op(api)
        .map(|(resource, op)| {
            let mut lines = vec![format!("client.{}.{}(", resource.path(), rb_name(&op.name))];
            let required: Vec<_> = op.body_fields.iter().filter(|f| f.required).collect();
            for (i, f) in required.iter().enumerate() {
                let sample = crate::backends::trim_doc_sample(
                    &crate::backends::snake_sample(
                        api,
                        &f.ty,
                        crate::backends::manifest_sample(api, &f.ty),
                    ),
                    2,
                );
                let comma = if i + 1 == required.len() { "" } else { "," };
                lines.push(format!(
                    "  {}: {}{comma}",
                    rb_name(&f.wire_name),
                    rb_literal(&sample)
                ));
            }
            lines.push(")".to_string());
            lines.join("\n")
        })
        .unwrap_or_else(|| "# (no request-bearing operation in this API)".to_string());
    format!(
        r#"# {name} Ruby SDK

The official Ruby client for the {name} API. Generated by redwood.
Built on Faraday with typed model classes and Enumerable pages.

## Install

```sh
gem install {gem}
```

## Getting started

```ruby
require "{gem}"

{env_comment}
client = {module}::Client.new

{getting_started_call}
```

## Requests are snake_case throughout

Nested request hashes take snake_case keys (symbols or strings) and are
translated to wire names at the HTTP boundary; your own data inside
JSON/map fields is never rewritten.

```ruby
{create_example}
```

## Errors

Non-2xx responses raise `{module}::APIError` (google.rpc Status:
`status_code`, `code`, `details`). Protocol surprises raise
`APIResponseError`; connection failures raise `APIConnectionError`.

## Retries

Automatic retries apply only to idempotent methods (GET/HEAD/PUT/DELETE)
and default to 0; counts normalize to a bounded 0–10 integer. Streaming
requests never auto-retry, and a caller-supplied Faraday connection's
timeout policy is authoritative.

{pagination_section}{streaming_section}{webhooks_section}
## Reference

The full method reference ships in the gem as `api.md` (also in the
repository).
"#,
    )
}

impl RubyBackend {
    fn gem_name(&self, api: &Api) -> String {
        self.config
            .gem_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase())
    }
}

pub(crate) fn module_name(api: &Api) -> String {
    api.name.to_upper_camel_case()
}

// ---- naming ----------------------------------------------------------------

const RUBY_KEYWORDS: &[&str] = &[
    "alias", "and", "begin", "break", "case", "class", "def", "defined?", "do", "else", "elsif",
    "end", "ensure", "false", "for", "if", "in", "module", "next", "nil", "not", "or", "redo",
    "rescue", "retry", "return", "self", "super", "then", "true", "undef", "unless", "until",
    "when", "while", "yield",
];

/// snake_case identifier for params/fields/methods, keyword-safe.
pub(crate) fn rb_name(wire: &str) -> String {
    let snake = wire.to_snake_case();
    if RUBY_KEYWORDS.contains(&snake.as_str()) {
        format!("{snake}_")
    } else {
        snake
    }
}

/// Ruby constants must start with an uppercase letter; type names otherwise
/// keep their spec identity.
pub(crate) fn rb_type_name(name: &str) -> String {
    if name.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        name.to_string()
    } else {
        name.to_upper_camel_case()
    }
}

fn class_name(resource: &Resource) -> String {
    resource.ident.to_upper_camel_case()
}

// ---- decoding ----------------------------------------------------------------

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

/// Expression decoding `value` (raw parsed JSON) into the typed model, or
/// `value` itself when decoding is the identity. Emitted code always runs in
/// a scope where the `Types` constant resolves.
fn decode_expr(api: &Api, ty: &Ty, value: &str) -> String {
    match ty {
        Ty::Timestamp => format!("Time.iso8601({value})"),
        Ty::Named(n) => match resolved_shape(api, n) {
            Some(Shape::Struct(s)) => {
                if s.fields.is_empty() {
                    match &s.additional {
                        Some(inner) => {
                            let inner_expr = decode_expr(api, inner, "v");
                            if inner_expr == "v" {
                                value.to_string()
                            } else {
                                format!("({value}).transform_values {{ |v| {inner_expr} }}")
                            }
                        }
                        None => format!("Types::{}.from_json({value})", rb_type_name(n)),
                    }
                } else {
                    format!("Types::{}.from_json({value})", rb_type_name(n))
                }
            }
            Some(Shape::Union(_)) => format!("Types.decode_{}({value})", rb_type_name(n)),
            Some(Shape::Enum(_)) | None => value.to_string(),
            Some(Shape::Alias(inner)) => {
                let inner = inner.clone();
                decode_expr(api, &inner, value)
            }
        },
        Ty::List(inner) => {
            let item = decode_expr(api, inner, "item");
            if item == "item" {
                value.to_string()
            } else {
                format!("({value}).map {{ |item| {item} }}")
            }
        }
        Ty::Map(inner) => {
            let item = decode_expr(api, inner, "v");
            if item == "v" {
                value.to_string()
            } else {
                format!("({value}).transform_values {{ |v| {item} }}")
            }
        }
        _ => value.to_string(),
    }
}

fn guarded_decode(api: &Api, ty: &Ty, value: &str) -> String {
    let expr = decode_expr(api, ty, value);
    if expr == value {
        value.to_string()
    } else {
        format!("{value}.nil? ? nil : {expr}")
    }
}

// ---- types.rb ---------------------------------------------------------------

fn emit_types(api: &Api, module: &str) -> String {
    let mut out = format!(
        "# frozen_string_literal: true\n\n# Code generated by redwood. DO NOT EDIT.\n\nrequire \"time\"\n\nmodule {module}\n  module Types\n"
    );
    for decl in api.types.values() {
        emit_type_decl(api, &mut out, decl);
    }
    emit_request_encoders(api, &mut out);
    out.push_str("  end\nend\n");
    out
}

// ---- request encoders ---------------------------------------------------------

/// Ruby expression evaluating to a callable encoding a request value of this
/// type (snake_case symbol/string keys -> wire names, recursively), or None
/// when no translation is needed. Lambdas defer method lookup so tables can
/// reference encoders defined later.
fn rb_value_encoder(api: &Api, ty: &Ty, ns: &str) -> Option<String> {
    match ty {
        Ty::Named(n) => match resolved_shape(api, n) {
            Some(Shape::Struct(s)) => {
                if s.fields.is_empty() {
                    match &s.additional {
                        Some(inner) => {
                            let inner = inner.clone();
                            rb_value_encoder(api, &inner, ns).map(|e| {
                                format!(
                                    "->(_v) {{ _v.is_a?(Hash) ? _v.to_h {{ |_k, _i| [_k, ({e}).call(_i)] }} : _v }}"
                                )
                            })
                        }
                        None => None,
                    }
                } else {
                    Some(format!("->(_v) {{ {ns}encode_{}(_v) }}", rb_type_name(n)))
                }
            }
            Some(Shape::Union(_)) => {
                Some(format!("->(_v) {{ {ns}encode_{}(_v) }}", rb_type_name(n)))
            }
            Some(Shape::Alias(inner)) => {
                let inner = inner.clone();
                rb_value_encoder(api, &inner, ns)
            }
            Some(Shape::Enum(_)) | None => None,
        },
        Ty::List(inner) => rb_value_encoder(api, inner, ns).map(|e| {
            format!("->(_v) {{ _v.is_a?(Array) ? _v.map {{ |_i| ({e}).call(_i) }} : _v }}")
        }),
        Ty::Map(inner) => rb_value_encoder(api, inner, ns).map(|e| {
            format!(
                "->(_v) {{ _v.is_a?(Hash) ? _v.to_h {{ |_k, _i| [_k, ({e}).call(_i)] }} : _v }}"
            )
        }),
        _ => None,
    }
}

/// Emit ENCODE_* key tables and encode_* module functions for every named
/// type reachable from a request position.
fn emit_request_encoders(api: &Api, out: &mut String) {
    let request_types = api.request_reachable();
    writeln!(
        out,
        "\n    # ---- request encoders (snake_case -> wire keys) ----\n"
    )
    .unwrap();
    write!(
        out,
        r#"    # Translate known snake_case keys (symbols or strings) to wire names,
    # encoding nested typed values. Wire-format and unknown keys pass through
    # untouched, so raw payloads and forward-compatible fields keep working.
    # Decoded Types::* value objects unwrap via to_h (round-trip); anything
    # else non-Hash is a caller bug surfaced BEFORE network I/O — silently
    # JSON-ing an arbitrary object would send its inspection string.
    def self.encode_fields(fields, data, drop: nil)
      data = data.to_h if !data.is_a?(Hash) && data.class.name.to_s.start_with?(name.split("::").first + "::Types")
      unless data.is_a?(Hash)
        raise TypeError, "expected a Hash (or a decoded Types value object), got #{{data.class}}"
      end

      out = {{}}
      claimed = {{}}
      data.each do |key, value|
        # Server-owned (readOnly) keys are silently REMOVED, so a fetched
        # resource can be modified and resubmitted without echoing state.
        next if drop&.include?(key.to_s)

        entry = fields[key.to_s]
        wire, encode = entry.nil? ? [key.to_s, nil] : entry
        # Two DISTINCT input keys normalizing to one wire key are a caller
        # bug surfaced pre-transport -- hash order must never pick a winner.
        if claimed.key?(wire) && claimed[wire] != key.to_s
          raise ArgumentError,
                "conflicting request keys #{{claimed[wire].inspect}} and #{{key.to_s.inspect}} " \
                "both map to the wire field #{{wire.inspect}}; supply exactly one spelling"
        end
        claimed[wire] = key.to_s
        out[entry.nil? ? key : wire] = encode && !value.nil? ? encode.call(value) : value
      end
      out
    end

"#
    )
    .unwrap();

    for decl in api.types.values() {
        if !request_types.contains(&decl.name) {
            continue;
        }
        let name = rb_type_name(&decl.name);
        match &decl.shape {
            Shape::Struct(s) => {
                if s.fields.is_empty() {
                    continue;
                }
                let refs: Vec<&Field> = s.input_fields().collect();
                let dropped: Vec<&Field> = s.fields.iter().filter(|f| f.read_only).collect();
                emit_rb_field_table(api, out, &name, &refs);
                emit_rb_drop_set(out, &name, &dropped);
                let drop_arg = if dropped.is_empty() {
                    String::new()
                } else {
                    format!(", drop: DROP_{}", table_const(&name))
                };
                writeln!(
                    out,
                    "    def self.encode_{name}(data)\n      encode_fields(ENCODE_{}, data{drop_arg})\n    end\n",
                    table_const(&name)
                )
                .unwrap();
            }
            Shape::Union(u) => match &u.discriminator {
                Some(disc) => {
                    writeln!(out, "    def self.encode_{name}(data)").unwrap();
                    // Same boundary policy as encode_fields: value objects
                    // unwrap, anything else non-Hash is a TypeError.
                    writeln!(out, "      data = data.to_h if !data.is_a?(Hash) && data.class.name.to_s.start_with?(name.split(\"::\").first + \"::Types\")").unwrap();
                    writeln!(out, "      unless data.is_a?(Hash)").unwrap();
                    writeln!(out, "        raise TypeError, \"expected a Hash (or a decoded Types value object), got #{{data.class}}\"").unwrap();
                    writeln!(out, "      end\n").unwrap();
                    writeln!(
                        out,
                        "      case data[\"{}\"] || data[:{}]",
                        disc.property, disc.property
                    )
                    .unwrap();
                    for v in &u.variants {
                        let Some(tag) = &v.tag else { continue };
                        if let Some(enc) = rb_value_encoder(api, &v.ty, "") {
                            writeln!(out, "      when \"{tag}\"").unwrap();
                            writeln!(out, "        ({enc}).call(data)").unwrap();
                        }
                    }
                    writeln!(out, "      else\n        data\n      end\n    end\n").unwrap();
                }
                None => {
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
                    emit_rb_field_table(api, out, &name, &merged);
                    emit_rb_drop_set(out, &name, &dropped);
                    let drop_arg = if dropped.is_empty() {
                        String::new()
                    } else {
                        format!(", drop: DROP_{}", table_const(&name))
                    };
                    writeln!(
                        out,
                        "    def self.encode_{name}(data)\n      encode_fields(ENCODE_{}, data{drop_arg})\n    end\n",
                        table_const(&name)
                    )
                    .unwrap();
                }
            },
            Shape::Enum(_) | Shape::Alias(_) => {}
        }
    }
}

/// Ruby constant names must be SHOUTY-compatible; type names may carry
/// underscores already, so uppercase deterministically.
fn table_const(name: &str) -> String {
    name.to_snake_case().to_uppercase()
}

/// Server-owned (readOnly) keys the request encoder silently drops, in both
/// snake_case and wire spellings.
fn emit_rb_drop_set(out: &mut String, name: &str, dropped: &[&Field]) {
    if dropped.is_empty() {
        return;
    }
    let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for f in dropped {
        keys.insert(rb_name(&f.wire_name));
        keys.insert(f.wire_name.clone());
    }
    let list = keys
        .iter()
        .map(|k| format!("\"{k}\""))
        .collect::<Vec<_>>()
        .join(", ");
    writeln!(out, "    DROP_{} = [{list}].freeze\n", table_const(name)).unwrap();
}

fn emit_rb_field_table(api: &Api, out: &mut String, name: &str, fields: &[&Field]) {
    writeln!(out, "    ENCODE_{} = {{", table_const(name)).unwrap();
    for f in fields {
        let snake = rb_name(&f.wire_name);
        let encoder = rb_value_encoder(api, &f.ty, "").unwrap_or_else(|| "nil".into());
        writeln!(
            out,
            "      \"{snake}\" => [\"{}\", {encoder}],",
            f.wire_name
        )
        .unwrap();
        if snake != f.wire_name {
            writeln!(
                out,
                "      \"{}\" => [\"{}\", {encoder}],",
                f.wire_name, f.wire_name
            )
            .unwrap();
        }
    }
    writeln!(out, "    }}.freeze\n").unwrap();
}

/// Names of every named type reachable from a request position.
fn emit_type_decl(api: &Api, out: &mut String, decl: &TypeDecl) {
    let name = rb_type_name(&decl.name);
    match &decl.shape {
        Shape::Struct(s) => {
            if s.fields.is_empty() && s.additional.is_some() {
                // Pure map type: decoded as a plain Hash.
                return;
            }
            writeln!(out, "    class {name}").unwrap();
            // Response direction: writeOnly fields are input secrets and are
            // never promised (or exposed via readers/to_h) here.
            let out_fields: Vec<&Field> = s.output_fields().collect();
            if !out_fields.is_empty() {
                let readers: Vec<String> = out_fields
                    .iter()
                    .map(|f| format!(":{}", rb_name(&f.wire_name)))
                    .collect();
                writeln!(out, "      attr_reader {}", readers.join(", ")).unwrap();
                out.push('\n');
                let kwargs: Vec<String> = out_fields
                    .iter()
                    .map(|f| format!("{}: nil", rb_name(&f.wire_name)))
                    .collect();
                writeln!(out, "      def initialize({})", kwargs.join(", ")).unwrap();
                for f in &out_fields {
                    let n = rb_name(&f.wire_name);
                    writeln!(out, "        @{n} = {n}").unwrap();
                }
                writeln!(out, "      end\n").unwrap();
                writeln!(out, "      def self.from_json(data)").unwrap();
                writeln!(out, "        new(").unwrap();
                for f in &out_fields {
                    let access = format!("data[\"{}\"]", f.wire_name);
                    writeln!(
                        out,
                        "          {}: {},",
                        rb_name(&f.wire_name),
                        guarded_decode(api, &f.ty, &access)
                    )
                    .unwrap();
                }
                writeln!(out, "        )").unwrap();
                writeln!(out, "      end\n").unwrap();
                // Round-trip support: a decoded value object can be passed
                // straight back into a request (snake keys; the encoders
                // translate to wire names). Nested objects unwrap recursively.
                writeln!(out, "      def to_h").unwrap();
                writeln!(out, "        {{").unwrap();
                for f in &out_fields {
                    let n = rb_name(&f.wire_name);
                    writeln!(out, "          {n}: Util.plain(@{n}),").unwrap();
                }
                writeln!(out, "        }}.reject {{ |_k, v| v.nil? }}").unwrap();
                writeln!(out, "      end").unwrap();
            } else {
                writeln!(out, "      def self.from_json(_data)").unwrap();
                writeln!(out, "        new").unwrap();
                writeln!(out, "      end\n").unwrap();
                writeln!(out, "      def to_h").unwrap();
                writeln!(out, "        {{}}").unwrap();
                writeln!(out, "      end").unwrap();
            }
            writeln!(out, "    end\n").unwrap();
        }
        Shape::Enum(e) => {
            // Enums are plain strings; expose the values as a frozen list.
            let values: Vec<String> = e.values.iter().map(|v| format!("\"{v}\"")).collect();
            writeln!(out, "    {name} = [{}].freeze\n", values.join(", ")).unwrap();
        }
        Shape::Union(u) => {
            writeln!(out, "    def self.decode_{name}(data)").unwrap();
            match &u.discriminator {
                Some(disc) => {
                    // Protobuf JSON gateways encode an unset optional union
                    // as null, {}, or a default-value discriminator ("");
                    // all three decode as absent. Unknown NON-empty tags
                    // still raise.
                    writeln!(
                        out,
                        "      return nil if data.nil? || data[\"{}\"].to_s.empty?",
                        disc.property
                    )
                    .unwrap();
                    writeln!(out, "      case data[\"{}\"]", disc.property).unwrap();
                    for v in &u.variants {
                        let Some(tag) = &v.tag else { continue };
                        writeln!(out, "      when \"{tag}\"").unwrap();
                        writeln!(out, "        {}", decode_expr(api, &v.ty, "data")).unwrap();
                    }
                    writeln!(out, "      else").unwrap();
                    writeln!(
                        out,
                        "        raise ArgumentError, \"{name}: unknown {} #{{data[\"{}\"].inspect}}\"",
                        disc.property, disc.property
                    )
                    .unwrap();
                    writeln!(out, "      end").unwrap();
                }
                None => {
                    for v in &u.variants {
                        let check = variant_check(api, &v.ty);
                        writeln!(
                            out,
                            "      return {} if {check}",
                            decode_expr(api, &v.ty, "data")
                        )
                        .unwrap();
                    }
                    writeln!(
                        out,
                        "      raise ArgumentError, \"{name}: no variant matched\""
                    )
                    .unwrap();
                }
            }
            writeln!(out, "    end\n").unwrap();
        }
        // Aliases decode through their target; nothing to declare.
        Shape::Alias(_) => {}
    }
}

/// A structural predicate for undiscriminated unions: first variant whose
/// shape matches wins.
fn variant_check(api: &Api, ty: &Ty) -> String {
    match ty {
        Ty::String | Ty::Timestamp | Ty::Bytes | Ty::Literal(_) => "data.is_a?(String)".into(),
        Ty::Bool => "[true, false].include?(data)".into(),
        Ty::Int32 | Ty::Int64 => "data.is_a?(Integer)".into(),
        Ty::Float | Ty::Double => "data.is_a?(Numeric)".into(),
        Ty::List(_) => "data.is_a?(Array)".into(),
        Ty::Map(_) | Ty::Json => "data.is_a?(Hash)".into(),
        Ty::Named(n) => match resolved_shape(api, n) {
            Some(Shape::Struct(s)) => {
                let required: Vec<String> = s
                    .fields
                    .iter()
                    .filter(|f| f.required && !f.nullable)
                    .map(|f| format!("\"{}\"", f.wire_name))
                    .collect();
                if required.is_empty() {
                    "data.is_a?(Hash)".into()
                } else {
                    format!(
                        "data.is_a?(Hash) && [{}].all? {{ |k| data.key?(k) }}",
                        required.join(", ")
                    )
                }
            }
            Some(Shape::Enum(_)) => "data.is_a?(String)".into(),
            _ => "true".into(),
        },
    }
}

// ---- resources ---------------------------------------------------------------

pub(crate) fn client_param<'a>(api: &'a Api, wire_name: &str) -> Option<&'a ClientParam> {
    api.client_params.iter().find(|c| c.wire_name == wire_name)
}

fn emit_resource(api: &Api, resource: &Resource, module: &str) -> String {
    let mut out = format!(
        "# frozen_string_literal: true\n\n# Code generated by redwood. DO NOT EDIT.\n\nmodule {module}\n  module Resources\n"
    );
    // Nested resources hang off this one as readers.
    let children: Vec<&Resource> = api
        .resources
        .iter()
        .filter(|r| r.parent.as_deref() == Some(resource.name.as_str()))
        .collect();
    let class = class_name(resource);
    writeln!(out, "    class {class}").unwrap();
    if !children.is_empty() {
        let readers: Vec<String> = children.iter().map(|c| format!(":{}", c.name)).collect();
        writeln!(out, "      attr_reader {}", readers.join(", ")).unwrap();
        out.push('\n');
    }
    writeln!(out, "      def initialize(core)").unwrap();
    writeln!(out, "        @core = core").unwrap();
    for child in &children {
        writeln!(
            out,
            "        @{} = {}.new(core)",
            child.name,
            class_name(child)
        )
        .unwrap();
    }
    writeln!(out, "      end").unwrap();
    for op in &resource.operations {
        out.push('\n');
        emit_method(api, op, &mut out);
    }
    out.push_str("    end\n  end\nend\n");
    out
}

/// Optional trailing skip-events argument for stream construction.
fn rb_skip_arg(api: &Api) -> String {
    if api.sse_skip_events.is_empty() {
        return String::new();
    }
    let list = api
        .sse_skip_events
        .iter()
        .map(|e| format!("\"{e}\""))
        .collect::<Vec<_>>()
        .join(", ");
    format!(", skip_events: [{list}]")
}

fn emit_method(api: &Api, op: &Operation, out: &mut String) {
    // Signature: positional own-ID, then keyword args for everything else.
    let mut sig: Vec<String> = Vec::new();
    for p in &op.positionals {
        sig.push(rb_name(&p.wire_name));
    }
    // (declaration, wire name, required) — required data reads first in the
    // declaration; binding is by keyword, so this orders docs, not calls.
    let mut entries: Vec<(String, &str, bool)> = Vec::new();
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        let is_client = client_param(api, &p.wire_name).is_some();
        let required = p.required && !is_client;
        let decl = if required {
            format!("{}:", rb_name(&p.wire_name))
        } else {
            format!("{}: nil", rb_name(&p.wire_name))
        };
        entries.push((decl, &p.wire_name, required));
    }
    for f in &op.body_fields {
        let decl = if f.required {
            format!("{}:", rb_name(&f.wire_name))
        } else {
            format!("{}: nil", rb_name(&f.wire_name))
        };
        entries.push((decl, &f.wire_name, f.required));
    }
    if op.whole_body.is_some() {
        entries.push(("body:".to_string(), "body", true));
    }
    entries.sort_by_key(|(_, _, required)| !required);
    let mut kwargs: Vec<&str> = Vec::new();
    for (decl, wire, _) in &entries {
        sig.push(decl.clone());
        kwargs.push(wire);
    }
    let is_sse = matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_)));
    if is_sse {
        sig.push("last_event_id: nil".to_string());
    }
    // Transport controls stay one visibly-separate keyword, always last.
    sig.push("request_options: nil".to_string());
    if let Some(s) = &op.summary {
        writeln!(out, "      # {}", s.trim().replace('\n', " ")).unwrap();
    }
    if sig.is_empty() {
        writeln!(out, "      def {}", rb_name(&op.name)).unwrap();
    } else {
        writeln!(out, "      def {}({})", rb_name(&op.name), sig.join(", ")).unwrap();
    }

    // Client-level defaults.
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        let Some(c) = client_param(api, &p.wire_name) else {
            continue;
        };
        writeln!(
            out,
            "        {name} = @core.resolve_default(\"{wire}\", \"{env}\", {name})",
            name = rb_name(&p.wire_name),
            wire = c.wire_name,
            env = c.env_var,
        )
        .unwrap();
    }

    // Path.
    let mut path_expr = String::new();
    let mut rest = op.path.as_str();
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|e| start + e)
            .expect("balanced");
        path_expr.push_str(&rest[..start]);
        let param = &rest[start + 1..end];
        path_expr.push_str(&format!(
            "#{{Util.path_param('{param}', {})}}",
            rb_name(param)
        ));
        rest = &rest[end + 1..];
    }
    path_expr.push_str(rest);
    writeln!(out, "        _path = \"{path_expr}\"").unwrap();

    // Pagination closes over these params for later page fetches: snapshot
    // array-valued filters NOW so caller mutation after page 1 cannot change
    // the query mid-traversal and mix result sets.
    if op.pagination.is_some() {
        for p in &op.query_params {
            if matches!(p.ty, Ty::List(_)) {
                let n = rb_name(&p.wire_name);
                writeln!(out, "        {n} = {n}.dup if {n}.is_a?(Array)").unwrap();
            }
        }
    }

    // Query / body hashes (wire keys; nil values dropped by the core).
    let has_query = !op.query_params.is_empty();
    if has_query {
        writeln!(out, "        _query = {{").unwrap();
        for p in &op.query_params {
            writeln!(
                out,
                "          \"{}\" => {},",
                p.wire_name,
                rb_name(&p.wire_name)
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
            // nil (omitted optional) must bypass the encoder: reject strips
            // it below, and the encoders type-check their input loudly.
            let value = match rb_value_encoder(api, &f.ty, "Types.") {
                Some(enc) => format!(
                    "{n}.nil? ? nil : ({enc}).call({n})",
                    n = rb_name(&f.wire_name)
                ),
                None => rb_name(&f.wire_name),
            };
            writeln!(out, "          \"{}\" => {value},", f.wire_name).unwrap();
        }
        writeln!(out, "        }}.reject {{ |_k, v| v.nil? }}").unwrap();
    }
    let query_arg = if has_query { ", query: _query" } else { "" };
    let body_arg = if has_body {
        ", body: _body".to_string()
    } else if let Some(ty) = &op.whole_body {
        match rb_value_encoder(api, ty, "Types.") {
            Some(enc) => format!(", body: ({enc}).call(body)"),
            None => ", body: body".to_string(),
        }
    } else {
        String::new()
    };
    let http = op.http_method.as_str().to_lowercase();

    match (&op.pagination, &op.response) {
        (Some(page), _) => {
            writeln!(
                out,
                "        _data = @core.request(:{http}, _path{query_arg}{body_arg}, request_options: request_options) || {{}}"
            )
            .unwrap();
            let item_decode = decode_expr(api, &page.item_ty, "item");
            let items_expr = if item_decode == "item" {
                format!("(_data[\"{}\"] || [])", page.items_field)
            } else {
                format!(
                    "(_data[\"{}\"] || []).map {{ |item| {item_decode} }}",
                    page.items_field
                )
            };
            writeln!(out, "        _items = {items_expr}").unwrap();
            let mut cursor_access = String::from("_data");
            let segments: Vec<&str> = page.next_cursor_path.split('.').collect();
            for (i, segment) in segments.iter().enumerate() {
                if i + 1 == segments.len() {
                    cursor_access = format!("({cursor_access})[\"{segment}\"].to_s");
                } else {
                    cursor_access = format!("(({cursor_access})[\"{segment}\"] || {{}})");
                }
            }
            writeln!(out, "        _next_cursor = {cursor_access}").unwrap();
            // Refetch re-enters the method with the cursor swapped in.
            let mut refetch_args: Vec<String> = Vec::new();
            for p in &op.positionals {
                refetch_args.push(rb_name(&p.wire_name));
            }
            for wire in &kwargs {
                let name = rb_name(wire);
                if *wire == page.cursor_param {
                    refetch_args.push(format!("{name}: _c"));
                } else {
                    refetch_args.push(format!("{name}: {name}"));
                }
            }
            // Page fetches keep the caller's transport options unchanged.
            refetch_args.push("request_options: request_options".to_string());
            writeln!(out, "        Page.new(_items, _next_cursor) do |_c|").unwrap();
            writeln!(
                out,
                "          {}({})",
                rb_name(&op.name),
                refetch_args.join(", ")
            )
            .unwrap();
            writeln!(out, "        end").unwrap();
        }
        (None, ResponseKind::Json(ty)) => {
            writeln!(
                out,
                "        _data = @core.request(:{http}, _path{query_arg}{body_arg}, request_options: request_options)"
            )
            .unwrap();
            writeln!(out, "        {}", decode_expr(api, ty, "_data")).unwrap();
        }
        (None, ResponseKind::Sse(ty)) => {
            let decode = decode_expr(api, ty, "event");
            writeln!(
                out,
                "        _headers = last_event_id ? {{ \"Last-Event-ID\" => last_event_id }} : nil"
            )
            .unwrap();
            writeln!(out, "        _decoder = ->(event) {{ {decode} }}").unwrap();
            writeln!(
                out,
                "        _auto_reconnect = !(request_options && request_options[:reconnect] == false)\n        Stream.new(_decoder, last_event_id: last_event_id{skip_arg}, auto_reconnect: _auto_reconnect) do |_cancel, _resume, &on_chunk|",
                skip_arg = rb_skip_arg(api)
            )
            .unwrap();
            writeln!(
                out,
                "          @core.stream_request(:{http}, _path{query_arg}{body_arg}, headers: (_resume ? {{ \"Last-Event-ID\" => _resume }} : nil), request_options: request_options, cancel: _cancel, &on_chunk)"
            )
            .unwrap();
            writeln!(out, "        end").unwrap();
        }
        (None, ResponseKind::Empty) => {
            // Declared-void: 204/empty succeed (output-bearing ops reject them).
            writeln!(
                out,
                "        @core.request(:{http}, _path{query_arg}{body_arg}, expects_body: false, request_options: request_options)"
            )
            .unwrap();
            writeln!(out, "        nil").unwrap();
        }
    }
    writeln!(out, "      end").unwrap();
}

// ---- client ------------------------------------------------------------------

fn emit_client(api: &Api, module: &str, config: &crate::config::RubyConfig) -> String {
    let mut out = format!(
        "# frozen_string_literal: true\n\n# Code generated by redwood. DO NOT EDIT.\n\nrequire \"json\"\n\nmodule {module}\n"
    );
    writeln!(
        out,
        "  # The {} API client. Resource groups are readers; unset options are\n  # read from the environment.",
        api.name
    )
    .unwrap();
    writeln!(out, "  class Client").unwrap();
    let readers: Vec<String> = api
        .resources
        .iter()
        .filter(|r| r.parent.is_none())
        .map(|r| format!(":{}", r.name))
        .collect();
    writeln!(out, "    attr_reader {}", readers.join(", ")).unwrap();
    out.push('\n');
    let mut ctor: Vec<String> = vec!["api_key: nil".into(), "base_url: nil".into()];
    if !api.webhooks.is_empty() {
        ctor.push("webhook_secret: nil".into());
    }
    ctor.push(format!("max_retries: {}", api.max_retries));
    ctor.push("connection: nil".into());
    ctor.push("stream_transport: nil".into());
    for c in &api.client_params {
        ctor.push(format!("{}: nil", rb_name(&c.wire_name)));
    }
    writeln!(out, "    def initialize({})", ctor.join(", ")).unwrap();
    // Presence and validity are separate: explicitly supplied blank values
    // are configuration errors and never fall back to the environment.
    let mut blank_checked: Vec<String> = vec!["api_key".into(), "base_url".into()];
    if !api.webhooks.is_empty() {
        blank_checked.push("webhook_secret".into());
    }
    for c in &api.client_params {
        blank_checked.push(rb_name(&c.wire_name));
    }
    for name in &blank_checked {
        writeln!(
            out,
            "      if !{name}.nil? && {name}.to_s.strip.empty?\n        raise ArgumentError, \"{name} must not be blank\"\n      end"
        )
        .unwrap();
    }
    if !matches!(api.auth, Auth::None) {
        // Authenticated APIs require a credential; a no-security API
        // constructs unauthenticated and demands nothing.
        writeln!(
            out,
            "      api_key = (api_key || ENV.fetch(\"{}\", \"\")).to_s.strip",
            api.api_key_env
        )
        .unwrap();
        writeln!(out, "      if api_key.empty?").unwrap();
        writeln!(
            out,
            "        raise ArgumentError, \"missing API key: pass api_key: or set {}\"",
            api.api_key_env
        )
        .unwrap();
        writeln!(out, "      end").unwrap();
    }
    writeln!(
        out,
        "      base_url = (base_url || ENV.fetch(\"{}_BASE_URL\", \"\")).to_s.strip",
        api.name.to_uppercase()
    )
    .unwrap();
    writeln!(
        out,
        "      base_url = \"{}\" if base_url.empty?",
        api.base_url
    )
    .unwrap();
    if !api.webhooks.is_empty() {
        writeln!(
            out,
            "      @webhook_secret = (webhook_secret || ENV.fetch(\"{}\", \"\")).to_s.strip",
            api.webhook_env
        )
        .unwrap();
    }
    writeln!(out, "      defaults = {{}}").unwrap();
    for c in &api.client_params {
        writeln!(
            out,
            "      defaults[\"{}\"] = ({} || ENV.fetch(\"{}\", \"\")).to_s.strip",
            c.wire_name,
            rb_name(&c.wire_name),
            c.env_var
        )
        .unwrap();
    }
    let auth = match &api.auth {
        Auth::Bearer => "[\"Authorization\", \"Bearer #{api_key}\"]".to_string(),
        Auth::ApiKeyHeader(h) => format!("[\"{h}\", api_key]"),
        Auth::None => "[\"\", \"\"]".to_string(),
    };
    writeln!(out, "      core = Core.new(").unwrap();
    writeln!(out, "        base_url: base_url,").unwrap();
    writeln!(out, "        auth_header: {auth},").unwrap();
    writeln!(out, "        max_retries: max_retries,").unwrap();
    writeln!(out, "        defaults: defaults,").unwrap();
    writeln!(
        out,
        "        user_agent: \"{}-ruby/{} (api {})\",",
        api.name.to_lowercase(),
        config.package_version.as_deref().unwrap_or("0.1.0"),
        api.version
    )
    .unwrap();
    writeln!(out, "        connection: connection,").unwrap();
    writeln!(out, "        stream_transport: stream_transport").unwrap();
    writeln!(out, "      )").unwrap();
    for resource in api.resources.iter().filter(|r| r.parent.is_none()) {
        writeln!(
            out,
            "      @{} = Resources::{}.new(core)",
            resource.name,
            class_name(resource)
        )
        .unwrap();
    }
    writeln!(out, "    end").unwrap();

    if !api.webhooks.is_empty() {
        let payload = webhook_payload(api);
        let decode = match &payload {
            Some(ty) => decode_expr(api, ty, "JSON.parse(payload)"),
            None => "JSON.parse(payload)".to_string(),
        };
        write!(
            out,
            r#"
    # Verify a Standard Webhooks delivery and return the typed payload.
    def unwrap_webhook(payload, headers)
      if @webhook_secret.to_s.empty?
        raise ArgumentError, "missing webhook secret: pass webhook_secret: or set {env}"
      end
      Webhooks.verify(@webhook_secret, payload, headers)
      {decode}
    end
"#,
            env = api.webhook_env,
        )
        .unwrap();
    }
    out.push_str("  end\nend\n");
    out
}

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

fn emit_entry(api: &Api, gem: &str) -> String {
    let mut out = String::from(
        "# frozen_string_literal: true\n\n# Code generated by redwood. DO NOT EDIT.\n\n",
    );
    for file in ["core", "pagination", "sse", "types"] {
        writeln!(out, "require_relative \"{gem}/{file}\"").unwrap();
    }
    if !api.webhooks.is_empty() {
        writeln!(out, "require_relative \"{gem}/webhooks\"").unwrap();
    }
    for resource in &api.resources {
        writeln!(
            out,
            "require_relative \"{gem}/resources/{}\"",
            resource.ident
        )
        .unwrap();
    }
    writeln!(out, "require_relative \"{gem}/client\"").unwrap();
    out
}

fn emit_gemfile() -> String {
    "# frozen_string_literal: true\n\nsource \"https://rubygems.org\"\n\ngemspec\n\ngroup :development, :test do\n  gem \"rspec\", \"~> 3.13\"\n  gem \"vcr\", \"~> 6.2\"\n  gem \"webmock\", \"~> 3.23\"\nend\n".to_string()
}

fn emit_gemspec(api: &Api, gem: &str, config: &crate::config::RubyConfig) -> String {
    format!(
        r#"# frozen_string_literal: true

Gem::Specification.new do |spec|
  spec.name = "{gem}"
  spec.version = "{version}"
  spec.summary = "The official Ruby SDK for the {name} API"
  spec.description = "Generated client for the {name} API: resources, pagination, SSE streaming, and webhook verification. See README.md and api.md."
  spec.authors = [{authors}]
  spec.license = "{license}"
{homepage}  spec.files = Dir["lib/**/*.rb"] + ["README.md", "api.md"]
  spec.required_ruby_version = ">= 3.1"
  spec.metadata = {{
{metadata}    "rubygems_mfa_required" => "true"
  }}
  spec.add_dependency "faraday", "~> 2.0"
  # base64 left the default gem set in Ruby 3.4; webhooks.rb requires it,
  # and the root file loads webhooks eagerly.
  spec.add_dependency "base64", ">= 0.1"
end
"#,
        name = api.name,
        version = config.package_version.as_deref().unwrap_or("0.1.0"),
        // RubyGems validates against SPDX; "Nonstandard" is its documented
        // value for a proprietary/nonstandard license (npm's UNLICENSED).
        license = config.license.as_deref().unwrap_or("Nonstandard"),
        authors = match &config.authors {
            Some(names) if !names.is_empty() => names
                .iter()
                .map(|n| format!("\"{n}\""))
                .collect::<Vec<_>>()
                .join(", "),
            _ => format!("\"{}\"", api.name),
        },
        homepage = config
            .homepage
            .as_deref()
            .map(|h| format!("  spec.homepage = \"{h}\"\n"))
            .unwrap_or_default(),
        metadata = {
            let mut entries = String::new();
            for (key, value) in [
                ("homepage_uri", &config.homepage),
                ("source_code_uri", &config.source_code_uri),
                ("changelog_uri", &config.changelog_uri),
            ] {
                if let Some(url) = value {
                    entries.push_str(&format!("    \"{key}\" => \"{url}\",\n"));
                }
            }
            entries
        }
    )
}

// ---- conformance driver --------------------------------------------------------

/// Render a manifest sample as a Ruby literal.
pub(crate) fn rb_literal(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Null => "nil".into(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::String(_) => value.to_string(),
        serde_json::Value::Array(items) => {
            let inner: Vec<String> = items.iter().map(rb_literal).collect();
            format!("[{}]", inner.join(", "))
        }
        serde_json::Value::Object(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{} => {}",
                        serde_json::Value::String(k.clone()),
                        rb_literal(v)
                    )
                })
                .collect();
            format!("{{{}}}", inner.join(", "))
        }
    }
}

fn emit_conformance(api: &Api, gem: &str, module: &str) -> String {
    let mut out = format!(
        r#"# frozen_string_literal: true

# Code generated by redwood. DO NOT EDIT.
# Conformance driver: calls every operation against the mock at MOCK_URL.
# Loads the gem from the ambient load path/GEM_HOME — run with `-Ilib` for
# a source-tree run, or under an installed gem home for the packaging gate.
require "{gem}"
warn "{gem} loaded from: #{{$LOADED_FEATURES.grep(%r{{{gem}/client}}).first}}"

$failures = 0

def run(op_id)
  yield
  puts "PASS #{{op_id}}"
rescue StandardError => e
  $failures += 1
  puts "FAIL #{{op_id}}: #{{e.message.to_s[0, 160]}}"
end

def check_page(page)
  # Auto-iterate: the mock serves two pages and rejects the second when any
  # non-cursor query param drifts from the first request.
  total = page.count
  raise "expected 2 items across pages, got #{{total}}" unless total == 2
end

def check_stream(stream)
  count = stream.count
  raise "expected 2 events, got #{{count}}" unless count == 2
end

client = {module}::Client.new({conf_auth}base_url: ENV.fetch("MOCK_URL"))

"#,
        conf_auth = if matches!(api.auth, Auth::None) {
            ""
        } else {
            "api_key: \"conformance-key\", "
        }
    );

    for resource in &api.resources {
        for op in &resource.operations {
            let mut args: Vec<String> = Vec::new();
            for _ in &op.positionals {
                args.push("\"sample\"".into());
            }
            for p in op.path_params.iter().chain(op.query_params.iter()) {
                let sample = super::manifest_sample(api, &p.ty);
                args.push(format!(
                    "{}: {}",
                    rb_name(&p.wire_name),
                    rb_literal(&sample)
                ));
            }
            for f in &op.body_fields {
                // snake_case nested keys: the SDK's request encoders must
                // translate them back to wire names for the mock to accept.
                let sample = super::snake_sample(api, &f.ty, super::manifest_sample(api, &f.ty));
                args.push(format!(
                    "{}: {}",
                    rb_name(&f.wire_name),
                    rb_literal(&sample)
                ));
            }
            if let Some(ty) = &op.whole_body {
                let sample = super::snake_sample(api, ty, super::manifest_sample(api, ty));
                args.push(format!("body: {}", rb_literal(&sample)));
            }
            let invoke = format!(
                "client.{}.{}({})",
                resource.path(),
                rb_name(&op.name),
                args.join(", ")
            );
            let wrapped = match (&op.pagination, &op.response) {
                (Some(_), _) => format!("check_page({invoke})"),
                (None, ResponseKind::Sse(_)) => format!("check_stream({invoke})"),
                _ => invoke,
            };
            writeln!(out, "run(\"{}\") {{ {} }}", op.id, wrapped).unwrap();
        }
    }
    writeln!(out, "\nexit([$failures, 1].min)").unwrap();
    out
}
