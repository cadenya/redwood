//! Generated RSpec suite for the Ruby SDK.
//!
//! Layout mirrors what the reference Ruby API clients ship inside the gem:
//! one spec file per resource (an example per operation, `context` blocks
//! for the permutations that exist structurally — client-default params,
//! pagination, SSE), a behavioral spec for cross-cutting runtime contracts
//! (errors, retries, request options, encoding), and VCR cassettes as the
//! GOLDEN FILES: each per-operation example replays its generated cassette
//! with strict method/path/query/JSON-body matching, so a drifted request
//! shape fails as an unhandled-request error; refresh goldens by
//! regenerating, never by live recording. Everything here derives structurally
//! from the IR — no spec-specific names in this module.

use serde_json::{json, Value};
use std::fmt::Write as _;

use super::ruby::{client_param, module_name, rb_literal, rb_name, rb_type_name};
use super::{manifest_sample, manifest_sample_output, snake_sample, FileSet};
use crate::ir::*;

const SPEC_BASE_URL: &str = "https://api.test.example";

pub(crate) fn emit(api: &Api, gem: &str, files: &mut FileSet) {
    let module = module_name(api);
    // Use an explicit relative path. A bare `spec_helper` can resolve to a
    // dependency's helper when the invoking Ruby does not put `spec/` first
    // on $LOAD_PATH (notably clean Linux/CI installations).
    files.insert(
        ".rspec".into(),
        "--require ./spec/spec_helper.rb\n--color\n".into(),
    );
    files.insert("spec/spec_helper.rb".into(), spec_helper(api, gem));
    for resource in &api.resources {
        files.insert(
            format!("spec/resources/{}_spec.rb", resource.ident),
            resource_spec(api, resource, &module),
        );
        for op in &resource.operations {
            files.insert(format!("spec/cassettes/{}.yml", op.id), cassette(api, op));
        }
    }
    files.insert("spec/behavior_spec.rb".into(), behavior_spec(api, &module));
    if !api.webhooks.is_empty() {
        files.insert("spec/webhooks_spec.rb".into(), webhooks_spec(api, &module));
    }
}

// ---- shared golden pieces ---------------------------------------------------

/// A housekeeping frame interleaved into SSE goldens when the API
/// configures [sse] skip_events: its data is deliberately NOT JSON, so a
/// stream that fails to skip it fails the test loudly instead of silently
/// yielding a third event.
fn skip_frame(api: &Api) -> String {
    match api.sse_skip_events.first() {
        Some(name) => format!("event: {name}\ndata: not-json-housekeeping\n\n"),
        None => String::new(),
    }
}

fn sample_path(op: &Operation) -> String {
    let mut out = String::new();
    let mut rest = op.path.as_str();
    while let Some(start) = rest.find('{') {
        let end = rest[start..]
            .find('}')
            .map(|e| start + e)
            .expect("balanced");
        out.push_str(&rest[..start]);
        out.push_str("sample");
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

fn scalar_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Flattened query pairs from the samples (repeated key per array element —
/// the SDK's flat encoding).
fn query_pairs(api: &Api, op: &Operation) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for p in &op.query_params {
        match manifest_sample(api, &p.ty) {
            Value::Array(items) => {
                for item in items {
                    pairs.push((p.wire_name.clone(), scalar_str(&item)));
                }
            }
            Value::Null => {}
            other => pairs.push((p.wire_name.clone(), scalar_str(&other))),
        }
    }
    pairs
}

fn query_string(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k}={}", v.replace(' ', "%20").replace('+', "%2B")))
        .collect::<Vec<_>>()
        .join("&")
}

/// The golden request body (wire keys), if the operation has one.
fn request_body_value(api: &Api, op: &Operation) -> Option<Value> {
    if !op.body_fields.is_empty() {
        let mut obj = serde_json::Map::new();
        for f in &op.body_fields {
            obj.insert(f.wire_name.clone(), manifest_sample(api, &f.ty));
        }
        return Some(Value::Object(obj));
    }
    op.whole_body.as_ref().map(|ty| manifest_sample(api, ty))
}

/// Set a dotted path inside a JSON object, creating intermediate objects.
fn set_in(value: &mut Value, path: &str, new: Value) {
    let mut current = value;
    let segments: Vec<&str> = path.split('.').collect();
    for (i, segment) in segments.iter().enumerate() {
        let obj = match current {
            Value::Object(map) => map,
            other => {
                *other = json!({});
                other.as_object_mut().unwrap()
            }
        };
        if i + 1 == segments.len() {
            obj.insert((*segment).to_string(), new);
            return;
        }
        current = obj
            .entry((*segment).to_string())
            .or_insert_with(|| json!({}));
    }
}

/// Call arguments in the SDK's public shape: positional samples first, then
/// snake_case keyword samples. `skip_client_params` leaves client-default
/// params to resolve from the client.
fn call_args(api: &Api, op: &Operation, skip_client_params: bool) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    for _ in &op.positionals {
        args.push("\"sample\"".into());
    }
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        if skip_client_params && client_param(api, &p.wire_name).is_some() {
            continue;
        }
        let sample = manifest_sample(api, &p.ty);
        args.push(format!(
            "{}: {}",
            rb_name(&p.wire_name),
            rb_literal(&sample)
        ));
    }
    for f in &op.body_fields {
        let sample = snake_sample(api, &f.ty, manifest_sample(api, &f.ty));
        args.push(format!(
            "{}: {}",
            rb_name(&f.wire_name),
            rb_literal(&sample)
        ));
    }
    if let Some(ty) = &op.whole_body {
        let sample = snake_sample(api, ty, manifest_sample(api, ty));
        args.push(format!("body: {}", rb_literal(&sample)));
    }
    args
}

