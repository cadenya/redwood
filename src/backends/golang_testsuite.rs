//! Generated Go test suite for the SDK.
//!
//! Mirrors the signed-off Ruby RSpec design with Go idioms: testify
//! suite/assert/require, external `_test` packages for isolation, stdlib
//! `httptest` for request recording, and golden files in `testdata/golden/`
//! playing the cassette role — one JSON document per operation holding the
//! expected request (method, path, query, parsed body) and the canned
//! response. The golden server replays interactions in order and fails the
//! test when the incoming request drifts. Everything derives structurally
//! from the IR — no spec-specific names in this module.

use serde_json::{json, Value};
use std::fmt::Write as _;

use super::golang::{client_param, go_name, params_type_name};
use super::openapi_export::{go_ty_label, go_value};
use super::{manifest_sample_output, FileSet};
use crate::ir::*;

pub(crate) fn emit(api: &Api, pkg: &str, module: &str, files: &mut FileSet) {
    let finish = |body: String| body.replace("MODULE_PATH", module);
    for resource in &api.resources {
        files.insert(
            format!("resource_{}_test.go", resource.ident),
            finish(resource_test(api, resource, pkg)),
        );
        for op in &resource.operations {
            files.insert(
                format!("testdata/golden/{}.json", op.id),
                golden_file(api, op),
            );
        }
    }
    files.insert("suite_test.go".into(), finish(harness(api, pkg)));
    files.insert("behavior_test.go".into(), finish(behavior_test(api, pkg)));
    if !api.webhooks.is_empty() {
        files.insert("webhooks_test.go".into(), finish(webhooks_test(api, pkg)));
    }
}

// ---- golden files -----------------------------------------------------------

/// A housekeeping frame interleaved into SSE goldens when the API
/// configures [sse] skip_events — non-JSON data makes a failed skip loud.
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
        Value::Number(n) if n.as_f64() == Some(1.0) => "1".to_string(),
        other => other.to_string(),
    }
}

/// Query as url.Values-shaped map (key -> ordered value list).
fn golden_query(api: &Api, op: &Operation, cursor_override: Option<(&str, &str)>) -> Value {
    let mut map = serde_json::Map::new();
    for p in &op.query_params {
        let sample = wire_sample_go(api, &p.ty, 0);
        let values: Vec<Value> = match sample {
            Value::Array(items) => items.iter().map(|i| json!(scalar_str(i))).collect(),
            Value::Null => continue,
            other => vec![json!(scalar_str(&other))],
        };
        let values = match cursor_override {
            Some((param, replacement)) if p.wire_name == param => vec![json!(replacement)],
            _ => values,
        };
        map.insert(p.wire_name.clone(), Value::Array(values));
    }
    Value::Object(map)
}

/// Wire sample MIRRORING what the generated Go code actually sends: the
/// same value choices as go_value (enums pick a non-UNSPECIFIED member)
/// and the same field materialization as encoding/json (an optional
/// empty-map field is dropped by omitempty).
fn wire_sample_go(api: &Api, ty: &Ty, depth: usize) -> Value {
    const MAX_DEPTH: usize = 8;
    match ty {
        Ty::String => json!("sample"),
        Ty::Bool => json!(true),
        Ty::Int32 | Ty::Int64 => json!(1),
        Ty::Float | Ty::Double => json!(1.0),
        Ty::Timestamp => json!("2026-01-01T00:00:00Z"),
        Ty::Bytes => json!("sample"),
        Ty::Json => json!({}),
        Ty::Literal(v) => json!(v),
        Ty::Map(_) => json!({}),
        Ty::List(inner) => {
            if depth > MAX_DEPTH {
                json!([])
            } else {
                json!([wire_sample_go(api, inner, depth + 1)])
            }
        }
        Ty::Named(n) => match api.types.get(n).map(|d| &d.shape) {
            Some(Shape::Struct(st)) => {
                if depth > MAX_DEPTH {
                    return json!({});
                }
                let mut map = serde_json::Map::new();
                for f in st.input_fields().filter(|f| f.required && !f.nullable) {
                    let value = wire_sample_go(api, &f.ty, depth + 1);
                    map.insert(f.wire_name.clone(), value);
                }
                Value::Object(map)
            }
            Some(Shape::Enum(e)) => {
                let value = e
                    .values
                    .iter()
                    .find(|v| !v.ends_with("UNSPECIFIED"))
                    .or_else(|| e.values.first())
                    .cloned()
                    .unwrap_or_default();
                json!(value)
            }
            Some(Shape::Union(u)) => {
                if depth > MAX_DEPTH {
                    return json!({});
                }
                u.variants
                    .first()
                    .map(|v| wire_sample_go(api, &v.ty, depth + 1))
                    .unwrap_or(json!({}))
            }
            Some(Shape::Alias(inner)) => {
                let inner = inner.clone();
                wire_sample_go(api, &inner, depth + 1)
            }
            None => json!({}),
        },
    }
}

