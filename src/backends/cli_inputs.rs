//! CLI request-body inputs: everything the CLI backend emits for the
//! flattened flags planned by [`crate::ir::plan`] — flag declarations, the
//! body-assembly action, usage grammar, schema bullets, and the embedded
//! request schema the runtime validates against.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use anyhow::Result;
use heck::ToKebabCase;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};

use super::cli::{escape_go, first_line, flag_name, go_quote};
use crate::config::CliConfig;
use crate::ir::plan::{body_plan, BodyPlan, Input, InputKind, PlanOptions, UnionPlan};
use crate::ir::*;

/// Body plans keyed by operation id.
pub type Plans = BTreeMap<String, BodyPlan>;

/// Plan options for one operation: the global [lang.cli.body] settings plus
/// that operation's renames, reserving its parameter flag names.
pub fn plan_options(config: &CliConfig, op: &Operation) -> PlanOptions {
    let defaults = PlanOptions::default();
    let body = &config.body;
    let mut rename = IndexMap::new();
    for (key, renames) in &body.rename {
        if crate::config::key_matches(key, op) {
            for (path, seg) in renames {
                rename.insert(path.clone(), seg.clone());
            }
        }
    }
    let mut reserved: Vec<String> = op
        .path_params
        .iter()
        .chain(op.query_params.iter())
        .map(|p| flag_name(&p.wire_name))
        .collect();
    if matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_))) {
        reserved.push("last-event-id".into());
    }
    PlanOptions {
        elide: body.elide.clone().unwrap_or(defaults.elide),
        singular: body.singular.clone(),
        rename,
        enum_short_forms: body.enum_short_forms.unwrap_or(defaults.enum_short_forms),
        reserved,
    }
}

/// Plan every operation's body up front so naming collisions and unused
/// renames fail generation with the operation named.
pub fn plan_all(api: &Api, config: &CliConfig) -> Result<Plans> {
    for key in config.body.rename.keys() {
        let matched = api
            .resources
            .iter()
            .flat_map(|r| r.operations.iter())
            .any(|op| crate::config::key_matches(key, op));
        anyhow::ensure!(
            matched,
            "[lang.cli.body.rename] {key:?} matches no operation (use \"<method> <path>\" or an operationId)"
        );
    }
    let mut plans = Plans::new();
    for op in api.resources.iter().flat_map(|r| r.operations.iter()) {
        let opts = plan_options(config, op);
        if let Some(plan) = body_plan(api, op, &opts).map_err(|e| {
            anyhow::anyhow!("{} ({} {}): {e}", op.id, op.http_method.as_str(), op.path)
        })? {
            plans.insert(op.id.clone(), plan);
        }
    }
    Ok(plans)
}

/// Go constant holding an operation's embedded request schema.
pub fn schema_const_name(resource: &Resource, op: &Operation) -> String {
    format!(
        "bodySchema{}{}",
        super::golang::exported_name(&resource.ident),
        super::golang::exported_name(&op.name)
    )
}

// ---- flag declarations --------------------------------------------------------

fn go_string_list(items: &[String]) -> String {
    let quoted: Vec<String> = items
        .iter()
        .map(|s| format!("\"{}\"", escape_go(s)))
        .collect();
    format!("[]string{{{}}}", quoted.join(", "))
}

fn path_literal(path: &[String]) -> String {
    go_string_list(path)
}

fn choices(input: &Input) -> Option<String> {
    let values = input.enum_values.as_ref()?;
    Some(match &input.enum_short {
        Some(short) => short
            .iter()
            .map(|(s, _)| s.clone())
            .collect::<Vec<_>>()
            .join(", "),
        None => values.join(", "),
    })
}