fn call_expr(api: &Api, resource: &Resource, op: &Operation, skip_client_params: bool) -> String {
    let args = call_args(api, op, skip_client_params);
    let accessor = format!("client.{}", resource.path());
    if args.is_empty() {
        format!("{accessor}.{}", rb_name(&op.name))
    } else {
        format!("{accessor}.{}({})", rb_name(&op.name), args.join(", "))
    }
}

// ---- cassettes (golden files) ----------------------------------------------

fn interaction(
    method: &str,
    uri: String,
    body: Option<String>,
    status: u16,
    content_type: &str,
    response_body: String,
) -> Value {
    let mut request = json!({
        "method": method.to_lowercase(),
        "uri": uri,
        "body": { "encoding": "UTF-8", "string": body.clone().unwrap_or_default() },
        "headers": {},
    });
    if body.is_some() {
        request["headers"] = json!({ "Content-Type": ["application/json"] });
    }
    json!({
        "request": request,
        "response": {
            "status": { "code": status, "message": if status == 204 { "No Content" } else { "OK" } },
            "headers": { "Content-Type": [content_type] },
            "body": { "encoding": "UTF-8", "string": response_body },
            "http_version": "1.1",
        },
        "recorded_at": "Thu, 01 Jan 2026 00:00:00 GMT",
    })
}

fn cassette(api: &Api, op: &Operation) -> String {
    let path = sample_path(op);
    let pairs = query_pairs(api, op);
    let uri = |extra_pairs: &[(String, String)]| -> String {
        let qs = query_string(extra_pairs);
        if qs.is_empty() {
            format!("{SPEC_BASE_URL}{path}")
        } else {
            format!("{SPEC_BASE_URL}{path}?{qs}")
        }
    };
    let body = request_body_value(api, op).map(|v| v.to_string());
    let method = op.http_method.as_str();

    let interactions: Vec<Value> = match (&op.pagination, &op.response) {
        (Some(page), ResponseKind::Json(ty)) => {
            let make_page = |cursor: &str| -> Value {
                let mut body = manifest_sample_output(api, ty);
                set_in(
                    &mut body,
                    &page.items_field,
                    json!([manifest_sample_output(api, &page.item_ty)]),
                );
                set_in(&mut body, &page.next_cursor_path, json!(cursor));
                body
            };
            let mut second_pairs = pairs.clone();
            for pair in &mut second_pairs {
                if pair.0 == page.cursor_param {
                    pair.1 = "cursor_page_2".into();
                }
            }
            vec![
                interaction(
                    method,
                    uri(&pairs),
                    body.clone(),
                    200,
                    "application/json",
                    make_page("cursor_page_2").to_string(),
                ),
                interaction(
                    method,
                    uri(&second_pairs),
                    body,
                    200,
                    "application/json",
                    make_page("").to_string(),
                ),
            ]
        }
        (None, ResponseKind::Json(ty)) => vec![interaction(
            method,
            uri(&pairs),
            body,
            200,
            "application/json",
            manifest_sample_output(api, ty).to_string(),
        )],
        (None, ResponseKind::Sse(ty)) => {
            let payload = manifest_sample_output(api, ty).to_string();
            let sse = format!(
                "id: e1\ndata: {payload}\n\n{}id: e2\ndata: {payload}\n\n",
                skip_frame(api)
            );
            vec![interaction(
                method,
                uri(&pairs),
                body,
                200,
                "text/event-stream",
                sse,
            )]
        }
        (None, ResponseKind::Empty) => vec![interaction(
            method,
            uri(&pairs),
            body,
            204,
            "application/json",
            String::new(),
        )],
        (Some(_), _) => unreachable!("pagination implies JSON"),
    };

    let doc =
        json!({ "http_interactions": interactions, "recorded_with": "redwood (VCR 6.x format)" });
    format!(
        "---\n{}",
        serde_yaml::to_string(&doc).expect("cassette yaml")
    )
}

// ---- per-resource specs -----------------------------------------------------