fn request_body_value(api: &Api, op: &Operation) -> Option<Value> {
    if !op.body_fields.is_empty() {
        let mut obj = serde_json::Map::new();
        for f in &op.body_fields {
            // The generated body materialization is nil-check based and the
            // builder sets EVERY field, so every sampled field reaches the
            // wire — including empty maps and empty structs.
            obj.insert(f.wire_name.clone(), wire_sample_go(api, &f.ty, 0));
        }
        return Some(Value::Object(obj));
    }
    op.whole_body.as_ref().map(|ty| wire_sample_go(api, ty, 0))
}

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

fn golden_file(api: &Api, op: &Operation) -> String {
    let path = sample_path(op);
    let body = request_body_value(api, op);
    let method = op.http_method.as_str();
    let interaction = |query: Value, status: u16, content_type: &str, response_body: String| {
        json!({
            "request": {
                "method": method,
                "path": path,
                "query": query,
                "body": body,
            },
            "response": {
                "status": status,
                "contentType": content_type,
                "body": response_body,
            },
        })
    };
    let interactions: Vec<Value> = match (&op.pagination, &op.response) {
        (Some(page), ResponseKind::Json(ty)) => {
            let make_page = |cursor: &str| -> String {
                let mut body = manifest_sample_output(api, ty);
                set_in(
                    &mut body,
                    &page.items_field,
                    json!([manifest_sample_output(api, &page.item_ty)]),
                );
                set_in(&mut body, &page.next_cursor_path, json!(cursor));
                body.to_string()
            };
            vec![
                interaction(
                    golden_query(api, op, None),
                    200,
                    "application/json",
                    make_page("cursor_page_2"),
                ),
                interaction(
                    golden_query(api, op, Some((page.cursor_param.as_str(), "cursor_page_2"))),
                    200,
                    "application/json",
                    make_page(""),
                ),
            ]
        }
        (None, ResponseKind::Json(ty)) => vec![interaction(
            golden_query(api, op, None),
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
                golden_query(api, op, None),
                200,
                "text/event-stream",
                sse,
            )]
        }
        (None, ResponseKind::Empty) => vec![interaction(
            golden_query(api, op, None),
            204,
            "application/json",
            String::new(),
        )],
        (Some(_), _) => unreachable!("pagination implies JSON"),
    };
    serde_json::to_string_pretty(&json!({ "interactions": interactions })).unwrap() + "\n"
}

// ---- call construction ------------------------------------------------------

fn accessor(resource: &Resource) -> String {
    resource
        .path()
        .split('.')
        .map(go_name)
        .map(|s| format!("{s}()"))
        .collect::<Vec<_>>()
        .join(".")
}

fn builder_name(resource: &Resource, op: &Operation) -> String {
    let params = params_type_name(resource, op);
    params
        .strip_suffix("Params")
        .map(|b| format!("{b}Builder"))
        .unwrap_or_else(|| format!("{params}Builder"))
}

/// Builder-setter argument for a field value sampled from the IR. Unlike
/// the doc-sample renderer, the suite sets OPTIONAL fields too, so typed
/// timestamps need a real time.Time literal (matching the sampler value).
fn setter_arg(api: &Api, ty: &Ty, pkg: &str) -> String {
    match ty {
        Ty::Timestamp => "time.Date(2026, time.January, 1, 0, 0, 0, 0, time.UTC)".to_string(),
        Ty::Map(inner) => format!("map[string]{}{{}}", go_ty_label(api, inner, pkg)),
        Ty::List(inner) => {
            // Variadic setter: pass the single sampled element by value.
            let element = setter_arg(api, inner, pkg);
            element.strip_prefix('&').unwrap_or(&element).to_string()
        }
        _ => go_value(api, ty, pkg),
    }
}

/// The params expression via the fluent builder, or None when the op has no
/// params. `skip_client_params` leaves client defaults to resolve.
fn params_expr(
    api: &Api,
    resource: &Resource,
    op: &Operation,
    pkg: &str,
    skip_client_params: bool,
) -> Option<String> {
    if !op.has_params() {
        return None;
    }
    let mut chain = format!("(&{pkg}.{}{{}})", builder_name(resource, op));
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        if skip_client_params && client_param(api, &p.wire_name).is_some() {
            continue;
        }
        write!(
            chain,
            ".\n\t\t{}({})",
            go_name(&p.wire_name),
            setter_arg(api, &p.ty, pkg)
        )
        .unwrap();
    }
    for f in &op.body_fields {
        write!(
            chain,
            ".\n\t\t{}({})",
            go_name(&f.wire_name),
            setter_arg(api, &f.ty, pkg)
        )
        .unwrap();
    }
    if let Some(ty) = &op.whole_body {
        write!(chain, ".\n\t\tBody({})", setter_arg(api, ty, pkg)).unwrap();
    }
    chain.push_str(".\n\t\tToParams()");
    Some(chain)
}

