//! Manifest backend: dumps the IR operation table as JSON for the
//! conformance harness. The mock server validates every SDK request against
//! this file, and drivers synthesize call arguments from the samples here,
//! so "every endpoint validated" is mechanical, not manual.

use serde_json::{json, Map, Value};

use crate::backends::cli_inputs;
use crate::backends::{Backend, FileSet};
use crate::config::CliConfig;
use crate::ir::plan::{BodyPlan, InputKind};
use crate::ir::*;

/// The CLI configuration decides how body fields become flags; the manifest
/// records the same plan so the CLI harness drives the flattened surface.
#[derive(Default)]
pub struct ManifestBackend {
    pub cli: CliConfig,
}

impl Backend for ManifestBackend {
    fn name(&self) -> &'static str {
        "manifest"
    }

    fn generate(&self, api: &Api) -> anyhow::Result<FileSet> {
        let mut files = FileSet::new();
        files.insert("manifest.json".into(), render_with(api, &self.cli)?);
        Ok(files)
    }
}

pub fn render(api: &Api) -> anyhow::Result<String> {
    render_with(api, &CliConfig::default())
}

pub fn render_with(api: &Api, cli: &CliConfig) -> anyhow::Result<String> {
    let plans = cli_inputs::plan_all(api, cli)?;
    let mut operations = Vec::new();
    for resource in &api.resources {
        for op in &resource.operations {
            operations.push(operation_json(api, resource, op, plans.get(&op.id)));
        }
    }
    let webhooks: Vec<Value> = api
        .webhooks
        .iter()
        .map(|w| {
            json!({
                "name": w.name,
                "payloadSample": sample(api, &w.payload, 0, SampleDir::Output),
                "discriminatorField": w.discriminator_field,
            })
        })
        .collect();
    let doc = json!({
        "api": { "name": api.name, "version": api.version, "baseUrl": api.base_url },
        "clientParams": api.client_params.iter().map(|c| json!({
            "wireName": c.wire_name, "envVar": c.env_var,
        })).collect::<Vec<_>>(),
        "operations": operations,
        "webhooks": webhooks,
    });
    Ok(serde_json::to_string_pretty(&doc)? + "\n")
}

fn operation_json(
    api: &Api,
    resource: &Resource,
    op: &Operation,
    plan: Option<&BodyPlan>,
) -> Value {
    let params_json = |params: &[Param]| -> Vec<Value> {
        params
            .iter()
            .map(|p| {
                json!({
                    "name": p.wire_name,
                    "required": p.required,
                    "sample": sample(api, &p.ty, 0, SampleDir::Input),
                })
            })
            .collect()
    };
    let body_json: Vec<Value> = op
        .body_fields
        .iter()
        .map(|f| {
            // The CLI flag that takes this field whole (a document, or the
            // field's own typed flag).
            let cli_flag = plan.and_then(|p| {
                p.inputs
                    .iter()
                    .find(|i| i.path.len() == 1 && i.path[0] == f.wire_name)
                    .map(|i| i.flag.clone())
            });
            json!({
                "name": f.wire_name,
                "required": f.required,
                "sample": sample(api, &f.ty, 0, SampleDir::Input),
                "cliFlag": cli_flag,
            })
        })
        .collect();
    let cli_json = plan.map(|p| cli_plan_json(api, p));
    let (response_kind, response_sample) = match (&op.pagination, &op.response) {
        (Some(page), ResponseKind::Json(ty)) => ("paginated", page_response_sample(api, page, ty)),
        (None, ResponseKind::Json(ty)) => ("json", sample(api, ty, 0, SampleDir::Output)),
        (None, ResponseKind::Sse(ty)) => ("sse", sample(api, ty, 0, SampleDir::Output)),
        (None, ResponseKind::Empty) => ("empty", Value::Null),
        (Some(_), _) => unreachable!("pagination implies JSON"),
    };
    json!({
        "id": op.id,
        // Dotted accessor path for nested resources, e.g. "parent.children".
        "resource": resource.path(),
        "method": op.name,
        "httpMethod": op.http_method.as_str(),
        "path": op.path,
        "positionals": op.positionals.iter().map(|p| json!({
            "name": p.wire_name,
            "sample": sample(api, &p.ty, 0, SampleDir::Input),
        })).collect::<Vec<_>>(),
        "pathParams": params_json(&op.path_params),
        "queryParams": params_json(&op.query_params),
        "bodyFields": body_json,
        // A non-object body sent whole (params carry it as `body`). A
        // discriminated union additionally lists its arms as `choices`, the
        // shape flag-driven surfaces (the CLI) expose instead of a document:
        // exactly one arm is given, keyed by `name`, and the request body is
        // that arm's object.
        "wholeBody": op.whole_body.as_ref().map(|ty| {
            let mut body = json!({
                "sample": sample(api, ty, 0, SampleDir::Input),
            });
            if let Some(choices) = api.body_choices(op) {
                let arms: Vec<Value> = choices.iter().map(|c| {
                    let arm_sample = sample(api, &Ty::Named(c.variant.clone()), 0, SampleDir::Input);
                    let input_sample = match &c.payload_field {
                        Some(field) => arm_sample.get(field).cloned().unwrap_or(Value::Null),
                        None => arm_sample.clone(),
                    };
                    json!({
                        "tag": c.tag,
                        "name": c.wire_name,
                        "sample": input_sample,
                        "bodySample": arm_sample,
                    })
                }).collect();
                body["choices"] = Value::Array(arms);
            }
            body
        }),
        "response": { "kind": response_kind, "sample": response_sample },
        // The flattened CLI surface: every input with a sample value the
        // harness can pass, and the unions whose arms group them.
        "cli": cli_json,
        "pagination": op.pagination.as_ref().map(|p| json!({
            "itemsField": p.items_field,
            "cursorParam": p.cursor_param,
            "nextCursorPath": p.next_cursor_path,
        })),
    })
}