fn result_assertions(api: &Api, op: &Operation, module: &str, out: &mut String) {
    match (&op.pagination, &op.response) {
        (Some(_), _) => {
            writeln!(out, "        items = result.to_a").unwrap();
            writeln!(out, "        expect(items.length).to eq(2)").unwrap();
        }
        (None, ResponseKind::Json(Ty::Named(n))) => match api.types.get(n).map(|d| &d.shape) {
            Some(Shape::Struct(s)) if !s.fields.is_empty() || s.additional.is_none() => {
                writeln!(
                    out,
                    "        expect(result).to be_a({module}::Types::{})",
                    rb_type_name(n)
                )
                .unwrap();
            }
            _ => {
                writeln!(out, "        expect(result).not_to be_nil").unwrap();
            }
        },
        (None, ResponseKind::Json(_)) => {
            writeln!(out, "        expect(result).not_to be_nil").unwrap();
        }
        (None, ResponseKind::Sse(_)) => {
            writeln!(out, "        events = result.to_a").unwrap();
            writeln!(out, "        expect(events.length).to eq(2)").unwrap();
            writeln!(out, "        expect(result.last_event_id).to eq(\"e2\")").unwrap();
        }
        (None, ResponseKind::Empty) => {
            writeln!(out, "        expect(result).to be_nil").unwrap();
        }
    }
}

fn resource_spec(api: &Api, resource: &Resource, module: &str) -> String {
    let mut out = format!(
        "# frozen_string_literal: true\n\n# Code generated by redwood. DO NOT EDIT.\n\nRSpec.describe \"client.{}\" do\n  let(:client) {{ build_client }}\n",
        resource.path()
    );
    for op in &resource.operations {
        writeln!(out, "\n  describe \"#{}\" do", rb_name(&op.name)).unwrap();

        // The golden example: replay the generated cassette with strict
        // method/path/query/JSON-body matching. A drifted request fails as
        // an unhandled-request error before any assertion runs.
        writeln!(
            out,
            "    it \"sends the golden request and decodes the response\" do"
        )
        .unwrap();
        writeln!(out, "      VCR.use_cassette(\"{}\") do", op.id).unwrap();
        writeln!(
            out,
            "        result = {}",
            call_expr(api, resource, op, false)
        )
        .unwrap();
        result_assertions(api, op, module, &mut out);
        writeln!(out, "      end").unwrap();
        writeln!(out, "    end").unwrap();

        // Client-default permutation: only when the operation actually has a
        // client-defaultable param.
        let defaults: Vec<&ClientParam> = op
            .path_params
            .iter()
            .chain(op.query_params.iter())
            .filter_map(|p| client_param(api, &p.wire_name))
            .collect();
        if let Some(c) = defaults.first() {
            let snake = rb_name(&c.wire_name);
            let default_value = format!("default_{snake}");
            // The default only shows in the URL for PATH params; query-side
            // defaults are asserted via the request query instead.
            let is_path = op.path.contains(&format!("{{{}}}", c.wire_name));
            writeln!(
                out,
                "\n    context \"when {snake} falls back to the client default\" do"
            )
            .unwrap();
            writeln!(out, "      it \"resolves the client-level value\" do").unwrap();
            let http = op.http_method.as_str().to_lowercase();
            if is_path {
                let mut path = op.path.clone();
                path = path.replace(&format!("{{{}}}", c.wire_name), &default_value);
                // Remaining placeholders get their explicit sample values.
                let mut expected = String::new();
                let mut rest = path.as_str();
                while let Some(start) = rest.find('{') {
                    let end = rest[start..]
                        .find('}')
                        .map(|e| start + e)
                        .expect("balanced");
                    expected.push_str(&rest[..start]);
                    expected.push_str("sample");
                    rest = &rest[end + 1..];
                }
                expected.push_str(rest);
                writeln!(
                    out,
                    "        stub = stub_request(:{http}, \"#{{SPEC_BASE_URL}}{expected}\")"
                )
                .unwrap();
            } else {
                writeln!(out, "        stub = stub_request(:{http}, %r{{\\A#{{Regexp.escape(SPEC_BASE_URL)}}}})").unwrap();
            }
            if !op.query_params.is_empty() {
                writeln!(out, "          .with(query: hash_including({{}}))").unwrap();
            }
            let stub_response = default_stub_response(api, op);
            writeln!(out, "          .to_return({stub_response})").unwrap();
            writeln!(
                out,
                "        {}",
                consume_expr(&call_expr(api, resource, op, true), op)
            )
            .unwrap();
            writeln!(out, "        expect(stub).to have_been_requested").unwrap();
            if !is_path {
                writeln!(out, "        expect(WebMock).to have_requested(:{http}, %r{{.*}}).with(query: hash_including(\"{}\" => \"{default_value}\"))", c.wire_name).unwrap();
            }
            writeln!(out, "      end").unwrap();
            writeln!(out, "    end").unwrap();
        }
        writeln!(out, "  end").unwrap();
    }
    out.push_str("end\n");
    out
}