fn call_expr(
    api: &Api,
    resource: &Resource,
    op: &Operation,
    pkg: &str,
    skip_client_params: bool,
) -> String {
    let mut args = vec!["ctx".to_string()];
    for _ in &op.positionals {
        args.push("\"sample\"".into());
    }
    if let Some(params) = params_expr(api, resource, op, pkg, skip_client_params) {
        args.push(params);
    }
    format!(
        "client.{}.{}({})",
        accessor(resource),
        go_name(&op.name),
        args.join(", ")
    )
}

// ---- shared harness ---------------------------------------------------------

fn harness(api: &Api, pkg: &str) -> String {
    let mut client_opts: Vec<String> = vec![format!("{pkg}.WithBaseURL(baseURL)")];
    match &api.auth {
        Auth::None => {}
        _ => client_opts.push(format!("{pkg}.WithAPIKey(\"test-key\")")),
    }
    for c in &api.client_params {
        let snake = heck::ToSnakeCase::to_snake_case(c.wire_name.as_str());
        client_opts.push(format!(
            "{pkg}.With{}(\"default_{snake}\")",
            go_name(&c.wire_name)
        ));
    }
    format!(
        r#"// Code generated by redwood. DO NOT EDIT.
package {pkg}_test

import (
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"sync/atomic"
	"testing"

	"github.com/stretchr/testify/assert"
	"github.com/stretchr/testify/require"

	{pkg} "{module}"
)

type goldenRequest struct {{
	Method string              `json:"method"`
	Path   string              `json:"path"`
	Query  map[string][]string `json:"query"`
	Body   json.RawMessage     `json:"body"`
}}

type goldenResponse struct {{
	Status      int    `json:"status"`
	ContentType string `json:"contentType"`
	Body        string `json:"body"`
}}

type goldenInteraction struct {{
	Request  goldenRequest  `json:"request"`
	Response goldenResponse `json:"response"`
}}

type golden struct {{
	Interactions []goldenInteraction `json:"interactions"`
}}

func loadGolden(t *testing.T, id string) golden {{
	t.Helper()
	raw, err := os.ReadFile(filepath.Join("testdata", "golden", id+".json"))
	require.NoError(t, err, "golden file for %s", id)
	var g golden
	require.NoError(t, json.Unmarshal(raw, &g))
	require.NotEmpty(t, g.Interactions)
	return g
}}

// goldenServer replays the golden interactions in order and fails the test
// when the incoming request drifts from the recorded method, path, query,
// or parsed JSON body. Assertions in the handler goroutine use assert (safe
// Errorf semantics); the caller checks the final count with require.
func goldenServer(t *testing.T, g golden) (*httptest.Server, *atomic.Int32) {{
	t.Helper()
	var served atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{
		index := int(served.Add(1)) - 1
		if !assert.Less(t, index, len(g.Interactions), "more requests than golden interactions") {{
			w.WriteHeader(http.StatusInternalServerError)
			return
		}}
		want := g.Interactions[index]
		assert.Equal(t, want.Request.Method, r.Method)
		assert.Equal(t, want.Request.Path, r.URL.Path)
		gotQuery := map[string][]string(r.URL.Query())
		wantQuery := want.Request.Query
		if wantQuery == nil {{
			wantQuery = map[string][]string{{}}
		}}
		if len(gotQuery) != 0 || len(wantQuery) != 0 {{
			assert.True(t, reflect.DeepEqual(wantQuery, gotQuery), "query drifted: want %v, got %v", wantQuery, gotQuery)
		}}
		gotBody := readAll(t, r)
		if len(want.Request.Body) == 0 || string(want.Request.Body) == "null" {{
			assert.Empty(t, gotBody, "unexpected request body")
		}} else {{
			var wantParsed, gotParsed any
			require.NoError(t, json.Unmarshal(want.Request.Body, &wantParsed))
			if assert.NoError(t, json.Unmarshal(gotBody, &gotParsed), "request body is not JSON") {{
				assert.True(t, reflect.DeepEqual(wantParsed, gotParsed), "body drifted: want %s, got %s", want.Request.Body, gotBody)
			}}
		}}
		w.Header().Set("Content-Type", want.Response.ContentType)
		w.WriteHeader(want.Response.Status)
		_, _ = w.Write([]byte(want.Response.Body))
	}}))
	t.Cleanup(server.Close)
	return server, &served
}}