/// Help text for one input: requiredness, the field's description, and
/// the value form the input takes.
fn usage_text(input: &Input, plan: &BodyPlan) -> String {
    let mut parts: Vec<String> = Vec::new();
    // A document is one way to supply a subtree, never the only one.
    if input.required && !matches!(input.kind, InputKind::Doc(_)) {
        parts.push("Required.".into());
    }
    let desc = input
        .description
        .as_deref()
        .map(first_line)
        .unwrap_or_default();
    let desc = desc.trim_end_matches('.').to_string();
    if !desc.is_empty() {
        parts.push(format!("{desc}."));
    }
    let form = match &input.kind {
        InputKind::Leaf(Ty::Timestamp) => "RFC 3339 timestamp.".to_string(),
        InputKind::Leaf(_) => match choices(input) {
            Some(c) => format!("One of: {c}."),
            None => String::new(),
        },
        InputKind::KvMap(_) => match choices(input) {
            Some(c) => format!("KEY=VALUE with VALUE one of: {c} (repeatable; or a document)."),
            None => "KEY=VALUE (repeatable; or a document).".to_string(),
        },
        InputKind::ScalarList(_) => match choices(input) {
            Some(c) => format!("One of: {c} (repeatable)."),
            None => "Repeatable.".to_string(),
        },
        InputKind::Doc(_) => "YAML/JSON document (literal, @path, or - for stdin).".to_string(),
        InputKind::EntryDoc(_) => {
            "KEY=VALUE, KEY:=JSON, or a YAML/JSON document (repeatable).".to_string()
        }
        InputKind::DocList(_) => {
            "One YAML/JSON document per occurrence (literal, @path, or -).".to_string()
        }
        InputKind::ShorthandList { fields, .. } => {
            let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
            let pair = match &input.pair_form {
                Some(_) => " NAME=VALUE is also accepted.",
                None => "",
            };
            format!(
                "key=value,... over {} (repeatable; or a document).{pair}",
                keys.join(", ")
            )
        }
        InputKind::UnionTag => {
            let union = &plan.unions[input.union.expect("tag input")];
            let tags: Vec<String> = union.arms.iter().map(|a| a.tag.to_kebab_case()).collect();
            let inferred = if union.inferable {
                "; inferred from the arm's flags"
            } else {
                ""
            };
            format!(
                "One of: {}{inferred}. Or a YAML/JSON document.",
                tags.join(", ")
            )
        }
    };
    if !form.is_empty() {
        parts.push(form);
    }
    parts.join(" ")
}

/// Emit the flag declarations for a body plan (plus the source flags).
pub fn emit_flags(plan: &BodyPlan, out: &mut String) {
    for input in &plan.inputs {
        let usage = escape_go(&usage_text(input, plan));
        let category = if input.category.is_empty() {
            String::new()
        } else {
            format!(", Category: \"{}\"", escape_go(&input.category))
        };
        let name = &input.flag;
        let decl = match &input.kind {
            InputKind::Leaf(Ty::Bool) => {
                format!("&cli.BoolFlag{{Name: \"{name}\", Usage: \"{usage}\"{category}}}")
            }
            InputKind::Leaf(Ty::Int32) | InputKind::Leaf(Ty::Int64) => {
                format!("&cli.IntFlag{{Name: \"{name}\", Usage: \"{usage}\"{category}}}")
            }
            InputKind::Leaf(Ty::Float) | InputKind::Leaf(Ty::Double) => {
                format!("&cli.FloatFlag{{Name: \"{name}\", Usage: \"{usage}\"{category}}}")
            }
            InputKind::Leaf(_) | InputKind::UnionTag => {
                format!("&cli.StringFlag{{Name: \"{name}\", Usage: \"{usage}\"{category}}}")
            }
            InputKind::Doc(_) => {
                format!("&cli.StringFlag{{Name: \"{name}\", Usage: \"{usage}\", TakesFile: true{category}}}")
            }
            _ => format!("&cli.StringSliceFlag{{Name: \"{name}\", Usage: \"{usage}\"{category}}}"),
        };
        writeln!(out, "\t\t\t\t\t{decl},").unwrap();
    }
    writeln!(
        out,
        "\t\t\t\t\t&cli.StringFlag{{Name: \"file\", Aliases: []string{{\"f\"}}, TakesFile: true, Usage: \"Whole request body from a YAML/JSON file (or - for stdin); other flags override its values\"}},"
    )
    .unwrap();
    writeln!(
        out,
        "\t\t\t\t\t&cli.BoolFlag{{Name: \"dry-run\", Usage: \"Print the assembled request body (YAML; JSON with --display json) and exit without calling the API\"}},"
    )
    .unwrap();
    writeln!(
        out,
        "\t\t\t\t\t&cli.BoolFlag{{Name: \"strict\", Usage: \"Reject fields the request does not accept in --file and document inputs instead of dropping them with a warning\"}},"
    )
    .unwrap();
}

// ---- action -------------------------------------------------------------------

/// Flags whose single value may be `-` (stdin), and repeatable flags whose
/// occurrences may be.
pub fn stdin_inputs(plan: &BodyPlan) -> (Vec<String>, Vec<String>) {
    let mut singles = vec!["file".to_string()];
    let mut slices = Vec::new();
    for input in plan.stdin_capable() {
        if BodyPlan::repeatable(&input.kind) {
            slices.push(input.flag.clone());
        } else {
            singles.push(input.flag.clone());
        }
    }
    (singles, slices)
}