/// Some results must be consumed for the request to happen or complete.
fn consume_expr(call: &str, op: &Operation) -> String {
    match (&op.pagination, &op.response) {
        (None, ResponseKind::Sse(_)) => format!("({call}).to_a"),
        _ => call.to_string(),
    }
}

fn default_stub_response(api: &Api, op: &Operation) -> String {
    match (&op.pagination, &op.response) {
        (Some(page), ResponseKind::Json(ty)) => {
            let mut body = manifest_sample_output(api, ty);
            set_in(
                &mut body,
                &page.items_field,
                json!([manifest_sample_output(api, &page.item_ty)]),
            );
            set_in(&mut body, &page.next_cursor_path, json!(""));
            format!(
                "status: 200, body: {}, headers: {{ \"Content-Type\" => \"application/json\" }}",
                rb_literal(&json!(body.to_string()))
            )
        }
        (None, ResponseKind::Json(ty)) => format!(
            "status: 200, body: {}, headers: {{ \"Content-Type\" => \"application/json\" }}",
            rb_literal(&json!(manifest_sample_output(api, ty).to_string()))
        ),
        (None, ResponseKind::Sse(ty)) => {
            let payload = manifest_sample_output(api, ty).to_string();
            let sse = format!(
                "id: e1\ndata: {payload}\n\n{}id: e2\ndata: {payload}\n\n",
                skip_frame(api)
            );
            format!(
                "status: 200, body: {}, headers: {{ \"Content-Type\" => \"text/event-stream\" }}",
                rb_literal(&json!(sse))
            )
        }
        (None, ResponseKind::Empty) => "status: 204, body: \"\"".to_string(),
        (Some(_), _) => unreachable!(),
    }
}

// ---- spec_helper ------------------------------------------------------------

fn spec_helper(api: &Api, gem: &str) -> String {
    let mut client_args: Vec<String> = Vec::new();
    match api.auth {
        Auth::None => {}
        Auth::Basic => {
            client_args.push("username: \"test-user\"".into());
            client_args.push("password: \"test-password\"".into());
        }
        _ => client_args.push("api_key: \"test-key\"".into()),
    }
    for c in &api.client_params {
        let snake = rb_name(&c.wire_name);
        client_args.push(format!("{snake}: \"default_{snake}\""));
    }
    client_args.push("base_url: SPEC_BASE_URL".into());
    format!(
        r#"# frozen_string_literal: true

# Code generated by redwood. DO NOT EDIT.
$LOAD_PATH.unshift File.expand_path("../lib", __dir__)

require "{gem}"
require "json"
require "webmock/rspec"
require "vcr"

SPEC_BASE_URL = "{base}"

VCR.configure do |c|
  c.cassette_library_dir = File.expand_path("cassettes", __dir__)
  c.hook_into :webmock
  # Cassettes are the GOLDEN FILES for every operation's request shape,
  # generated deterministically by redwood — refresh them by REGENERATING,
  # never by recording against a live server (recording would persist real
  # credentials and secret bodies into durable files).
  c.default_cassette_options = {{
    record: :none,
    match_requests_on: %i[method path query json_body],
    allow_unused_http_interactions: false,
  }}
  # Golden request bodies compare as PARSED JSON: serialization key order is
  # not part of the contract, but every key and value is.
  c.register_request_matcher :json_body do |real, recorded|
    real_body = real.body.to_s
    recorded_body = recorded.body.to_s
    if real_body.empty? || recorded_body.empty?
      real_body.empty? == recorded_body.empty?
    else
      JSON.parse(real_body) == JSON.parse(recorded_body)
    end
  end
end

def build_client(**overrides)
  {module}::Client.new(**{{ {client_args} }}.merge(overrides))
end

RSpec.configure do |config|
  config.expect_with :rspec do |expectations|
    expectations.include_chain_clauses_in_custom_matcher_descriptions = true
  end
  config.mock_with :rspec do |mocks|
    mocks.verify_partial_doubles = true
  end
  config.order = :random
  Kernel.srand config.seed
end
"#,
        base = SPEC_BASE_URL,
        module = module_name(api),
        client_args = client_args.join(", "),
    )
}

// ---- behavioral spec --------------------------------------------------------

/// Structural anchors: representative operations chosen by SHAPE, never by
/// name, per the schema-abstraction rule.
struct Anchors<'a> {
    simple_get: Option<(&'a Resource, &'a Operation)>,
    mutation: Option<(&'a Resource, &'a Operation)>,
    read_only: Option<(&'a Resource, &'a Operation, &'a Field, String, Vec<String>)>,
}