func readAll(t *testing.T, r *http.Request) []byte {{
	t.Helper()
	if r.Body == nil {{
		return nil
	}}
	defer r.Body.Close()
	buf := make([]byte, 0, 1024)
	tmp := make([]byte, 1024)
	for {{
		n, err := r.Body.Read(tmp)
		buf = append(buf, tmp[:n]...)
		if err != nil {{
			break
		}}
	}}
	return buf
}}

func newTestClient(t *testing.T, baseURL string, extra ...{pkg}.Option) *{pkg}.Client {{
	t.Helper()
	opts := []{pkg}.Option{{
		{client_opts},
	}}
	opts = append(opts, extra...)
	client, err := {pkg}.NewClient(opts...)
	require.NoError(t, err)
	return client
}}
"#,
        pkg = pkg,
        module = "MODULE_PATH",
        client_opts = client_opts.join(",\n\t\t"),
    )
}

// ---- per-resource tests -----------------------------------------------------

fn resource_test(api: &Api, resource: &Resource, pkg: &str) -> String {
    let suite_name = format!(
        "{}Suite",
        heck::ToUpperCamelCase::to_upper_camel_case(resource.ident.as_str())
    );
    let mut out = format!(
        r#"// Code generated by redwood. DO NOT EDIT.
package {pkg}_test

import (
	"context"
	"testing"
TIME_IMPORT
	"github.com/stretchr/testify/suite"
PKG_IMPORT)

type {suite_name} struct {{
	suite.Suite
}}

func Test{suite_name}(t *testing.T) {{
	suite.Run(t, new({suite_name}))
}}
"#,
    );
    for op in &resource.operations {
        let test_name = go_name(&op.name);
        let call = call_expr(api, resource, op, pkg, false);
        writeln!(out, "\nfunc (s *{suite_name}) Test{test_name}() {{").unwrap();
        writeln!(out, "\tg := loadGolden(s.T(), \"{}\")", op.id).unwrap();
        writeln!(out, "\tserver, served := goldenServer(s.T(), g)").unwrap();
        writeln!(out, "\tclient := newTestClient(s.T(), server.URL)").unwrap();
        writeln!(out, "\tctx := context.Background()").unwrap();
        emit_invocation(&mut out, op, &call, "\t");
        writeln!(
            out,
            "\ts.Require().Equal(int32(len(g.Interactions)), served.Load(), \"request count\")"
        )
        .unwrap();

        // Client-default permutation.
        let defaults: Vec<&ClientParam> = op
            .path_params
            .iter()
            .chain(op.query_params.iter())
            .filter_map(|p| client_param(api, &p.wire_name))
            .collect();
        if let Some(c) = defaults.first() {
            let snake = heck::ToSnakeCase::to_snake_case(c.wire_name.as_str());
            let is_path = op.path.contains(&format!("{{{}}}", c.wire_name));
            let default_call = call_expr(api, resource, op, pkg, true);
            writeln!(
                out,
                "\n\ts.Run(\"{} falls back to the client default\", func() {{",
                snake
            )
            .unwrap();
            if is_path {
                let mut expected = op.path.clone();
                expected =
                    expected.replace(&format!("{{{}}}", c.wire_name), &format!("default_{snake}"));
                let mut path = String::new();
                let mut rest = expected.as_str();
                while let Some(start) = rest.find('{') {
                    let end = rest[start..]
                        .find('}')
                        .map(|e| start + e)
                        .expect("balanced");
                    path.push_str(&rest[..start]);
                    path.push_str("sample");
                    rest = &rest[end + 1..];
                }
                path.push_str(rest);
                writeln!(
                    out,
                    "\t\tserver, hits := pathServer(s.T(), \"{path}\", g.Interactions[0].Response)"
                )
                .unwrap();
            } else {
                writeln!(out, "\t\tserver, hits := queryServer(s.T(), \"{}\", \"default_{snake}\", g.Interactions[0].Response)", c.wire_name).unwrap();
            }
            writeln!(out, "\t\tclient := newTestClient(s.T(), server.URL)").unwrap();
            writeln!(out, "\t\tctx := context.Background()").unwrap();
            emit_default_invocation(&mut out, op, &default_call, "\t\t");
            writeln!(out, "\t\ts.Require().Equal(int32(1), hits.Load())").unwrap();
            writeln!(out, "\t}})").unwrap();
        }
        writeln!(out, "}}").unwrap();
    }
    let time_import = if out.contains("time.Date(") {
        "\t\"time\"\n"
    } else {
        ""
    };
    out = out.replace("TIME_IMPORT\n", time_import);
    // The SDK import is only needed when a builder or typed value appears.
    let pkg_import = if out.contains(&format!("{pkg}.")) {
        format!("\n\t{pkg} \"MODULE_PATH\"\n")
    } else {
        String::new()
    };
    out = out.replace("PKG_IMPORT", &pkg_import);
    out
}