fn enum_literal(values: &[String], short: &Option<Vec<(String, String)>>) -> String {
    let shorts: Vec<String> = short
        .as_ref()
        .map(|s| s.iter().map(|(short, _)| short.clone()).collect())
        .unwrap_or_default();
    format!(
        "enumSpec{{Values: {}, Short: {}}}",
        go_string_list(values),
        go_string_list(&shorts)
    )
}

fn scalar_kind(ty: &Ty) -> &'static str {
    match ty {
        Ty::Bool => "scalarBool",
        Ty::Int32 | Ty::Int64 => "scalarInt",
        Ty::Float | Ty::Double => "scalarFloat",
        _ => "scalarString",
    }
}

fn enum_arg(input_values: &Option<Vec<String>>, short: &Option<Vec<(String, String)>>) -> String {
    match input_values {
        Some(values) => format!("&{}", enum_literal(values, short)),
        None => "nil".to_string(),
    }
}

fn union_literal(plan: &BodyPlan, union: &UnionPlan) -> String {
    let parent = match &union.parent_arm {
        Some(arm) => {
            let parent = &plan.unions[arm.union];
            format!(
                ", Parent: &unionParent{{Path: {}, Discriminator: \"{}\", Tag: \"{}\"}}",
                path_literal(&parent.path),
                escape_go(&parent.discriminator),
                escape_go(&arm.tag)
            )
        }
        None => String::new(),
    };
    let arms: Vec<String> = union
        .arms
        .iter()
        .map(|a| {
            format!(
                "{{Tag: \"{}\", Keys: {}, Init: {}}}",
                escape_go(&a.tag),
                go_string_list(&a.keys),
                go_string_list(&a.init_objects)
            )
        })
        .collect();
    format!(
        "unionSpec{{Flag: \"{}\", Path: {}, Discriminator: \"{}\", Required: {}, Inferable: {}{parent}, Arms: []unionArm{{{}}}}}",
        union.flag,
        path_literal(&union.path),
        escape_go(&union.discriminator),
        union.required,
        union.inferable,
        arms.join(", ")
    )
}

const EXIT: &str =
    "\t\t\t\t\t\tif err != nil {\n\t\t\t\t\t\t\treturn cli.Exit(err.Error(), 2)\n\t\t\t\t\t\t}\n";

fn guarded(out: &mut String, flag: &str, body: &str) {
    writeln!(out, "\t\t\t\t\tif cmd.IsSet(\"{flag}\") {{").unwrap();
    out.push_str(body);
    writeln!(out, "\t\t\t\t\t}}").unwrap();
}

fn checked(call: &str) -> String {
    format!("\t\t\t\t\t\tif err := {call}; err != nil {{\n\t\t\t\t\t\t\treturn cli.Exit(err.Error(), 2)\n\t\t\t\t\t\t}}\n")
}