fn find_anchors(api: &Api) -> Anchors<'_> {
    let ops = || {
        api.resources
            .iter()
            .flat_map(|r| r.operations.iter().map(move |o| (r, o)))
    };
    let simple_get = ops().find(|(_, o)| {
        o.http_method.as_str() == "GET"
            && o.body_fields.is_empty()
            && o.whole_body.is_none()
            && o.pagination.is_none()
            && matches!(o.response, ResponseKind::Json(_))
    });
    let mutation = ops().find(|(_, o)| {
        matches!(o.http_method.as_str(), "POST" | "PATCH" | "PUT")
            && !o.body_fields.is_empty()
            && matches!(o.response, ResponseKind::Json(_))
            && o.pagination.is_none()
    });
    let read_only = ops().find_map(|(r, o)| {
        for f in &o.body_fields {
            let Ty::Named(n) = &f.ty else { continue };
            let Some(Shape::Struct(s)) = api.types.get(n).map(|d| &d.shape) else {
                continue;
            };
            let ro: Vec<String> = s
                .fields
                .iter()
                .filter(|x| x.read_only)
                .map(|x| x.wire_name.clone())
                .collect();
            if !ro.is_empty() {
                return Some((r, o, f, n.clone(), ro));
            }
        }
        None
    });
    Anchors {
        simple_get,
        mutation,
        read_only,
    }
}