/// The golden invocation plus response-kind assertions.
fn emit_invocation(out: &mut String, op: &Operation, call: &str, indent: &str) {
    match (&op.pagination, &op.response) {
        (Some(_), _) => {
            writeln!(out, "{indent}page, err := {call}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(err)").unwrap();
            writeln!(out, "{indent}items, err := page.All(ctx)").unwrap();
            writeln!(out, "{indent}s.Require().NoError(err)").unwrap();
            writeln!(out, "{indent}s.Require().Len(items, 2)").unwrap();
        }
        (None, ResponseKind::Json(_)) => {
            writeln!(out, "{indent}result, err := {call}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(err)").unwrap();
            writeln!(out, "{indent}s.Require().NotNil(result)").unwrap();
        }
        (None, ResponseKind::Sse(_)) => {
            writeln!(out, "{indent}stream, err := {call}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(err)").unwrap();
            writeln!(out, "{indent}count := 0").unwrap();
            writeln!(out, "{indent}for stream.Next() {{").unwrap();
            writeln!(out, "{indent}\tcount++").unwrap();
            writeln!(out, "{indent}}}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(stream.Err())").unwrap();
            writeln!(out, "{indent}s.Require().Equal(2, count)").unwrap();
            writeln!(
                out,
                "{indent}s.Require().Equal(\"e2\", stream.LastEventID())"
            )
            .unwrap();
        }
        (None, ResponseKind::Empty) => {
            writeln!(out, "{indent}err := {call}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(err)").unwrap();
        }
    }
}

/// The client-default invocation only asserts routing, not deep decoding.
fn emit_default_invocation(out: &mut String, op: &Operation, call: &str, indent: &str) {
    match (&op.pagination, &op.response) {
        (Some(_), _) | (None, ResponseKind::Json(_)) => {
            writeln!(out, "{indent}_, err := {call}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(err)").unwrap();
        }
        (None, ResponseKind::Sse(_)) => {
            writeln!(out, "{indent}stream, err := {call}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(err)").unwrap();
            writeln!(out, "{indent}for stream.Next() {{").unwrap();
            writeln!(out, "{indent}}}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(stream.Err())").unwrap();
        }
        (None, ResponseKind::Empty) => {
            writeln!(out, "{indent}err := {call}").unwrap();
            writeln!(out, "{indent}s.Require().NoError(err)").unwrap();
        }
    }
}

// ---- behavioral tests -------------------------------------------------------

struct Anchors<'a> {
    simple_get: Option<(&'a Resource, &'a Operation)>,
    mutation: Option<(&'a Resource, &'a Operation)>,
    stream: Option<(&'a Resource, &'a Operation)>,
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
    let stream =
        ops().find(|(_, o)| o.pagination.is_none() && matches!(o.response, ResponseKind::Sse(_)));
    Anchors {
        simple_get,
        mutation,
        stream,
    }
}

fn behavior_test(api: &Api, pkg: &str) -> String {
    let anchors = find_anchors(api);
    let mut out = format!(
        r#"// Code generated by redwood. DO NOT EDIT.
// Cross-cutting runtime contracts, anchored on operations chosen by shape.
package {pkg}_test

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	{pkg} "MODULE_PATH"
)

// pathServer accepts any request for the expected path and replies with the
// canned response; used by client-default routing contexts.
func pathServer(t *testing.T, path string, response goldenResponse) (*httptest.Server, *atomic.Int32) {{
	t.Helper()
	var hits atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{
		hits.Add(1)
		require.Equal(t, path, r.URL.Path)
		w.Header().Set("Content-Type", response.ContentType)
		w.WriteHeader(response.Status)
		_, _ = w.Write([]byte(response.Body))
	}}))
	t.Cleanup(server.Close)
	return server, &hits
}}

// queryServer asserts one query parameter's resolved value.
func queryServer(t *testing.T, param, want string, response goldenResponse) (*httptest.Server, *atomic.Int32) {{
	t.Helper()
	var hits atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{
		hits.Add(1)
		require.Equal(t, want, r.URL.Query().Get(param))
		w.Header().Set("Content-Type", response.ContentType)
		w.WriteHeader(response.Status)
		_, _ = w.Write([]byte(response.Body))
	}}))
	t.Cleanup(server.Close)
	return server, &hits
}}