/// Emit the body-assembly section of a command action. On return the Go
/// code has a validated `_body` builder; `_rawBody` (non-nil for a
/// non-object whole body) or `_body.body` is the request body.
pub fn emit_action(op: &Operation, plan: &BodyPlan, schema_const: &str, out: &mut String) {
    writeln!(out, "\t\t\t\t\t_schema := parseBodySchema({schema_const})").unwrap();
    writeln!(out, "\t\t\t\t\t_body := newBodyBuilder()").unwrap();
    writeln!(out, "\t\t\t\t\t_strict := cmd.Bool(\"strict\")").unwrap();
    writeln!(out, "\t\t\t\t\tvar _rawBody any").unwrap();
    guarded(
        out,
        "file",
        &checked("_body.applyFile(\"file\", cmd.String(\"file\"), _schema, _strict)"),
    );
    // Documents first (outer before inner is the plan's walk order), then
    // union tags, then leaves and collections: deeper inputs win.
    for input in &plan.inputs {
        let flag = &input.flag;
        let path = path_literal(&input.path);
        match &input.kind {
            InputKind::Doc(_) if input.path.is_empty() => {
                let mut body = String::new();
                writeln!(
                    body,
                    "\t\t\t\t\t\t_doc, err := docArg(\"{flag}\", cmd.String(\"{flag}\"))"
                )
                .unwrap();
                body.push_str(EXIT);
                writeln!(
                    body,
                    "\t\t\t\t\t\tif _obj, ok := _doc.(map[string]any); ok {{"
                )
                .unwrap();
                writeln!(body, "\t\t\t\t\t\t\tif err := _body.merge(\"{flag}\", nil, _obj); err != nil {{\n\t\t\t\t\t\t\t\treturn cli.Exit(err.Error(), 2)\n\t\t\t\t\t\t\t}}").unwrap();
                writeln!(
                    body,
                    "\t\t\t\t\t\t}} else {{\n\t\t\t\t\t\t\t_rawBody = _doc\n\t\t\t\t\t\t}}"
                )
                .unwrap();
                guarded(out, flag, &body);
            }
            InputKind::Doc(_) => guarded(
                out,
                flag,
                &checked(&format!(
                    "_body.applyDoc(\"{flag}\", {path}, cmd.String(\"{flag}\"), _schema, _strict)"
                )),
            ),
            InputKind::UnionTag => {
                let union = &plan.unions[input.union.expect("tag input")];
                guarded(
                    out,
                    flag,
                    &checked(&format!(
                        "_body.applyUnionFlag({}, cmd.String(\"{flag}\"), _schema, _strict)",
                        union_literal(plan, union)
                    )),
                );
            }
            _ => {}
        }
    }
    for input in &plan.inputs {
        let flag = &input.flag;
        let path = path_literal(&input.path);
        match &input.kind {
            InputKind::Leaf(ty) => {
                let mut body = String::new();
                match (ty, &input.enum_values) {
                    (_, Some(values)) => {
                        writeln!(
                            body,
                            "\t\t\t\t\t\t_v, err := {}.parse(\"{flag}\", cmd.String(\"{flag}\"))",
                            enum_literal(values, &input.enum_short)
                        )
                        .unwrap();
                        body.push_str(EXIT);
                        body.push_str(&checked(&format!("_body.set(\"{flag}\", {path}, _v)")));
                    }
                    (Ty::Bool, _) => body.push_str(&checked(&format!("_body.set(\"{flag}\", {path}, cmd.Bool(\"{flag}\"))"))),
                    (Ty::Int32 | Ty::Int64, _) => body.push_str(&checked(&format!("_body.set(\"{flag}\", {path}, cmd.Int(\"{flag}\"))"))),
                    (Ty::Float | Ty::Double, _) => body.push_str(&checked(&format!("_body.set(\"{flag}\", {path}, cmd.Float(\"{flag}\"))"))),
                    (Ty::String | Ty::Bytes, _) => {
                        writeln!(body, "\t\t\t\t\t\t_v, err := stringArg(\"{flag}\", cmd.String(\"{flag}\"))").unwrap();
                        body.push_str(EXIT);
                        body.push_str(&checked(&format!("_body.set(\"{flag}\", {path}, _v)")));
                    }
                    _ => body.push_str(&checked(&format!("_body.set(\"{flag}\", {path}, cmd.String(\"{flag}\"))"))),
                }
                guarded(out, flag, &body);
            }
            InputKind::KvMap(ty) => guarded(
                out,
                flag,
                &checked(&format!(
                    "_body.applyEntries(\"{flag}\", {path}, cmd.StringSlice(\"{flag}\"), {}, {})",
                    scalar_kind(ty),
                    enum_arg(&input.enum_values, &input.enum_short)
                )),
            ),
            InputKind::ScalarList(ty) => guarded(
                out,
                flag,
                &checked(&format!(
                    "_body.applyScalarItems(\"{flag}\", {path}, cmd.StringSlice(\"{flag}\"), {}, {})",
                    scalar_kind(ty),
                    enum_arg(&input.enum_values, &input.enum_short)
                )),
            ),
            InputKind::EntryDoc(_) => guarded(
                out,
                flag,
                &checked(&format!(
                    "_body.applyEntryDocs(\"{flag}\", {path}, cmd.StringSlice(\"{flag}\"))"
                )),
            ),
            InputKind::DocList(_) => guarded(
                out,
                flag,
                &checked(&format!(
                    "_body.applyDocItems(\"{flag}\", {path}, cmd.StringSlice(\"{flag}\"))"
                )),
            ),
            InputKind::ShorthandList { fields, .. } => {
                let field_lits: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        format!(
                            "{{Wire: \"{}\", Key: \"{}\", Kind: {}, Enum: {}, Required: {}}}",
                            escape_go(&f.wire_name),
                            f.key,
                            scalar_kind(&f.ty),
                            enum_arg(&f.enum_values, &f.enum_short),
                            f.required
                        )
                    })
                    .collect();
                let pair = match &input.pair_form {
                    Some((k, v)) => format!(", PairKey: \"{}\", PairValue: \"{}\"", escape_go(k), escape_go(v)),
                    None => String::new(),
                };
                guarded(
                    out,
                    flag,
                    &checked(&format!(
                        "_body.applyShorthandItems(\"{flag}\", {path}, cmd.StringSlice(\"{flag}\"), shorthandSpec{{Fields: []shorthandField{{{}}}{pair}}})",
                        field_lits.join(", ")
                    )),
                );
            }
            InputKind::Doc(_) | InputKind::UnionTag => {}
        }
    }
    for union in &plan.unions {
        out.push_str(
            &checked(&format!(
                "_body.resolveUnion({})",
                union_literal(plan, union)
            ))
            .replace("\t\t\t\t\t\t", "\t\t\t\t\t"),
        );
    }
    let flag_for: Vec<String> = plan
        .inputs
        .iter()
        .filter(|i| !i.path.is_empty())
        .map(|i| format!("\"{}\": \"--{}\"", escape_go(&i.path.join(".")), i.flag))
        .collect();
    writeln!(
        out,
        "\t\t\t\t\tif err := _body.finish(_schema, map[string]string{{{}}}); err != nil {{\n\t\t\t\t\t\treturn cli.Exit(err.Error(), 2)\n\t\t\t\t\t}}",
        flag_for.join(", ")
    )
    .unwrap();
    if let Some(mask) = &op.update_mask {
        if let Some(input) = plan.inputs.iter().find(|i| i.path == [mask.clone()]) {
            writeln!(
                out,
                "\t\t\t\t\t// A partial update names the paths it changes; a mask supplied\n\t\t\t\t\t// by flag or document wins.\n\t\t\t\t\tif _, _given := _body.lookup({path}); !_given {{\n\t\t\t\t\t\tif _mask := _body.updateMask(\"{mask}\"); _mask != \"\" {{\n\t\t\t\t\t\t\t_ = _body.set(\"{flag}\", {path}, _mask)\n\t\t\t\t\t\t}}\n\t\t\t\t\t}}",
                flag = input.flag,
                mask = escape_go(mask),
                path = path_literal(&input.path)
            )
            .unwrap();
        }
    }
    writeln!(
        out,
        "\t\t\t\t\tif cmd.Bool(\"dry-run\") {{\n\t\t\t\t\t\tif _rawBody != nil {{\n\t\t\t\t\t\t\treturn printDocument(_display, _rawBody)\n\t\t\t\t\t\t}}\n\t\t\t\t\t\treturn printDocument(_display, _body.body)\n\t\t\t\t\t}}"
    )
    .unwrap();
}