fn behavior_spec(api: &Api, module: &str) -> String {
    let anchors = find_anchors(api);
    let mut out = String::from(
        "# frozen_string_literal: true\n\n# Code generated by redwood. DO NOT EDIT.\n# Cross-cutting runtime contracts, anchored on operations chosen by shape.\n\n",
    );

    // ---- client construction ----
    writeln!(out, "RSpec.describe \"client construction\" do").unwrap();
    for (label, bad) in [
        ("a query", "https://host.example?x=1"),
        ("a fragment", "https://host.example#frag"),
        ("userinfo", "https://user:pw@host.example"),
        ("a bare query delimiter", "https://host.example?"),
        ("a bare fragment delimiter", "https://host.example#"),
        ("a non-http scheme", "ftp://host.example"),
        ("no host", "https://"),
    ] {
        writeln!(out, "  context \"when the base URL carries {label}\" do").unwrap();
        writeln!(out, "    it \"is rejected before any request\" do").unwrap();
        writeln!(
            out,
            "      expect {{ build_client(base_url: \"{bad}\") }}.to raise_error(ArgumentError)"
        )
        .unwrap();
        writeln!(out, "    end").unwrap();
        writeln!(out, "  end").unwrap();
    }
    writeln!(out, "end").unwrap();

    let Some((resource, op)) = anchors.simple_get else {
        return out;
    };
    let get_call = call_expr(api, resource, op, false);
    let get_path = sample_path(op);
    let get_response = default_stub_response(api, op);
    let has_query = !op.query_params.is_empty();
    let with_query = if has_query {
        ".with(query: hash_including({}))"
    } else {
        ""
    };
    let stub_line = format!("stub_request(:get, \"#{{SPEC_BASE_URL}}{get_path}\"){with_query}");

    // ---- auth ----
    match &api.auth {
        Auth::Bearer => {
            writeln!(out, "\nRSpec.describe \"authentication\" do").unwrap();
            writeln!(out, "  let(:client) {{ build_client }}").unwrap();
            writeln!(
                out,
                "  it \"sends the bearer credential on every request\" do"
            )
            .unwrap();
            writeln!(out, "    stub = {stub_line}.to_return({get_response})").unwrap();
            writeln!(out, "    {get_call}").unwrap();
            writeln!(out, "    expect(stub).to have_been_requested").unwrap();
            writeln!(out, "    expect(WebMock).to(have_requested(:get, %r{{.*}}).with {{ |req| req.headers[\"Authorization\"] == \"Bearer test-key\" }})").unwrap();
            writeln!(out, "  end").unwrap();
            writeln!(out, "end").unwrap();
        }
        Auth::ApiKeyHeader(header) => {
            writeln!(out, "\nRSpec.describe \"authentication\" do").unwrap();
            writeln!(out, "  let(:client) {{ build_client }}").unwrap();
            writeln!(
                out,
                "  it \"sends the credential header on every request\" do"
            )
            .unwrap();
            writeln!(out, "    stub = {stub_line}.to_return({get_response})").unwrap();
            writeln!(out, "    {get_call}").unwrap();
            writeln!(out, "    expect(WebMock).to(have_requested(:get, %r{{.*}}).with {{ |req| req.headers[\"{header}\"] == \"test-key\" }})").unwrap();
            writeln!(out, "  end").unwrap();
            writeln!(out, "end").unwrap();
        }
        Auth::Basic => {
            writeln!(out, "\nRSpec.describe \"authentication\" do").unwrap();
            writeln!(out, "  let(:client) {{ build_client }}").unwrap();
            writeln!(
                out,
                "  it \"sends the HTTP Basic credential on every request\" do"
            )
            .unwrap();
            writeln!(out, "    stub = {stub_line}.to_return({get_response})").unwrap();
            writeln!(out, "    {get_call}").unwrap();
            writeln!(out, "    expect(WebMock).to(have_requested(:get, %r{{.*}}).with {{ |req| req.headers[\"Authorization\"] == \"Basic dGVzdC11c2VyOnRlc3QtcGFzc3dvcmQ=\" }})").unwrap();
            writeln!(out, "  end").unwrap();
            writeln!(out, "end").unwrap();
        }
        Auth::None => {}
    }

    // ---- error mapping ----
    writeln!(out, "\nRSpec.describe \"error mapping\" do").unwrap();
    writeln!(out, "  let(:client) {{ build_client }}").unwrap();
    writeln!(
        out,
        "\n  context \"when the API answers with an rpc status\" do"
    )
    .unwrap();
    writeln!(
        out,
        "    it \"raises APIError carrying status, code, and message\" do"
    )
    .unwrap();
    writeln!(out, "      {stub_line}.to_return(status: 404, body: '{{\"code\":5,\"message\":\"missing\",\"details\":[]}}', headers: {{ \"Content-Type\" => \"application/json\" }})").unwrap();
    writeln!(
        out,
        "      expect {{ {get_call} }}.to raise_error({module}::APIError) do |e|"
    )
    .unwrap();
    writeln!(out, "        expect(e.status_code).to eq(404)").unwrap();
    writeln!(out, "        expect(e.code).to eq(5)").unwrap();
    writeln!(out, "        expect(e.message).to eq(\"missing\")").unwrap();
    writeln!(out, "      end").unwrap();
    writeln!(out, "    end").unwrap();
    writeln!(out, "  end").unwrap();
    for (label, ret) in [
        (
            "the success body is empty where JSON is promised",
            "status: 200, body: \"\"",
        ),
        (
            "the success body is a JSON null",
            "status: 200, body: \"null\", headers: { \"Content-Type\" => \"application/json\" }",
        ),
        (
            "the success body is not JSON",
            "status: 200, body: \"<html>\", headers: { \"Content-Type\" => \"text/html\" }",
        ),
        (
            "an unfollowed redirect arrives",
            "status: 302, body: \"\", headers: { \"Location\" => \"https://elsewhere.example\" }",
        ),
    ] {
        writeln!(out, "\n  context \"when {label}\" do").unwrap();
        writeln!(out, "    it \"raises the stable protocol error\" do").unwrap();
        writeln!(out, "      {stub_line}.to_return({ret})").unwrap();
        writeln!(
            out,
            "      expect {{ {get_call} }}.to raise_error({module}::APIResponseError)"
        )
        .unwrap();
        writeln!(out, "    end").unwrap();
        writeln!(out, "  end").unwrap();
    }
    writeln!(out, "end").unwrap();

    // ---- retries ----
    writeln!(out, "\nRSpec.describe \"retries\" do").unwrap();
    writeln!(
        out,
        "  before {{ allow_any_instance_of({module}::Core).to receive(:sleep) }}"
    )
    .unwrap();
    writeln!(
        out,
        "\n  context \"when an idempotent request hits a retryable status\" do"
    )
    .unwrap();
    writeln!(
        out,
        "    it \"retries up to the client budget and then succeeds\" do"
    )
    .unwrap();
    writeln!(out, "      client = build_client(max_retries: 2)").unwrap();
    writeln!(out, "      stub = {stub_line}").unwrap();
    writeln!(
        out,
        "        .to_return({{ status: 503, body: \"\" }}, {{ {get_response} }})"
    )
    .unwrap();
    writeln!(out, "      {get_call}").unwrap();
    writeln!(out, "      expect(stub).to have_been_requested.times(2)").unwrap();
    writeln!(out, "    end").unwrap();
    writeln!(out, "  end").unwrap();
    if let Some((m_resource, m_op)) = anchors.mutation {
        let m_call = call_expr(api, m_resource, m_op, false);
        let m_path = sample_path(m_op);
        let m_http = m_op.http_method.as_str().to_lowercase();
        let m_response = default_stub_response(api, m_op);
        writeln!(
            out,
            "\n  context \"when a mutation hits a retryable status\" do"
        )
        .unwrap();
        writeln!(out, "    it \"does NOT retry by default\" do").unwrap();
        writeln!(out, "      client = build_client(max_retries: 2)").unwrap();
        writeln!(out, "      stub = stub_request(:{m_http}, \"#{{SPEC_BASE_URL}}{m_path}\").to_return(status: 503, body: \"\")").unwrap();
        writeln!(
            out,
            "      expect {{ {m_call} }}.to raise_error({module}::APIError)"
        )
        .unwrap();
        writeln!(out, "      expect(stub).to have_been_requested.times(1)").unwrap();
        writeln!(out, "    end").unwrap();
        writeln!(
            out,
            "\n    it \"retries when the CALLER opts this call in via request_options\" do"
        )
        .unwrap();
        writeln!(out, "      client = build_client").unwrap();
        writeln!(
            out,
            "      stub = stub_request(:{m_http}, \"#{{SPEC_BASE_URL}}{m_path}\")"
        )
        .unwrap();
        writeln!(
            out,
            "        .to_return({{ status: 503, body: \"\" }}, {{ {m_response} }})"
        )
        .unwrap();
        let m_call_opts = m_call.trim_end_matches(')');
        writeln!(
            out,
            "      {m_call_opts}, request_options: {{ max_retries: 2 }})"
        )
        .unwrap();
        writeln!(out, "      expect(stub).to have_been_requested.times(2)").unwrap();
        writeln!(out, "    end").unwrap();
        writeln!(out, "  end").unwrap();
    }
    writeln!(out, "end").unwrap();

    // ---- request options ----
    writeln!(out, "\nRSpec.describe \"request options\" do").unwrap();
    writeln!(out, "  let(:client) {{ build_client }}").unwrap();
    writeln!(
        out,
        "\n  context \"when an unknown option key is supplied\" do"
    )
    .unwrap();
    writeln!(out, "    it \"is rejected loudly before any request\" do").unwrap();
    let opt_call = get_call.trim_end_matches(')');
    let (opt_open, opt_close) = if get_call.ends_with(')') {
        (format!("{opt_call}, "), ")")
    } else {
        (format!("{get_call}("), ")")
    };
    writeln!(out, "      expect {{ {opt_open}request_options: {{ timout: 5 }}{opt_close} }}.to raise_error(ArgumentError, /unknown request_options key/)").unwrap();
    writeln!(
        out,
        "      expect(WebMock).not_to have_requested(:get, %r{{.*}})"
    )
    .unwrap();
    writeln!(out, "    end").unwrap();
    writeln!(out, "  end").unwrap();
    writeln!(
        out,
        "\n  context \"when per-request headers are supplied\" do"
    )
    .unwrap();
    writeln!(
        out,
        "    it \"sends them, replacing defaults case-insensitively\" do"
    )
    .unwrap();
    writeln!(out, "      {stub_line}.to_return({get_response})").unwrap();
    writeln!(out, "      {opt_open}request_options: {{ headers: {{ \"X-Request-ID\" => \"rid-1\", \"user-agent\" => \"custom-ua\" }} }}{opt_close}").unwrap();
    writeln!(out, "      expect(WebMock).to(have_requested(:get, %r{{.*}}).with {{ |req| req.headers[\"X-Request-Id\"] == \"rid-1\" && req.headers[\"User-Agent\"] == \"custom-ua\" }})").unwrap();
    writeln!(out, "    end").unwrap();
    writeln!(out, "  end").unwrap();
    for (label, bad) in [
        ("a negative timeout", "{ timeout: -1 }"),
        ("a non-numeric timeout", "{ timeout: \"soon\" }"),
        ("a fractional retry count", "{ max_retries: 1.5 }"),
    ] {
        writeln!(out, "\n  context \"when {label} is supplied\" do").unwrap();
        writeln!(out, "    it \"is rejected\" do").unwrap();
        writeln!(out, "      expect {{ {opt_open}request_options: {bad}{opt_close} }}.to raise_error(ArgumentError)").unwrap();
        writeln!(out, "    end").unwrap();
        writeln!(out, "  end").unwrap();
    }
    writeln!(out, "end").unwrap();

    // ---- request encoding ----
    if let Some((m_resource, m_op)) = anchors.mutation {
        let m_call = call_expr(api, m_resource, m_op, false);
        let m_path = sample_path(m_op);
        let m_http = m_op.http_method.as_str().to_lowercase();
        let m_response = default_stub_response(api, m_op);
        let golden = request_body_value(api, m_op).expect("mutation has a body");
        writeln!(out, "\nRSpec.describe \"request encoding\" do").unwrap();
        writeln!(out, "  let(:client) {{ build_client }}").unwrap();
        writeln!(
            out,
            "  it \"translates snake_case keys to the exact golden wire body\" do"
        )
        .unwrap();
        writeln!(
            out,
            "    {}",
            format!(
                "stub_request(:{m_http}, \"#{{SPEC_BASE_URL}}{m_path}\").to_return({m_response})"
            )
        )
        .unwrap();
        writeln!(out, "    {m_call}").unwrap();
        writeln!(out, "    expect(WebMock).to(have_requested(:{m_http}, %r{{.*}}).with {{ |req| JSON.parse(req.body) == JSON.parse({golden_literal}) }})", golden_literal = rb_literal(&json!(golden.to_string()))).unwrap();
        writeln!(out, "  end").unwrap();
        if let Some((ro_resource, ro_op, ro_field, _ty, ro_wires)) = &anchors.read_only {
            let snake_field = rb_name(&ro_field.wire_name);
            let base_sample = snake_sample(api, &ro_field.ty, manifest_sample(api, &ro_field.ty));
            let mut with_ro = base_sample.clone();
            if let Value::Object(map) = &mut with_ro {
                for wire in ro_wires.iter() {
                    map.insert(rb_name(wire), json!("server-owned"));
                }
            }
            let ro_http = ro_op.http_method.as_str().to_lowercase();
            let ro_path = sample_path(ro_op);
            let ro_response = default_stub_response(api, ro_op);
            // Rebuild the call with the readOnly-polluted field value.
            let mut args = call_args(api, ro_op, false);
            for arg in &mut args {
                if arg.starts_with(&format!("{snake_field}: ")) {
                    *arg = format!("{snake_field}: {}", rb_literal(&with_ro));
                }
            }
            let accessor = format!("client.{}", ro_resource.path());
            let ro_call = format!("{accessor}.{}({})", rb_name(&ro_op.name), args.join(", "));
            writeln!(
                out,
                "\n  context \"when a fetched value still carries server-owned fields\" do"
            )
            .unwrap();
            writeln!(
                out,
                "    it \"drops readOnly keys instead of echoing server state\" do"
            )
            .unwrap();
            writeln!(out, "      stub_request(:{ro_http}, \"#{{SPEC_BASE_URL}}{ro_path}\").to_return({ro_response})").unwrap();
            writeln!(out, "      {ro_call}").unwrap();
            writeln!(
                out,
                "      expect(WebMock).to(have_requested(:{ro_http}, %r{{.*}}).with do |req|"
            )
            .unwrap();
            writeln!(
                out,
                "        sent = JSON.parse(req.body).fetch(\"{}\", {{}})",
                ro_field.wire_name
            )
            .unwrap();
            for wire in ro_wires.iter() {
                writeln!(out, "        !sent.key?(\"{wire}\") &&").unwrap();
            }
            writeln!(out, "          true").unwrap();
            writeln!(out, "      end)").unwrap();
            writeln!(out, "    end").unwrap();
            writeln!(out, "  end").unwrap();
        }
        writeln!(out, "end").unwrap();
    }

    out
}

