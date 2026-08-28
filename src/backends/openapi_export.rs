//! OpenAPI export backend: re-emits the input spec (canonicalized YAML —
//! semantic values preserved; comments/formatting are not) with
//! `x-codeSamples` per operation for docs sites like Mintlify.
//!
//! Samples are rendered from one shared call plan built from the COMPLETE
//! IR operation (positional, required path/query, required body fields,
//! whole body) through the SAME naming helpers and target configuration the
//! SDK backends use — a sample that names a method or argument the
//! generated SDK does not accept is a generator defect, not a docs nit.

use heck::{ToLowerCamelCase, ToUpperCamelCase};
use serde_yaml::{Mapping, Value};

use crate::backends::{cli as cli_backend, cli_inputs, golang, python, ruby, Backend, FileSet};
use crate::config::{CliConfig, GoConfig, PythonConfig, RubyConfig, TypeScriptConfig};
use crate::ir::*;

pub struct OpenApiBackend {
    /// Raw source spec. The export preserves its SEMANTIC content (values),
    /// not its lexical form: serde_yaml reflows formatting and drops
    /// comments. Must come from the same parse the IR was lowered from.
    pub spec_source: String,
    pub ts_config: TypeScriptConfig,
    pub go_config: GoConfig,
    pub py_config: PythonConfig,
    pub rb_config: RubyConfig,
    pub cli_config: CliConfig,
}

impl Backend for OpenApiBackend {
    fn name(&self) -> &'static str {
        "openapi"
    }

    fn generate(&self, api: &Api) -> anyhow::Result<FileSet> {
        // Go/CLI sample naming honors the same vendor casings the SDK uses.
        golang::install_config_casings(&self.go_config.special_casings);

        let mut doc: Value = serde_yaml::from_str(&self.spec_source)
            .map_err(|e| anyhow::anyhow!("parsing source spec: {e}"))?;

        // Coherence gate: the raw document and the IR must describe the SAME
        // operation set, one-to-one. A partially supported document would
        // otherwise keep stale hand-written samples silently.
        let ir_ids: std::collections::BTreeSet<&str> = api
            .resources
            .iter()
            .flat_map(|r| r.operations.iter().map(|o| o.id.as_str()))
            .collect();
        let mut raw_ids: Vec<String> = Vec::new();
        if let Some(paths) = doc.get("paths").and_then(Value::as_mapping) {
            for (_path, item) in paths {
                let Some(item) = item.as_mapping() else {
                    continue;
                };
                for (_method, op) in item {
                    if let Some(id) = op
                        .as_mapping()
                        .and_then(|m| m.get(Value::from("operationId")))
                        .and_then(Value::as_str)
                    {
                        raw_ids.push(id.to_string());
                    }
                }
            }
        }
        let mut seen = std::collections::BTreeSet::new();
        let mut problems: Vec<String> = Vec::new();
        for id in &raw_ids {
            if !seen.insert(id.as_str()) {
                problems.push(format!("duplicate operationId {id}"));
            }
            if !ir_ids.contains(id.as_str()) {
                problems.push(format!("operationId {id} is not represented in the IR"));
            }
        }
        for id in &ir_ids {
            if !raw_ids.iter().any(|r| r == id) {
                problems.push(format!(
                    "IR operation {id} missing from the source document"
                ));
            }
        }
        if !problems.is_empty() {
            anyhow::bail!(
                "source spec and IR disagree; refusing to annotate:\n  {}",
                problems.join("\n  ")
            );
        }

        let paths = doc
            .get_mut("paths")
            .and_then(Value::as_mapping_mut)
            .ok_or_else(|| anyhow::anyhow!("spec has no paths object"))?;
        for (_path, item) in paths.iter_mut() {
            let Some(item) = item.as_mapping_mut() else {
                continue;
            };
            for (_method, op_value) in item.iter_mut() {
                let Some(op_map) = op_value.as_mapping_mut() else {
                    continue;
                };
                let Some(op_id) = op_map
                    .get(Value::from("operationId"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                else {
                    continue;
                };
                let samples = self.samples_for(api, &op_id).ok_or_else(|| {
                    anyhow::anyhow!("no IR operation for {op_id} despite coherence gate")
                })?;
                op_map.insert(Value::from("x-codeSamples"), samples);
            }
        }

        let mut files = FileSet::new();
        files.insert(
            "openapi.yml".into(),
            serde_yaml::to_string(&doc).map_err(|e| anyhow::anyhow!("serializing spec: {e}"))?,
        );
        Ok(files)
    }
}

// ---- shared call plan -------------------------------------------------------

/// Everything a sample must supply for the call to be accepted by the
/// generated SDKs. Client-default params (e.g. a workspace id) are
/// intentionally omitted: every SDK documents the env/client default.
struct Plan<'a> {
    positionals: Vec<&'a Param>,
    /// Required path params that are neither the positional nor client
    /// defaults, then required query params.
    named: Vec<&'a Param>,
    body: Vec<&'a Field>,
    whole_body: Option<&'a Ty>,
    response: &'a ResponseKind,
    paginated: bool,
}