/// The CLI body plan as JSON: inputs (flag, wire path, kind, sample, the
/// chain of union arms containing them) and unions (flag, tags).
fn cli_plan_json(api: &Api, plan: &BodyPlan) -> Value {
    let inputs: Vec<Value> = plan
        .inputs
        .iter()
        .map(|i| {
            let (kind, sample_value) = match &i.kind {
                InputKind::Leaf(ty) => ("leaf", sample(api, ty, 0, SampleDir::Input)),
                InputKind::KvMap(ty) => (
                    "kvMap",
                    json!({"key": sample(api, ty, 0, SampleDir::Input)}),
                ),
                InputKind::ScalarList(ty) => {
                    ("scalarList", json!([sample(api, ty, 0, SampleDir::Input)]))
                }
                InputKind::Doc(ty) => ("doc", sample(api, ty, 0, SampleDir::Input)),
                InputKind::EntryDoc(ty) => ("entryDoc", sample(api, ty, 0, SampleDir::Input)),
                InputKind::DocList(ty) => {
                    ("docList", json!([sample(api, ty, 0, SampleDir::Input)]))
                }
                InputKind::ShorthandList { item, .. } => (
                    "shorthandList",
                    json!([sample(api, item, 0, SampleDir::Input)]),
                ),
                InputKind::UnionTag => (
                    "unionTag",
                    Value::String(
                        i.union
                            .and_then(|u| plan.unions[u].arms.first())
                            .map(|a| a.tag.clone())
                            .unwrap_or_default(),
                    ),
                ),
            };
            // Outermost first, so a harness can pick the first arm of each.
            let mut arms = Vec::new();
            let mut current = i.arm.clone();
            while let Some(arm) = current {
                let union = &plan.unions[arm.union];
                arms.push(json!({"union": union.flag, "tag": arm.tag}));
                current = union.parent_arm.clone();
            }
            arms.reverse();
            json!({
                "flag": i.flag,
                "path": i.path,
                "kind": kind,
                "required": i.required,
                "sample": sample_value,
                "arms": arms,
            })
        })
        .collect();
    let unions: Vec<Value> = plan
        .unions
        .iter()
        .map(|u| {
            json!({
                "flag": u.flag,
                "path": u.path,
                "tags": u.arms.iter().map(|a| a.tag.clone()).collect::<Vec<_>>(),
                "inferable": u.inferable,
            })
        })
        .collect();
    json!({ "inputs": inputs, "unions": unions, "wholeBody": plan.whole_body })
}