/// Emit the merge of the assembled body into the params map.
pub fn emit_merge_into_values(plan: &BodyPlan, out: &mut String) {
    if plan.whole_body {
        writeln!(
            out,
            "\t\t\t\t\tif _rawBody != nil {{\n\t\t\t\t\t\tvalues[\"body\"] = _rawBody\n\t\t\t\t\t}} else {{\n\t\t\t\t\t\tvalues[\"body\"] = _body.body\n\t\t\t\t\t}}"
        )
        .unwrap();
    } else {
        writeln!(
            out,
            "\t\t\t\t\t_ = _rawBody\n\t\t\t\t\tfor _k, _v := range _body.body {{\n\t\t\t\t\t\tvalues[_k] = _v\n\t\t\t\t\t}}"
        )
        .unwrap();
    }
}

// ---- grammar and docs ---------------------------------------------------------

/// Usage grammar for one input, faithful to the parser.
pub fn input_grammar(input: &Input, plan: &BodyPlan) -> String {
    let flag = &input.flag;
    let core = match &input.kind {
        InputKind::Leaf(Ty::Bool) => format!("--{flag}[=true|false]"),
        InputKind::Leaf(_) | InputKind::ScalarList(_) => match &input.enum_short {
            Some(short) if short.len() <= 6 => format!(
                "--{flag} <{}>",
                short
                    .iter()
                    .map(|(s, _)| s.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            ),
            _ => format!("--{flag} <value>"),
        },
        InputKind::KvMap(_) => format!("--{flag} KEY=VALUE"),
        InputKind::Doc(_) | InputKind::DocList(_) => format!("--{flag} <doc>"),
        InputKind::EntryDoc(_) => format!("--{flag} KEY=VALUE|<doc>"),
        InputKind::ShorthandList { .. } => format!("--{flag} k=v,...|<doc>"),
        InputKind::UnionTag => {
            let union = &plan.unions[input.union.expect("tag input")];
            format!(
                "--{flag} <{}>",
                union
                    .arms
                    .iter()
                    .map(|a| a.tag.to_kebab_case())
                    .collect::<Vec<_>>()
                    .join("|")
            )
        }
    };
    let repeat = BodyPlan::repeatable(&input.kind);
    // Inside an arm, requiredness is conditional on the arm: bracketed.
    let required = input.required && input.arm.is_none();
    match (required, repeat) {
        (true, true) => format!("{core}..."),
        (true, false) => core,
        (false, true) => format!("[{core}]..."),
        (false, false) => format!("[{core}]"),
    }
}

/// The whole body's grammar for a usage line: leaf inputs (documents are
/// listed in the reference, not the one-line grammar) plus the source flags.
pub fn body_grammar(plan: &BodyPlan) -> String {
    let mut parts: Vec<String> = plan
        .inputs
        .iter()
        .filter(|i| !matches!(i.kind, InputKind::Doc(_)) || i.path.is_empty())
        .map(|i| input_grammar(i, plan))
        .collect();
    parts.push("[-f <doc>]".into());
    parts.push("[--dry-run]".into());
    parts.join(" ")
}

/// One reference bullet per input for `schema` markdown, registering the
/// named types documents refer to.
pub fn flag_bullets(api: &Api, plan: &BodyPlan, defs: &mut IndexMap<String, Value>) -> Vec<String> {
    let mut bullets = Vec::new();
    for input in &plan.inputs {
        let mut bullet = format!("- `{}`", input_grammar(input, plan));
        if input.required && !matches!(input.kind, InputKind::Doc(_)) {
            bullet.push_str(if input.arm.is_some() {
                " (required in its arm)"
            } else {
                " (required)"
            });
        }
        let desc = input
            .description
            .as_deref()
            .map(first_line)
            .unwrap_or_default();
        if !desc.is_empty() {
            bullet.push_str(" — ");
            bullet.push_str(&desc);
        }
        if !input.path.is_empty() {
            bullet.push_str(&format!(" Path: `{}`.", input.path.join(".")));
        }
        if !input.category.is_empty() {
            bullet.push_str(&format!(" Arm: `{}`.", input.category));
        }
        match &input.kind {
            InputKind::Doc(ty) | InputKind::DocList(ty) | InputKind::EntryDoc(ty) => {
                if let Ty::Named(name) = ty {
                    ty_schema(api, ty, defs);
                    bullet.push_str(&format!(" Document — see `{name}` under Types."));
                } else if let Ty::List(inner) | Ty::Map(inner) = ty {
                    if let Ty::Named(name) = inner.as_ref() {
                        ty_schema(api, inner, defs);
                        bullet.push_str(&format!(" Items — see `{name}` under Types."));
                    }
                }
            }
            InputKind::UnionTag => {
                let union = &plan.unions[input.union.expect("tag input")];
                let arms: Vec<String> = union
                    .arms
                    .iter()
                    .map(|a| format!("`{}` ({})", a.tag.to_kebab_case(), a.variant))
                    .collect();
                for a in &union.arms {
                    ty_schema(api, &Ty::Named(a.variant.clone()), defs);
                }
                bullet.push_str(&format!(" Arms: {}.", arms.join(", ")));
            }
            _ => {
                if let Some(c) = choices(input) {
                    bullet.push_str(&format!(" One of: {c}."));
                }
            }
        }
        bullets.push(bullet);
    }
    bullets.push("- `-f, --file <path>` — the whole request body from a YAML/JSON file (`-` for stdin); flags override its values.".into());
    bullets.push("- `--dry-run` — print the assembled body instead of sending it.".into());
    bullets.push("- `--strict` — reject unknown fields in documents.".into());
    bullets
}

// ---- request schema -----------------------------------------------------------

/// JSON Schema (request direction) for a type; named types land in `defs`
/// and are referenced by bare name.
pub fn ty_schema(api: &Api, ty: &Ty, defs: &mut IndexMap<String, Value>) -> Value {
    match ty {
        Ty::String => json!({"type": "string"}),
        Ty::Bool => json!({"type": "boolean"}),
        Ty::Int32 | Ty::Int64 => json!({"type": "integer"}),
        Ty::Float | Ty::Double => json!({"type": "number"}),
        Ty::Timestamp => json!({"type": "string", "format": "date-time"}),
        Ty::Bytes => json!({"type": "string", "format": "byte"}),
        Ty::Json => json!({}),
        Ty::Literal(v) => json!({"const": v}),
        Ty::List(inner) => json!({"type": "array", "items": ty_schema(api, inner, defs)}),
        Ty::Map(inner) => {
            json!({"type": "object", "additionalProperties": ty_schema(api, inner, defs)})
        }
        Ty::Named(name) => {
            if !defs.contains_key(name) {
                // Reserve the slot first: self-referential types must find
                // their own name present and stop recursing.
                defs.insert(name.clone(), Value::Null);
                let schema = match api.types.get(name) {
                    Some(decl) => decl_schema(api, &decl.shape, defs),
                    None => json!({}),
                };
                defs.insert(name.clone(), schema);
            }
            json!({"$ref": name})
        }
    }
}

pub fn decl_schema(api: &Api, shape: &Shape, defs: &mut IndexMap<String, Value>) -> Value {
    match shape {
        Shape::Struct(st) => {
            let mut properties = Map::new();
            let mut required = Vec::new();
            // Request direction: readOnly fields are server-owned and never
            // accepted as input.
            for f in st.input_fields() {
                let mut s = ty_schema(api, &f.ty, defs);
                if let (Some(desc), Some(obj)) = (&f.description, s.as_object_mut()) {
                    obj.entry("description")
                        .or_insert_with(|| Value::String(desc.clone()));
                }
                if f.required {
                    required.push(Value::String(f.wire_name.clone()));
                }
                properties.insert(f.wire_name.clone(), s);
            }
            let mut obj = Map::new();
            obj.insert("type".into(), json!("object"));
            obj.insert("properties".into(), Value::Object(properties));
            if !required.is_empty() {
                obj.insert("required".into(), Value::Array(required));
            }
            if let Some(additional) = &st.additional {
                obj.insert(
                    "additionalProperties".into(),
                    ty_schema(api, additional, defs),
                );
            }
            Value::Object(obj)
        }
        Shape::Enum(e) => {
            // Accepted input values (never `*_UNSPECIFIED`) and the short
            // forms the CLI expands.
            let (values, short) = crate::ir::plan::enum_forms(&e.values, true);
            let mut obj = Map::new();
            obj.insert("type".into(), json!("string"));
            obj.insert("enum".into(), json!(values));
            if let Some(short) = short {
                let table: Map<String, Value> = short
                    .into_iter()
                    .map(|(s, full)| (s, Value::String(full)))
                    .collect();
                obj.insert("enumShort".into(), Value::Object(table));
            }
            Value::Object(obj)
        }
        Shape::Union(u) => {
            let variants: Vec<Value> = u
                .variants
                .iter()
                .map(|v| ty_schema(api, &v.ty, defs))
                .collect();
            let mut obj = Map::new();
            obj.insert("oneOf".into(), Value::Array(variants));
            if let Some(d) = &u.discriminator {
                obj.insert("discriminator".into(), json!({"propertyName": d.property}));
            }
            Value::Object(obj)
        }
        Shape::Alias(inner) => ty_schema(api, inner, defs),
    }
}

/// The embedded request schema for an operation's body: a root object over
/// its body fields (or the whole-body type) with `$defs`.
pub fn request_schema_json(api: &Api, op: &Operation) -> String {
    let mut defs: IndexMap<String, Value> = IndexMap::new();
    let mut root = match &op.whole_body {
        Some(ty) => ty_schema(api, ty, &mut defs),
        None => {
            let mut properties = Map::new();
            let mut required = Vec::new();
            for f in &op.body_fields {
                properties.insert(f.wire_name.clone(), ty_schema(api, &f.ty, &mut defs));
                if f.required {
                    required.push(Value::String(f.wire_name.clone()));
                }
            }
            let mut obj = Map::new();
            obj.insert("type".into(), json!("object"));
            obj.insert("properties".into(), Value::Object(properties));
            if !required.is_empty() {
                obj.insert("required".into(), Value::Array(required));
            }
            Value::Object(obj)
        }
    };
    if let Some(obj) = root.as_object_mut() {
        let defs_map: Map<String, Value> = defs.into_iter().collect();
        obj.insert("$defs".into(), Value::Object(defs_map));
    }
    serde_json::to_string(&root).expect("schema serializes")
}

/// A Go `const` declaration holding the embedded schema.
pub fn emit_schema_const(name: &str, schema_json: &str) -> String {
    format!("const {name} = {}\n", go_quote(schema_json))
}

// ---- samples ------------------------------------------------------------------

/// Shell-quote a value for a documentation sample.
fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '=')
        })
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