fn plan<'a>(api: &Api, op: &'a Operation) -> Plan<'a> {
    let is_client_param = |wire: &str| api.client_params.iter().any(|c| c.wire_name == wire);
    let positionals: Vec<&Param> = op.positionals.iter().collect();
    let named = op
        .path_params
        .iter()
        .filter(|p| {
            p.required
                && !is_client_param(&p.wire_name)
                && !positionals.iter().any(|q| q.wire_name == p.wire_name)
        })
        .chain(
            op.query_params
                .iter()
                .filter(|q| q.required && !is_client_param(&q.wire_name)),
        )
        .collect();
    Plan {
        positionals,
        named,
        body: op.body_fields.iter().filter(|f| f.required).collect(),
        whole_body: op.whole_body.as_ref(),
        response: &op.response,
        paginated: op.pagination.is_some(),
    }
}

pub(crate) fn sample_id(wire: &str) -> String {
    use heck::ToSnakeCase;
    let snake = wire.to_snake_case();
    format!(
        "{}_123",
        snake.trim_end_matches("_id").trim_end_matches("id")
    )
}

/// Wire-keyed sample for TS/CLI; snake-keyed for Python/Ruby.
fn wire_sample(api: &Api, ty: &Ty) -> serde_json::Value {
    // Complete, untrimmed: truncation can delete REQUIRED fields and the
    // conformance mock already proves these samples are type-valid.
    crate::backends::manifest_sample(api, ty)
}

fn snake_sample_v(api: &Api, ty: &Ty) -> serde_json::Value {
    crate::backends::snake_sample(api, ty, crate::backends::manifest_sample(api, ty))
}

impl OpenApiBackend {
    fn samples_for(&self, api: &Api, op_id: &str) -> Option<Value> {
        let (resource, op) = api
            .resources
            .iter()
            .flat_map(|r| r.operations.iter().map(move |o| (r, o)))
            .find(|(_, o)| o.id == op_id)?;
        let plan = plan(api, op);

        let entries = [
            (
                "typescript",
                "TypeScript",
                self.ts_sample(api, resource, op, &plan),
            ),
            ("go", "Go", self.go_sample(api, resource, op, &plan)),
            ("python", "Python", self.py_sample(api, resource, op, &plan)),
            ("ruby", "Ruby", self.rb_sample(api, resource, op, &plan)),
            ("bash", "CLI", self.cli_sample(api, resource, op, &plan)),
            ("shell", "curl", self.curl_sample(api, op, &plan)),
        ];
        let list: Vec<Value> = entries
            .into_iter()
            .map(|(lang, label, source)| {
                let mut m = Mapping::new();
                m.insert(Value::from("lang"), Value::from(lang));
                m.insert(Value::from("label"), Value::from(label));
                m.insert(Value::from("source"), Value::from(source));
                Value::Mapping(m)
            })
            .collect();
        Some(Value::Sequence(list))
    }

    // ---- TypeScript ---------------------------------------------------------