func TestClientConstruction(t *testing.T) {{
	for _, bad := range []string{{
		"https://host.example?x=1",
		"https://host.example#frag",
		"https://user:pw@host.example",
		"https://host.example?",
		"https://host.example#",
		"ftp://host.example",
		"https://",
	}} {{
		t.Run(bad, func(t *testing.T) {{
			_, err := {pkg}.NewClient({ctor_opts}{pkg}.WithBaseURL(bad))
			require.Error(t, err, "base URL %q must be rejected before any request", bad)
		}})
	}}
}}
"#,
        pkg = pkg,
        ctor_opts = match &api.auth {
            Auth::None => String::new(),
            _ => format!("{pkg}.WithAPIKey(\"test-key\"), "),
        },
    );

    let Some((resource, op)) = anchors.simple_get else {
        return out;
    };
    let get_call = call_expr(api, resource, op, pkg, false);
    let go_get_call = |varname: &str| get_call.replacen("client.", &format!("{varname}."), 1);

    // A tiny fixed-response server factory reused by the error/retry tests.
    writeln!(
        out,
        r#"
type cannedResponse struct {{
	status      int
	contentType string
	body        string
	retryAfter  string
}}

// sequenceServer replies with each canned response in turn (repeating the
// last), records request headers, and counts hits.
func sequenceServer(t *testing.T, responses ...cannedResponse) (*httptest.Server, *atomic.Int32, *http.Header) {{
	t.Helper()
	var hits atomic.Int32
	var lastHeaders http.Header
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{
		index := int(hits.Add(1)) - 1
		if index >= len(responses) {{
			index = len(responses) - 1
		}}
		lastHeaders = r.Header.Clone()
		response := responses[index]
		if response.retryAfter != "" {{
			w.Header().Set("Retry-After", response.retryAfter)
		}}
		if response.contentType != "" {{
			w.Header().Set("Content-Type", response.contentType)
		}}
		w.WriteHeader(response.status)
		_, _ = w.Write([]byte(response.body))
	}}))
	t.Cleanup(server.Close)
	return server, &hits, &lastHeaders
}}
"#
    )
    .unwrap();

    let ok_body = match &op.response {
        ResponseKind::Json(ty) => manifest_sample_output(api, ty).to_string(),
        _ => "{}".to_string(),
    };
    let ok_literal = format!(
        "cannedResponse{{status: 200, contentType: \"application/json\", body: {}}}",
        go_string(&ok_body)
    );

    // Auth.
    match &api.auth {
        Auth::Bearer => {
            writeln!(
                out,
                r#"
func TestAuthenticationHeader(t *testing.T) {{
	server, _, headers := sequenceServer(t, {ok_literal})
	client := newTestClient(t, server.URL)
	ctx := context.Background()
	_, err := {call}
	require.NoError(t, err)
	require.Equal(t, "Bearer test-key", (*headers).Get("Authorization"))
}}"#,
                call = go_get_call("client")
            )
            .unwrap();
        }
        Auth::ApiKeyHeader(header) => {
            writeln!(
                out,
                r#"
func TestAuthenticationHeader(t *testing.T) {{
	server, _, headers := sequenceServer(t, {ok_literal})
	client := newTestClient(t, server.URL)
	ctx := context.Background()
	_, err := {call}
	require.NoError(t, err)
	require.Equal(t, "test-key", (*headers).Get("{header}"))
}}"#,
                call = go_get_call("client")
            )
            .unwrap();
        }
        Auth::None => {}
    }

    // Error mapping.
    writeln!(out, r#"
func TestErrorMapping(t *testing.T) {{
	t.Run("rpc status payloads map to APIError", func(t *testing.T) {{
		server, _, _ := sequenceServer(t, cannedResponse{{status: 404, contentType: "application/json", body: `{{"code":5,"message":"missing","details":[]}}`}})
		client := newTestClient(t, server.URL)
		ctx := context.Background()
		_, err := {call}
		var apiErr *{pkg}.APIError
		require.ErrorAs(t, err, &apiErr)
		require.Equal(t, 404, apiErr.StatusCode)
		require.Equal(t, 5, apiErr.Code)
		require.Equal(t, "missing", apiErr.Message)
	}})
	for name, tc := range map[string]struct {{
		response cannedResponse
		contains string
	}}{{
		"an empty body where JSON is promised": {{cannedResponse{{status: 200, body: ""}}, "protocol error"}},
		"a JSON null body":                     {{cannedResponse{{status: 200, contentType: "application/json", body: "null"}}, "protocol error"}},
		"a non-JSON body":                      {{cannedResponse{{status: 200, contentType: "text/html", body: "<html>"}}, "decoding response"}},
	}} {{
		t.Run(name, func(t *testing.T) {{
			server, _, _ := sequenceServer(t, tc.response)
			client := newTestClient(t, server.URL)
			ctx := context.Background()
			_, err := {call}
			require.Error(t, err)
			require.ErrorContains(t, err, tc.contains)
		}})
	}}
}}

func TestContextCancellation(t *testing.T) {{
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{
		<-r.Context().Done()
	}}))
	t.Cleanup(server.Close)
	client := newTestClient(t, server.URL)
	ctx, cancel := context.WithTimeout(context.Background(), 50*time.Millisecond)
	defer cancel()
	_, err := {call}
	require.Error(t, err)
	require.True(t, errors.Is(err, context.DeadlineExceeded), "got %v", err)
}}