fn sample_scalar(
    api: &Api,
    ty: &Ty,
    input_enum: &Option<Vec<(String, String)>>,
    values: &Option<Vec<String>>,
) -> String {
    if let Some(short) = input_enum {
        if let Some((s, _)) = short.first() {
            return s.clone();
        }
    }
    if let Some(values) = values {
        if let Some(v) = values.first() {
            return v.clone();
        }
    }
    match super::manifest_sample(api, ty) {
        Value::String(s) => s,
        other => other.to_string(),
    }
}

/// The minimal flag list a documentation sample needs: every required
/// input in the first arm of each union, rendered the way a user types it.
/// Accepted by the CLI's preflight, so samples are executable.
pub fn sample_args(api: &Api, plan: &BodyPlan) -> Vec<String> {
    let first_tag = |union: usize| plan.unions[union].arms.first().map(|a| a.tag.clone());
    let in_first_arms = |input: &Input| {
        let mut current = input.arm.clone();
        while let Some(arm) = current {
            if first_tag(arm.union).as_deref() != Some(arm.tag.as_str()) {
                return false;
            }
            current = plan.unions[arm.union].parent_arm.clone();
        }
        true
    };
    let mut args: Vec<String> = Vec::new();
    let mut covered_unions: Vec<usize> = Vec::new();
    for input in &plan.inputs {
        if !input.required || !in_first_arms(input) {
            continue;
        }
        let flag = format!("--{}", input.flag);
        let rendered: Vec<String> = match &input.kind {
            InputKind::Doc(_) | InputKind::UnionTag => continue,
            InputKind::Leaf(Ty::Bool) => vec![flag.clone()],
            InputKind::Leaf(ty) => vec![format!(
                "{flag} {}",
                shell_quote(&sample_scalar(
                    api,
                    ty,
                    &input.enum_short,
                    &input.enum_values
                ))
            )],
            InputKind::KvMap(ty) => vec![format!(
                "{flag} {}",
                shell_quote(&format!(
                    "key={}",
                    sample_scalar(api, ty, &input.enum_short, &input.enum_values)
                ))
            )],
            InputKind::ScalarList(ty) => vec![format!(
                "{flag} {}",
                shell_quote(&sample_scalar(
                    api,
                    ty,
                    &input.enum_short,
                    &input.enum_values
                ))
            )],
            InputKind::ShorthandList { item, fields } => {
                let sample = super::manifest_sample(api, item);
                let pairs: Vec<String> = fields
                    .iter()
                    .filter_map(|f| {
                        sample.get(&f.wire_name).map(|v| {
                            let value = match v {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            };
                            let value = f
                                .enum_short
                                .as_ref()
                                .and_then(|s| s.first().map(|(short, _)| short.clone()))
                                .unwrap_or(value);
                            format!("{}={value}", f.key)
                        })
                    })
                    .collect();
                if pairs.is_empty() {
                    vec![format!("{flag} '{{}}'")]
                } else {
                    vec![format!("{flag} {}", shell_quote(&pairs.join(",")))]
                }
            }
            InputKind::DocList(ty) => vec![format!(
                "{flag} {}",
                shell_quote(&super::manifest_sample(api, ty).to_string())
            )],
            InputKind::EntryDoc(ty) => vec![format!(
                "{flag} {}",
                shell_quote(&super::manifest_sample(api, ty).to_string())
            )],
        };
        if let Some(arm) = &input.arm {
            covered_unions.push(arm.union);
        }
        args.extend(rendered);
    }
    // A required union whose first arm contributed nothing needs its tag.
    for (index, union) in plan.unions.iter().enumerate() {
        if !union.required || covered_unions.contains(&index) {
            continue;
        }
        let parent_selected = union
            .parent_arm
            .as_ref()
            .is_none_or(|arm| first_tag(arm.union).as_deref() == Some(arm.tag.as_str()));
        if !parent_selected {
            continue;
        }
        if let Some(tag) = first_tag(index) {
            args.push(format!("--{} {}", union.flag, tag.to_kebab_case()));
        }
    }
    args
}