    fn ts_sample(&self, api: &Api, resource: &Resource, op: &Operation, plan: &Plan) -> String {
        let package = self
            .ts_config
            .package_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase());
        let accessor: String = resource
            .path()
            .split('.')
            .map(|p| p.to_lower_camel_case())
            .collect::<Vec<_>>()
            .join(".");
        let method = op.name.to_lower_camel_case();
        let mut args: Vec<String> = Vec::new();
        for p in &plan.positionals {
            args.push(format!("'{}'", sample_id(&p.wire_name)));
        }
        let mut fields: Vec<String> = Vec::new();
        for p in &plan.named {
            fields.push(format!(
                "{}: {}",
                p.wire_name.to_lower_camel_case(),
                ts_value(&wire_sample(api, &p.ty))
            ));
        }
        for f in &plan.body {
            fields.push(format!(
                "{}: {}",
                f.wire_name.to_lower_camel_case(),
                ts_value(&wire_sample(api, &f.ty))
            ));
        }
        if let Some(ty) = plan.whole_body {
            fields.push(format!("body: {}", ts_value(&wire_sample(api, ty))));
        }
        if !fields.is_empty() {
            args.push(format!("{{ {} }}", fields.join(", ")));
        }
        let call = format!("client.{accessor}.{method}({})", args.join(", "));
        let usage = match (plan.paginated, plan.response) {
            (true, _) => format!(
                "const page = await {call};\nfor await (const item of page) {{\n  console.log(item);\n}}"
            ),
            (false, ResponseKind::Sse(_)) => format!(
                "const stream = await {call};\ntry {{\n  for await (const event of stream) {{\n    console.log(event);\n  }}\n}} finally {{\n  await stream.close();\n}}"
            ),
            (false, ResponseKind::Empty) => format!("await {call};"),
            _ => format!("const result = await {call};"),
        };
        format!(
            "import {name} from '{package}';\n\nconst client = new {name}();\n{usage}",
            name = api.name
        )
    }

    // ---- Go -----------------------------------------------------------------

    fn go_sample(&self, api: &Api, resource: &Resource, op: &Operation, plan: &Plan) -> String {
        let pkg = self
            .go_config
            .package_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase());
        let module = self
            .go_config
            .module_path
            .clone()
            .unwrap_or_else(|| format!("example.com/{}-go", api.name.to_lowercase()));
        let chain = match &resource.parent {
            Some(parent) => format!(
                "client.{}().{}()",
                golang::go_name(parent),
                golang::go_name(&resource.name)
            ),
            None => format!("client.{}()", golang::go_name(&resource.name)),
        };
        let method = golang::go_name(&op.name);
        let mut args: Vec<String> = vec!["ctx".into()];
        for p in &plan.positionals {
            args.push(format!("\"{}\"", sample_id(&p.wire_name)));
        }
        let mut pre = String::new();
        if op.has_params() {
            let params_ty = golang::params_type_name(resource, op);
            let mut fields: Vec<String> = Vec::new();
            for p in &plan.named {
                fields.push(format!(
                    "\t\t{}: {},",
                    golang::go_name(&p.wire_name),
                    go_value(api, &p.ty, &pkg)
                ));
            }
            for f in &plan.body {
                fields.push(format!(
                    "\t\t{}: {},",
                    golang::go_name(&f.wire_name),
                    go_value(api, &f.ty, &pkg)
                ));
            }
            if let Some(ty) = plan.whole_body {
                fields.push(format!("\t\tBody: {},", go_value(api, ty, &pkg)));
            }
            pre = if fields.is_empty() {
                format!("\tparams := &{pkg}.{params_ty}{{}}\n")
            } else {
                format!(
                    "\tparams := &{pkg}.{params_ty}{{\n{}\n\t}}\n",
                    fields.join("\n")
                )
            };
            args.push("params".into());
        }
        let call = format!("{chain}.{method}({})", args.join(", "));
        let usage = match (plan.paginated, plan.response) {
            (false, ResponseKind::Empty) => {
                format!("\tif err := {call}; err != nil {{\n\t\tlog.Fatal(err)\n\t}}")
            }
            (false, ResponseKind::Sse(_)) => format!(
                "\tstream, err := {call}\n\tif err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n\tdefer stream.Close()\n\tfor stream.Next() {{\n\t\tfmt.Println(stream.Current())\n\t}}\n\tif err := stream.Err(); err != nil {{\n\t\tlog.Fatal(err)\n\t}}"
            ),
            _ => format!(
                "\tresult, err := {call}\n\tif err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n\tfmt.Printf(\"%+v\\n\", result)"
            ),
        };
        let fmt_import = if usage.contains("fmt.") {
            "\t\"fmt\"\n"
        } else {
            ""
        };
        format!(
            "package main\n\nimport (\n\t\"context\"\n{fmt_import}\t\"log\"\n\n\t{pkg} \"{module}\"\n)\n\nfunc main() {{\n\tclient, err := {pkg}.NewClient()\n\tif err != nil {{\n\t\tlog.Fatal(err)\n\t}}\n\tctx := context.Background()\n{pre}{usage}\n}}"
        )
    }

    // ---- Python -------------------------------------------------------------

    fn py_sample(&self, api: &Api, resource: &Resource, op: &Operation, plan: &Plan) -> String {
        let pkg = self
            .py_config
            .package_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase());
        let method = python::py_name(&op.name);
        let mut args: Vec<String> = Vec::new();
        for p in &plan.positionals {
            args.push(format!("\"{}\"", sample_id(&p.wire_name)));
        }
        for p in &plan.named {
            args.push(format!(
                "{}={}",
                python::py_name(&p.wire_name),
                python::py_literal(&snake_sample_v(api, &p.ty))
            ));
        }
        for f in &plan.body {
            args.push(format!(
                "{}={}",
                python::py_name(&f.wire_name),
                python::py_literal(&snake_sample_v(api, &f.ty))
            ));
        }
        if let Some(ty) = plan.whole_body {
            args.push(format!(
                "body={}",
                python::py_literal(&snake_sample_v(api, ty))
            ));
        }
        let call = format!("client.{}.{method}({})", resource.path(), args.join(", "));
        let usage = match (plan.paginated, plan.response) {
            (true, _) => format!("    for item in {call}:\n        print(item)"),
            (false, ResponseKind::Sse(_)) => format!(
                "    with {call} as stream:\n        for event in stream:\n            print(event)"
            ),
            (false, ResponseKind::Empty) => format!("    {call}"),
            _ => format!("    result = {call}\n    print(result)"),
        };
        format!(
            "from {pkg} import {name}\n\nwith {name}() as client:\n{usage}",
            name = api.name
        )
    }

    // ---- Ruby ---------------------------------------------------------------

    fn rb_sample(&self, api: &Api, resource: &Resource, op: &Operation, plan: &Plan) -> String {
        let gem = self
            .rb_config
            .gem_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase());
        let module = api.name.to_upper_camel_case();
        let method = ruby::rb_name(&op.name);
        let mut args: Vec<String> = Vec::new();
        for p in &plan.positionals {
            args.push(format!("\"{}\"", sample_id(&p.wire_name)));
        }
        for p in &plan.named {
            args.push(format!(
                "{}: {}",
                ruby::rb_name(&p.wire_name),
                ruby::rb_literal(&snake_sample_v(api, &p.ty))
            ));
        }
        for f in &plan.body {
            args.push(format!(
                "{}: {}",
                ruby::rb_name(&f.wire_name),
                ruby::rb_literal(&snake_sample_v(api, &f.ty))
            ));
        }
        if let Some(ty) = plan.whole_body {
            args.push(format!(
                "body: {}",
                ruby::rb_literal(&snake_sample_v(api, ty))
            ));
        }
        let call = format!("client.{}.{method}({})", resource.path(), args.join(", "));
        let usage = match (plan.paginated, plan.response) {
            (true, _) => format!("{call}.each do |item|\n  puts item.inspect\nend"),
            (false, ResponseKind::Sse(_)) => {
                format!("stream = {call}\nstream.each {{ |event| puts event.inspect }}")
            }
            (false, ResponseKind::Empty) => call,
            _ => format!("result = {call}\nputs result.inspect"),
        };
        format!("require \"{gem}\"\n\nclient = {module}::Client.new\n{usage}")
    }

    // ---- CLI ----------------------------------------------------------------

    fn cli_sample(&self, api: &Api, resource: &Resource, op: &Operation, plan: &Plan) -> String {
        let binary = self
            .cli_config
            .binary_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase());
        let cmd: String = resource
            .path()
            .split('.')
            .map(cli_backend::command_name)
            .collect::<Vec<_>>()
            .join(" ");
        let mut line = format!("{binary} {cmd} {}", cli_backend::command_name(&op.name));
        for p in &plan.positionals {
            line.push_str(&format!(" {}", sample_id(&p.wire_name)));
        }
        let push_flag = |line: &mut String, wire: &str, ty: &Ty| {
            let flag = cli_backend::flag_name(wire);
            let value = wire_sample(api, ty);
            // The CLI grammar for list flags is REPEATABLE ([--flag <v>]...),
            // one occurrence per element — a whole-array value would decode
            // as a nested array and fail the typed decode.
            let occurrences: Vec<serde_json::Value> = match value {
                serde_json::Value::Array(items) => items,
                other => vec![other],
            };
            for value in occurrences {
                let rendered = match &value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Bool(_) | serde_json::Value::Number(_) => value.to_string(),
                    other => format!("'{other}'"), // JSON documents, shell-quoted
                };
                line.push_str(&format!(" \\\n  --{flag} {rendered}"));
            }
        };
        for p in &plan.named {
            push_flag(&mut line, &p.wire_name, &p.ty);
        }
        let _ = (&plan.body, plan.whole_body);
        // The body the way a user types it: every required input of the
        // flattened surface (first arm of each union), so the sample is
        // both minimal and accepted by the CLI's own preflight.
        let opts = cli_inputs::plan_options(&self.cli_config, op);
        if let Ok(Some(body)) = crate::ir::plan::body_plan(api, op, &opts) {
            for arg in cli_inputs::sample_args(api, &body) {
                line.push_str(&format!(" \\\n  {arg}"));
            }
        }
        line
    }

    // ---- cURL ---------------------------------------------------------------

    fn curl_sample(&self, api: &Api, op: &Operation, plan: &Plan) -> String {
        let mut path = op.path.clone();
        for p in op.positionals.iter().chain(op.path_params.iter()) {
            path = path.replace(
                &format!("{{{}}}", p.wire_name),
                &percent_encode(&sample_id(&p.wire_name)),
            );
        }

        let mut query = Vec::new();
        for p in op.query_params.iter().filter(|p| p.required) {
            let value = wire_sample(api, &p.ty);
            let values = match value {
                serde_json::Value::Array(items) => items,
                other => vec![other],
            };
            for value in values {
                query.push(format!(
                    "{}={}",
                    percent_encode(&p.wire_name),
                    percent_encode(&curl_scalar(&value))
                ));
            }
        }

        let mut url = format!("{}{}", api.base_url.trim_end_matches('/'), path);
        if !query.is_empty() {
            url.push('?');
            url.push_str(&query.join("&"));
        }

        let mut line = format!(
            "curl --request {} \\\n  --url {}",
            op.http_method.as_str(),
            shell_single_quote(&url)
        );
        match &api.auth {
            Auth::Bearer => line.push_str(&format!(
                " \\\n  --header \"Authorization: Bearer ${{{}}}\"",
                api.api_key_env
            )),
            Auth::ApiKeyHeader(header) => line.push_str(&format!(
                " \\\n  --header \"{}: ${{{}}}\"",
                shell_double_quote_content(header),
                api.api_key_env
            )),
            Auth::None => {}
        }

        let body = if let Some(ty) = plan.whole_body {
            Some(wire_sample(api, ty))
        } else if !plan.body.is_empty() {
            let mut object = serde_json::Map::new();
            for field in &plan.body {
                object.insert(field.wire_name.clone(), wire_sample(api, &field.ty));
            }
            Some(serde_json::Value::Object(object))
        } else {
            None
        };
        if let Some(body) = body {
            line.push_str(" \\\n  --header 'Content-Type: application/json'");
            line.push_str(&format!(
                " \\\n  --data {}",
                shell_single_quote(&body.to_string())
            ));
        }
        line
    }
}