/// Response body for a paginated op: one item, no next cursor (so drivers
/// terminate after a single page).
fn page_response_sample(api: &Api, page: &Pagination, response_ty: &Ty) -> Value {
    let mut body = match sample(api, response_ty, 0, SampleDir::Output) {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    body.insert(
        page.items_field.clone(),
        Value::Array(vec![sample(api, &page.item_ty, 0, SampleDir::Output)]),
    );
    // Strip any next cursor the generic sampler put in.
    if let Some((envelope, _)) = page.next_cursor_path.split_once('.') {
        body.remove(envelope);
    } else {
        body.remove(&page.next_cursor_path);
    }
    Value::Object(body)
}

/// Public entry for other backends' conformance drivers.
pub fn manifest_sample(api: &Api, ty: &Ty) -> Value {
    sample(api, ty, 0, SampleDir::Input)
}

/// Response-direction sample (writeOnly fields omitted).
pub fn manifest_sample_output(api: &Api, ty: &Ty) -> Value {
    sample(api, ty, 0, SampleDir::Output)
}

/// A request sample with schema-known object keys renamed to snake_case, for
/// drivers of languages whose request surface is snake_case (the generated
/// encoders translate back to wire names — the wire-validating mock then
/// proves the round trip). Json/Map values keep their keys untouched.
pub fn snake_sample(api: &Api, ty: &Ty, value: Value) -> Value {
    match (ty, value) {
        (Ty::Named(n), value) => match api.types.get(n).map(|d| &d.shape) {
            Some(Shape::Struct(s)) => {
                let Value::Object(map) = value else {
                    return value;
                };
                let mut out = serde_json::Map::new();
                for (key, item) in map {
                    match s.fields.iter().find(|f| f.wire_name == key) {
                        Some(field) => {
                            let snake = heck::ToSnakeCase::to_snake_case(key.as_str());
                            out.insert(snake, snake_sample(api, &field.ty, item));
                        }
                        None => {
                            out.insert(key, item);
                        }
                    }
                }
                Value::Object(out)
            }
            Some(Shape::Union(u)) => {
                let tag = u.discriminator.as_ref().and_then(|d| {
                    value
                        .get(&d.property)
                        .and_then(|t| t.as_str())
                        .map(String::from)
                });
                let variant = u
                    .variants
                    .iter()
                    .find(|v| v.tag.as_deref() == tag.as_deref());
                match variant {
                    Some(v) => {
                        let ty = v.ty.clone();
                        snake_sample(api, &ty, value)
                    }
                    None => value,
                }
            }
            Some(Shape::Alias(inner)) => {
                let inner = inner.clone();
                snake_sample(api, &inner, value)
            }
            _ => value,
        },
        (Ty::List(inner), Value::Array(items)) => Value::Array(
            items
                .into_iter()
                .map(|item| snake_sample(api, inner, item))
                .collect(),
        ),
        (Ty::Map(inner), Value::Object(map)) => Value::Object(
            map.into_iter()
                .map(|(k, v)| (k, snake_sample(api, inner, v)))
                .collect(),
        ),
        (_, value) => value,
    }
}

/// Deterministic minimal sample value for a type: required fields only,
/// depth-limited, cycle-safe. Depth only limits recursion into container
/// shapes; scalars and enums always render a type-correct value, or strict
/// decoders (Go) choke on `{}` where a string/timestamp/enum belongs.
/// Which direction a sample renders for: requests omit readOnly fields,
/// responses omit writeOnly fields.
#[derive(Clone, Copy, PartialEq)]
pub enum SampleDir {
    Input,
    Output,
}

fn sample(api: &Api, ty: &Ty, depth: usize, dir: SampleDir) -> Value {
    const MAX_DEPTH: usize = 8;
    match ty {
        Ty::String => json!("sample"),
        Ty::Bool => json!(true),
        Ty::Int32 | Ty::Int64 => json!(1),
        Ty::Float | Ty::Double => json!(1.5),
        Ty::Timestamp => json!("2026-01-01T00:00:00Z"),
        Ty::Bytes => json!("aGVsbG8="),
        Ty::Json => json!({}),
        Ty::Literal(v) => json!(v),
        Ty::List(inner) => {
            if depth > MAX_DEPTH {
                json!([])
            } else {
                json!([sample(api, inner, depth + 1, dir)])
            }
        }
        Ty::Map(_) => json!({}),
        Ty::Named(name) => match api.types.get(name).map(|d| &d.shape) {
            Some(Shape::Struct(s)) => {
                if depth > MAX_DEPTH {
                    return json!({});
                }
                let mut map = Map::new();
                let keep = |f: &&Field| match dir {
                    SampleDir::Input => !f.read_only,
                    SampleDir::Output => !f.write_only,
                };
                for f in s
                    .fields
                    .iter()
                    .filter(keep)
                    .filter(|f| f.required && !f.nullable)
                {
                    map.insert(f.wire_name.clone(), sample(api, &f.ty, depth + 1, dir));
                }
                Value::Object(map)
            }
            // The first SPECIFIED value: an `*_UNSPECIFIED` member is the
            // protobuf zero, never a value a client should send.
            Some(Shape::Enum(e)) => json!(e
                .values
                .iter()
                .find(|v| !v.ends_with("_UNSPECIFIED"))
                .or(e.values.first())
                .cloned()
                .unwrap_or_default()),
            Some(Shape::Union(u)) => {
                if depth > MAX_DEPTH {
                    return json!({});
                }
                u.variants
                    .first()
                    .map(|v| sample(api, &v.ty, depth + 1, dir))
                    .unwrap_or(json!({}))
            }
            // Also depth-capped: an alias cycle would otherwise recurse
            // through this arm forever (it never reaches the guarded arms).
            Some(Shape::Alias(inner)) => {
                if depth > MAX_DEPTH {
                    json!({})
                } else {
                    sample(api, &inner.clone(), depth + 1, dir)
                }
            }
            None => json!({}),
        },
    }
}
