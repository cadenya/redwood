//! api.md backend: a Markdown projection of the same IR the SDKs are built
//! from, so docs can never drift from the generated code.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use heck::{ToLowerCamelCase, ToUpperCamelCase};

use crate::backends::{Backend, FileSet};
use crate::ir::*;

pub struct DocsBackend;

impl Backend for DocsBackend {
    fn name(&self) -> &'static str {
        "docs"
    }

    fn generate(&self, api: &Api) -> anyhow::Result<FileSet> {
        let mut files = FileSet::new();
        files.insert("api.md".into(), render(api));
        Ok(files)
    }
}

pub fn render(api: &Api) -> String {
    let mut out = String::new();
    writeln!(out, "# {} API\n", api.name).unwrap();
    for resource in &api.resources {
        let heading: String = resource
            .path()
            .split('.')
            .map(|part| part.to_upper_camel_case())
            .collect::<Vec<_>>()
            .join(".");
        writeln!(out, "# {heading}\n").unwrap();
        if let Some(d) = &resource.description {
            writeln!(out, "{}\n", d.trim()).unwrap();
        }
        let types = resource_types(resource);
        if !types.is_empty() {
            writeln!(out, "Types:\n").unwrap();
            for ty in &types {
                writeln!(out, "- <code>{ty}</code>").unwrap();
            }
            out.push('\n');
        }
        writeln!(out, "Methods:\n").unwrap();
        for op in &resource.operations {
            writeln!(out, "- {}", method_line(api, resource, op)).unwrap();
        }
        out.push('\n');
    }
    if !api.webhooks.is_empty() {
        writeln!(out, "# Webhooks\n").unwrap();
        writeln!(out, "Methods:\n").unwrap();
        writeln!(
            out,
            "- <code>client.webhooks.unwrap(payload, headers) -> UnwrapWebhookEvent</code>\n"
        )
        .unwrap();
        writeln!(out, "Events:\n").unwrap();
        for webhook in &api.webhooks {
            let summary = webhook
                .summary
                .as_deref()
                .or(webhook.description.as_deref())
                .unwrap_or_default();
            writeln!(
                out,
                "- <code>{}</code> -> {} — {}",
                webhook.name,
                ty_label(&webhook.payload),
                summary,
            )
            .unwrap();
        }
        out.push('\n');
    }
    out
}

fn resource_types(resource: &Resource) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for op in &resource.operations {
        let ty = match (&op.pagination, &op.response) {
            (Some(page), _) => Some(&page.item_ty),
            (None, ResponseKind::Json(ty) | ResponseKind::Sse(ty)) => Some(ty),
            (None, ResponseKind::Empty) => None,
        };
        if let Some(Ty::Named(n)) = ty {
            names.insert(n.clone());
        }
    }
    names
}

fn method_line(api: &Api, resource: &Resource, op: &Operation) -> String {
    let mut args: Vec<String> = Vec::new();
    for p in &op.positionals {
        args.push(p.wire_name.to_lower_camel_case());
    }
    if op.has_params() {
        args.push("{ ...params }".to_string());
    }
    let return_ty = match (&op.pagination, &op.response) {
        (Some(page), _) => format!("Page&lt;{}&gt;", ty_label(&page.item_ty)),
        (None, ResponseKind::Json(ty)) => ty_label(ty),
        (None, ResponseKind::Sse(ty)) => format!("Stream&lt;{}&gt;", ty_label(ty)),
        (None, ResponseKind::Empty) => "void".to_string(),
    };
    format!(
        "<code title=\"{method} {path}\">client.{res}.{name}({args}) -> {return_ty}</code>",
        method = op.http_method.as_str().to_lowercase(),
        path = op.path,
        res = resource
            .path()
            .split('.')
            .map(|part| part.to_lower_camel_case())
            .collect::<Vec<_>>()
            .join("."),
        name = op.name.to_lower_camel_case(),
        args = args.join(", "),
        // api.name unused today, kept for host-language variants later.
        return_ty = return_ty,
    )
    .replace("{api}", &api.name)
}

fn ty_label(ty: &Ty) -> String {
    match ty {
        Ty::String | Ty::Timestamp | Ty::Bytes => "string".into(),
        Ty::Bool => "boolean".into(),
        Ty::Int32 | Ty::Int64 | Ty::Float | Ty::Double => "number".into(),
        Ty::Json => "unknown".into(),
        Ty::Literal(v) => format!("'{v}'"),
        Ty::Named(n) => n.clone(),
        Ty::List(inner) => format!("Array&lt;{}&gt;", ty_label(inner)),
        Ty::Map(inner) => format!("Record&lt;string, {}&gt;", ty_label(inner)),
    }
}