// ---- value renderers --------------------------------------------------------

fn curl_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(value) => value.clone(),
        other => other.to_string(),
    }
}

fn percent_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write;
            write!(encoded, "%{byte:02X}").expect("writing to a String cannot fail");
        }
    }
    encoded
}

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_double_quote_content(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('`', "\\`")
        .replace('$', "\\$")
}

/// serde_json value as a TypeScript literal (JSON is valid TS).
fn ts_value(v: &serde_json::Value) -> String {
    v.to_string()
}

/// Typed Go value for a REQUIRED field position (never a pointer).
pub(crate) fn go_value(api: &Api, ty: &Ty, pkg: &str) -> String {
    match ty {
        Ty::String | Ty::Timestamp | Ty::Bytes => "\"sample\"".into(),
        Ty::Bool => "true".into(),
        Ty::Int32 | Ty::Int64 => "1".into(),
        Ty::Float | Ty::Double => "1.0".into(),
        Ty::Literal(v) => format!("\"{v}\""),
        Ty::Json => "map[string]any{}".into(),
        Ty::Map(_) => "map[string]any{}".into(),
        Ty::List(inner) => {
            // Slice ELEMENTS are values even where field positions hold
            // pointers; strip the address-of from composite literals.
            let element = go_value(api, inner, pkg);
            let element = element.strip_prefix('&').unwrap_or(&element);
            format!("[]{}{{{element}}}", go_ty_label(api, inner, pkg))
        }
        Ty::Named(n) => match api.types.get(n).map(|d| &d.shape) {
            Some(Shape::Enum(e)) => {
                let value = e
                    .values
                    .iter()
                    .find(|v| !v.ends_with("UNSPECIFIED"))
                    .or_else(|| e.values.first())
                    .cloned()
                    .unwrap_or_default();
                format!("{pkg}.{}(\"{value}\")", golang::go_type_name(n))
            }
            Some(Shape::Struct(s)) => {
                // Struct-typed fields are always pointers in the generated
                // Go types; render an address-of literal. Request direction:
                // the INPUT view (readOnly fields don't exist there).
                let fields: Vec<String> = s
                    .input_fields()
                    .filter(|f| f.required)
                    .map(|f| {
                        format!(
                            "{}: {}",
                            golang::go_name(&f.wire_name),
                            go_value(api, &f.ty, pkg)
                        )
                    })
                    .collect();
                format!(
                    "&{pkg}.{}{{{}}}",
                    golang::go_input_type_name(api, &api.divergent_types(), n),
                    fields.join(", ")
                )
            }
            Some(Shape::Union(u)) => {
                // Field positions hold *Union; a composite literal is
                // addressable (a constructor's return value is not). The
                // variant's required discriminator literal stamps the tag.
                let (index, variant) = u
                    .variants
                    .iter()
                    .enumerate()
                    .next()
                    .expect("union has variants");
                format!(
                    "&{pkg}.{}{{{}: {}}}",
                    golang::go_input_type_name(api, &api.divergent_types(), n),
                    golang::variant_field_name(variant, index),
                    go_value(api, &variant.ty, pkg)
                )
            }
            Some(Shape::Alias(inner)) => {
                let inner = inner.clone();
                go_value(api, &inner, pkg)
            }
            None => "nil".into(),
        },
    }
}

pub(crate) fn go_ty_label(api: &Api, ty: &Ty, pkg: &str) -> String {
    match ty {
        Ty::String | Ty::Timestamp | Ty::Bytes => "string".into(),
        Ty::Bool => "bool".into(),
        Ty::Int32 => "int32".into(),
        Ty::Int64 => "int64".into(),
        Ty::Float | Ty::Double => "float64".into(),
        // Sample labels sit in request position: use the input view name.
        Ty::Named(n) => format!(
            "{pkg}.{}",
            golang::go_input_type_name(api, &api.divergent_types(), n)
        ),
        _ => "any".into(),
    }
}
