//! Backends are projections of the IR. Each one implements `Backend` and
//! depends only on `ir::Api` — never on the OpenAPI document.

pub mod cli;
pub mod cli_inputs;
pub mod docs;
pub mod golang;
mod golang_testsuite;
pub mod manifest;
pub mod openapi_export;
pub mod python;
pub mod ruby;
mod ruby_rspec;
pub mod typescript;

pub use manifest::{manifest_sample, manifest_sample_output, snake_sample};

use std::collections::BTreeMap;

use crate::ir::{Api, Operation, Resource};

/// Structural pick of a representative request-bearing operation for doc
/// examples: prefer an op the IR named `create` with required body fields.
/// Backends must know nothing about any particular spec — the accessor,
/// field names, and sampled values all come from the IR.
pub(crate) fn doc_example_op(api: &Api) -> Option<(&Resource, &Operation)> {
    let candidates = api.resources.iter().flat_map(|r| {
        r.operations
            .iter()
            .filter(|o| o.body_fields.iter().any(|f| f.required))
            .map(move |o| (r, o))
    });
    let mut fallback = None;
    for (r, o) in candidates {
        if o.name == "create" {
            return Some((r, o));
        }
        fallback.get_or_insert((r, o));
    }
    fallback
}

/// Bound a sampled value for README display: objects keep their first
/// `keep` entries, arrays their first element (recursively). Examples stay
/// short; enum members stay real because they come from the sampler.
pub(crate) fn trim_doc_sample(value: &serde_json::Value, keep: usize) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.iter()
                .take(keep)
                .map(|(k, v)| (k.clone(), trim_doc_sample(v, keep)))
                .collect(),
        ),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .iter()
                .take(1)
                .map(|v| trim_doc_sample(v, keep))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A generated SDK: relative path -> file contents.
pub type FileSet = BTreeMap<String, String>;

pub trait Backend {
    fn name(&self) -> &'static str;
    fn generate(&self, api: &Api) -> anyhow::Result<FileSet>;
}