func TestRetries(t *testing.T) {{
	// Retry-After: 0 keeps these instant — backoff honors the header.
	retryable := cannedResponse{{status: 503, body: "", retryAfter: "0"}}
	t.Run("idempotent requests retry within the client budget", func(t *testing.T) {{
		server, hits, _ := sequenceServer(t, retryable, {ok_literal})
		client := newTestClient(t, server.URL, {pkg}.WithMaxRetries(2))
		ctx := context.Background()
		_, err := {call}
		require.NoError(t, err)
		require.Equal(t, int32(2), hits.Load())
	}})
}}"#, call = go_get_call("client"), pkg = pkg, ok_literal = ok_literal).unwrap();

    if let Some((m_resource, m_op)) = anchors.mutation {
        let m_call = call_expr(api, m_resource, m_op, pkg, false);
        let m_ok = match &m_op.response {
            ResponseKind::Json(ty) => manifest_sample_output(api, ty).to_string(),
            _ => "{}".to_string(),
        };
        let m_ok_literal = format!(
            "cannedResponse{{status: 200, contentType: \"application/json\", body: {}}}",
            go_string(&m_ok)
        );
        // The opt-in variant appends a request option to the golden call.
        let m_call_optin = format!(
            "{}, {pkg}.WithRequestRetries(2))",
            strip_last_paren(&m_call)
        );
        writeln!(
            out,
            r#"
func TestMutationRetryPolicy(t *testing.T) {{
	retryable := cannedResponse{{status: 503, body: "", retryAfter: "0"}}
	t.Run("mutations do NOT retry by default", func(t *testing.T) {{
		server, hits, _ := sequenceServer(t, retryable)
		client := newTestClient(t, server.URL, {pkg}.WithMaxRetries(2))
		ctx := context.Background()
		_, err := {m_call}
		require.Error(t, err)
		require.Equal(t, int32(1), hits.Load())
	}})
	t.Run("the CALLER opts one call in via WithRequestRetries", func(t *testing.T) {{
		server, hits, _ := sequenceServer(t, retryable, {m_ok_literal})
		client := newTestClient(t, server.URL)
		ctx := context.Background()
		_, err := {m_call_optin}
		require.NoError(t, err)
		require.Equal(t, int32(2), hits.Load())
	}})
}}"#,
            m_call = m_call,
            m_call_optin = m_call_optin,
            m_ok_literal = m_ok_literal,
            pkg = pkg
        )
        .unwrap();
    }

    writeln!(
        out,
        r#"
func TestPerRequestHeaders(t *testing.T) {{
	server, _, headers := sequenceServer(t, {ok_literal})
	client := newTestClient(t, server.URL)
	ctx := context.Background()
	_, err := {call}
	require.NoError(t, err)
	require.Equal(t, "rid-1", (*headers).Get("X-Request-Id"))
}}"#,
        call = format!(
            "{}, {pkg}.WithRequestHeader(\"X-Request-ID\", \"rid-1\"))",
            strip_last_paren(&go_get_call("client"))
        ),
        ok_literal = ok_literal
    )
    .unwrap();

    {
        writeln!(
            out,
            r#"
// WithDebugLog dumps each HTTP exchange (request line, redacted headers,
// bodies) to the supplied writer without changing behavior. The credential
// must never appear in the dump.
func TestDebugLog(t *testing.T) {{
	server, _, _ := sequenceServer(t, {ok_literal})
	var buf strings.Builder
	client := newTestClient(t, server.URL, {pkg}.WithDebugLog(&buf))
	ctx := context.Background()
	_, err := {call}
	require.NoError(t, err)
	dump := buf.String()
	require.Contains(t, dump, "> GET ", "request line logged")
	require.Contains(t, dump, "< HTTP ", "response status logged")
	require.Contains(t, dump, "Authorization: [redacted]", "auth header redacted, not omitted")
	require.NotContains(t, dump, "test-key", "credential leaked into debug output")
}}"#,
            call = go_get_call("client"),
            pkg = pkg,
            ok_literal = ok_literal
        )
        .unwrap();
    }

    if let Some((resource, op)) = anchors.stream {
        let stream_call = call_expr(api, resource, op, pkg, false);
        writeln!(out, r#"
// A cancelled request context ends a stream PROMPTLY: no reconnect attempts
// (the context is dead — every retry would fail and burn the backoff budget)
// and the cancellation surfaces through Err. A CLI's Ctrl-C depends on this.
func TestStreamCancellationStopsPromptly(t *testing.T) {{
	var hits atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {{
		hits.Add(1)
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(200)
		_, _ = w.Write([]byte("id: e1\ndata: {{}}\n\n"))
		if f, ok := w.(http.Flusher); ok {{
			f.Flush()
		}}
		<-r.Context().Done()
	}}))
	t.Cleanup(server.Close)
	client := newTestClient(t, server.URL)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	stream, err := {stream_call}
	require.NoError(t, err)
	defer stream.Close()
	require.True(t, stream.Next(), "first event arrives: %v", stream.Err())
	cancel()
	started := time.Now()
	require.False(t, stream.Next(), "stream ends after cancellation")
	require.Less(t, time.Since(started), 2*time.Second, "cancellation must not wait out the reconnect backoff")
	require.True(t, errors.Is(stream.Err(), context.Canceled), "got %v", stream.Err())
	require.Equal(t, int32(1), hits.Load(), "no reconnect against a cancelled context")
}}"#, stream_call = stream_call).unwrap();
    }

    out
}