// ---- webhooks ---------------------------------------------------------------

fn webhooks_spec(api: &Api, module: &str) -> String {
    let payload = api
        .webhooks
        .first()
        .map(|w| manifest_sample_output(api, &w.payload).to_string())
        .unwrap_or_else(|| "{}".to_string());
    format!(
        r##"# frozen_string_literal: true

# Code generated by redwood. DO NOT EDIT.

require "base64"
require "openssl"

RSpec.describe "webhook verification" do
  let(:secret_key) {{ "0123456789abcdef0123456789abcdef" }}
  let(:secret) {{ "whsec_#{{Base64.strict_encode64(secret_key)}}" }}
  let(:payload) {{ {payload_literal} }}

  def signed_headers(payload, at: Time.now.to_i, key: secret_key, msg_id: "msg_1")
    signature = Base64.strict_encode64(OpenSSL::HMAC.digest("SHA256", key, "#{{msg_id}}.#{{at}}.#{{payload}}"))
    {{
      "webhook-id" => msg_id,
      "webhook-timestamp" => at.to_s,
      "webhook-signature" => "v1,#{{signature}}",
    }}
  end

  context "with an authentic signature" do
    it "verifies" do
      expect {{ {module}::Webhooks.verify(secret, payload, signed_headers(payload)) }}.not_to raise_error
    end
  end

  context "when the payload was tampered with" do
    it "rejects" do
      headers = signed_headers(payload)
      expect {{ {module}::Webhooks.verify(secret, payload + " ", headers) }}
        .to raise_error({module}::WebhookVerificationError)
    end
  end

  context "with the wrong secret" do
    it "rejects" do
      other = "whsec_#{{Base64.strict_encode64("fedcba9876543210fedcba9876543210")}}"
      expect {{ {module}::Webhooks.verify(other, payload, signed_headers(payload)) }}
        .to raise_error({module}::WebhookVerificationError)
    end
  end

  context "when the timestamp is outside the tolerance" do
    it "rejects the replay" do
      headers = signed_headers(payload, at: Time.now.to_i - 3600)
      expect {{ {module}::Webhooks.verify(secret, payload, headers) }}
        .to raise_error({module}::WebhookVerificationError, /tolerance/)
    end
  end

  context "when a signature header is missing" do
    it "rejects" do
      headers = signed_headers(payload).reject {{ |k, _| k == "webhook-signature" }}
      expect {{ {module}::Webhooks.verify(secret, payload, headers) }}
        .to raise_error({module}::WebhookVerificationError)
    end
  end
end
"##,
        payload_literal = rb_literal(&json!(payload)),
    )
}