/// Strip exactly ONE trailing close-paren (trim_end_matches would eat a
/// builder chain's `ToParams())` entirely).
fn strip_last_paren(s: &str) -> &str {
    s.strip_suffix(')').unwrap_or(s)
}

fn go_string(s: &str) -> String {
    if !s.contains('`') {
        format!("`{s}`")
    } else {
        serde_json::to_string(s).unwrap()
    }
}

// ---- webhooks ---------------------------------------------------------------

fn webhooks_test(api: &Api, pkg: &str) -> String {
    let payload = api
        .webhooks
        .first()
        .map(|w| manifest_sample_output(api, &w.payload).to_string())
        .unwrap_or_else(|| "{}".to_string());
    format!(
        r#"// Code generated by redwood. DO NOT EDIT.
package {pkg}_test

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/base64"
	"fmt"
	"net/http"
	"testing"
	"time"

	"github.com/stretchr/testify/require"

	{pkg} "MODULE_PATH"
)

var webhookKey = []byte("0123456789abcdef0123456789abcdef")

func webhookSecret() string {{
	return "whsec_" + base64.StdEncoding.EncodeToString(webhookKey)
}}

func signedHeaders(payload []byte, at int64, key []byte) http.Header {{
	mac := hmac.New(sha256.New, key)
	fmt.Fprintf(mac, "msg_1.%d.%s", at, payload)
	signature := base64.StdEncoding.EncodeToString(mac.Sum(nil))
	headers := http.Header{{}}
	headers.Set("webhook-id", "msg_1")
	headers.Set("webhook-timestamp", fmt.Sprintf("%d", at))
	headers.Set("webhook-signature", "v1,"+signature)
	return headers
}}

func TestWebhookVerification(t *testing.T) {{
	payload := []byte({payload_literal})
	t.Run("an authentic signature verifies", func(t *testing.T) {{
		require.NoError(t, {pkg}.VerifyWebhook(webhookSecret(), payload, signedHeaders(payload, time.Now().Unix(), webhookKey)))
	}})
	t.Run("a tampered payload is rejected", func(t *testing.T) {{
		headers := signedHeaders(payload, time.Now().Unix(), webhookKey)
		require.Error(t, {pkg}.VerifyWebhook(webhookSecret(), append(payload, ' '), headers))
	}})
	t.Run("the wrong secret is rejected", func(t *testing.T) {{
		other := "whsec_" + base64.StdEncoding.EncodeToString([]byte("fedcba9876543210fedcba9876543210"))
		require.Error(t, {pkg}.VerifyWebhook(other, payload, signedHeaders(payload, time.Now().Unix(), webhookKey)))
	}})
	t.Run("a stale timestamp is rejected as replay", func(t *testing.T) {{
		headers := signedHeaders(payload, time.Now().Add(-time.Hour).Unix(), webhookKey)
		require.ErrorContains(t, {pkg}.VerifyWebhook(webhookSecret(), payload, headers), "tolerance")
	}})
	t.Run("a missing signature header is rejected", func(t *testing.T) {{
		headers := signedHeaders(payload, time.Now().Unix(), webhookKey)
		headers.Del("webhook-signature")
		require.Error(t, {pkg}.VerifyWebhook(webhookSecret(), payload, headers))
	}})
}}
"#,
        pkg = pkg,
        payload_literal = go_string(&payload),
    )
}
