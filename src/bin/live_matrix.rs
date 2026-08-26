use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_yaml::{Mapping, Value as YamlValue};
use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::PathBuf;

const SDKS: [&str; 5] = ["typescript", "go", "python", "ruby", "cli"];
const METHODS: [&str; 8] = [
    "get", "post", "put", "patch", "delete", "head", "options", "trace",
];

#[derive(Debug, Deserialize)]
struct Manifest {
    operations: Vec<ManifestOperation>,
}

#[derive(Debug, Deserialize)]
struct ManifestOperation {
    id: String,
    #[serde(rename = "httpMethod")]
    http_method: String,
    path: String,
}

#[derive(Debug, Default, Deserialize)]
struct EvidenceFile {
    #[serde(default)]
    operations: BTreeMap<String, BTreeMap<String, LiveEvidence>>,
}

#[derive(Debug, Clone, Deserialize)]
struct LiveEvidence {
    status: String,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug, Deserialize)]
struct ResultFile {
    sdk: String,
    #[serde(rename = "executedAt")]
    executed_at: String,
    operations: BTreeMap<String, ResultEntry>,
}

#[derive(Debug, Deserialize)]
struct ResultEntry {
    status: String,
    #[serde(default)]
    evidence: String,
}

#[derive(Debug)]
struct Operation {
    id: String,
    method: String,
    path: String,
    description: String,
}

fn main() -> Result<()> {
    let root = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or(env::current_dir().context("resolve current directory")?);
    let spec_path = root.join("api-spec.yml");
    let manifest_path = root.join("gen/manifest/manifest.json");
    let evidence_path = root.join("e2e/live-matrix/evidence.json");
    let output_path = root.join("e2e/live-matrix/api-endpoint-live-test-matrix.csv");

    let spec: YamlValue = serde_yaml::from_str(
        &fs::read_to_string(&spec_path).with_context(|| format!("read {}", spec_path.display()))?,
    )
    .with_context(|| format!("parse {}", spec_path.display()))?;
    let operations = extract_operations(&spec)?;

    let manifest: Manifest = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .with_context(|| format!("read {}", manifest_path.display()))?,
    )
    .with_context(|| format!("parse {}", manifest_path.display()))?;
    validate_manifest(&operations, &manifest)?;

    let mut evidence: EvidenceFile = serde_json::from_str(
        &fs::read_to_string(&evidence_path)
            .with_context(|| format!("read {}", evidence_path.display()))?,
    )
    .with_context(|| format!("parse {}", evidence_path.display()))?;
    merge_result_files(&root, &mut evidence)?;
    validate_evidence(&operations, &evidence)?;

    let csv = render_csv(&operations, &evidence);
    fs::write(&output_path, csv).with_context(|| format!("write {}", output_path.display()))?;
    println!(
        "wrote {} operations to {}",
        operations.len(),
        output_path.display()
    );
    Ok(())
}

fn merge_result_files(root: &PathBuf, evidence: &mut EvidenceFile) -> Result<()> {
    for sdk in SDKS {
        let result_path = root.join(format!("e2e/live-matrix/results-{sdk}.json"));
        if !result_path.exists() {
            continue;
        }
        let results: ResultFile = serde_json::from_str(
            &fs::read_to_string(&result_path)
                .with_context(|| format!("read {}", result_path.display()))?,
        )
        .with_context(|| format!("parse {}", result_path.display()))?;
        if results.sdk != sdk {
            bail!(
                "{} declares SDK {:?}, expected {:?}",
                result_path.display(),
                results.sdk,
                sdk
            );
        }
        for (operation_id, result) in results.operations {
            if !matches!(result.status.as_str(), "completed" | "failed" | "blocked") {
                bail!(
                    "invalid result status {:?} for {operation_id}/{sdk} in {}",
                    result.status,
                    result_path.display()
                );
            }
            let by_sdk = evidence.operations.entry(operation_id).or_default();
            let should_merge = result.status != "blocked" || !by_sdk.contains_key(sdk);
            if should_merge {
                by_sdk.insert(
                    sdk.to_owned(),
                    LiveEvidence {
                        status: result.status,
                        evidence: format!(
                            "{} ({}): {}",
                            result_path
                                .strip_prefix(root)
                                .unwrap_or(&result_path)
                                .display(),
                            results.executed_at,
                            result.evidence
                        ),
                    },
                );
            }
        }
    }
    Ok(())
}

fn extract_operations(spec: &YamlValue) -> Result<Vec<Operation>> {
    let paths = mapping_get(spec, "paths")?
        .as_mapping()
        .context("OpenAPI paths must be a mapping")?;
    let mut operations = Vec::new();
    let mut ids = BTreeSet::new();

    for (path_value, item_value) in paths {
        let path = path_value
            .as_str()
            .context("OpenAPI path key must be a string")?;
        let item = item_value
            .as_mapping()
            .with_context(|| format!("path item {path} must be a mapping"))?;
        for method in METHODS {
            let Some(operation_value) = map_get(item, method) else {
                continue;
            };
            let operation = operation_value
                .as_mapping()
                .with_context(|| format!("{method} {path} must be a mapping"))?;
            let id = required_string(operation, "operationId", method, path)?;
            if !ids.insert(id.clone()) {
                bail!("duplicate operationId {id}");
            }
            let description = optional_string(operation, "description")
                .or_else(|| optional_string(operation, "summary"))
                .map(collapse_whitespace)
                .unwrap_or_default();
            operations.push(Operation {
                id,
                method: method.to_ascii_uppercase(),
                path: path.to_owned(),
                description,
            });
        }
    }
    operations.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(operations)
}

fn validate_manifest(operations: &[Operation], manifest: &Manifest) -> Result<()> {
    let spec_by_id: BTreeMap<_, _> = operations
        .iter()
        .map(|operation| (operation.id.as_str(), operation))
        .collect();
    let manifest_ids: BTreeSet<_> = manifest
        .operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect();
    let spec_ids: BTreeSet<_> = spec_by_id.keys().copied().collect();
    if spec_ids != manifest_ids {
        let only_spec: Vec<_> = spec_ids.difference(&manifest_ids).copied().collect();
        let only_manifest: Vec<_> = manifest_ids.difference(&spec_ids).copied().collect();
        bail!(
            "spec/manifest operation mismatch; only spec={only_spec:?}, only manifest={only_manifest:?}"
        );
    }
    for operation in &manifest.operations {
        let spec_operation = spec_by_id[operation.id.as_str()];
        if operation.http_method != spec_operation.method || operation.path != spec_operation.path {
            bail!(
                "manifest mismatch for {}: spec={} {}, manifest={} {}",
                operation.id,
                spec_operation.method,
                spec_operation.path,
                operation.http_method,
                operation.path
            );
        }
    }
    Ok(())
}

fn validate_evidence(operations: &[Operation], evidence: &EvidenceFile) -> Result<()> {
    let operation_ids: BTreeSet<_> = operations
        .iter()
        .map(|operation| operation.id.as_str())
        .collect();
    for (operation_id, sdk_evidence) in &evidence.operations {
        if !operation_ids.contains(operation_id.as_str()) {
            bail!("evidence references unknown operationId {operation_id}");
        }
        for (sdk, live_evidence) in sdk_evidence {
            if !SDKS.contains(&sdk.as_str()) {
                bail!("evidence for {operation_id} references unknown SDK {sdk}");
            }
            if !matches!(
                live_evidence.status.as_str(),
                "completed" | "attempted" | "not_started" | "failed" | "blocked"
            ) {
                bail!(
                    "invalid live status {:?} for {operation_id}/{sdk}",
                    live_evidence.status
                );
            }
            if live_evidence.status == "completed" && live_evidence.evidence.trim().is_empty() {
                bail!("completed status for {operation_id}/{sdk} requires evidence");
            }
        }
    }
    Ok(())
}

fn render_csv(operations: &[Operation], evidence: &EvidenceFile) -> String {
    let mut rows = Vec::with_capacity(operations.len() + 1);
    let mut header = vec![
        "operation_id".to_owned(),
        "http_method".to_owned(),
        "path".to_owned(),
        "description".to_owned(),
        "overall_live_status".to_owned(),
    ];
    for sdk in SDKS {
        header.push(format!("{sdk}_live_status"));
        header.push(format!("{sdk}_live_evidence"));
        header.push(format!("{sdk}_snippet"));
    }
    rows.push(csv_row(&header));

    for operation in operations {
        let mut sdk_values = Vec::with_capacity(SDKS.len() * 3);
        let mut statuses = Vec::with_capacity(SDKS.len());
        for sdk in SDKS {
            let sdk_evidence = evidence
                .operations
                .get(&operation.id)
                .and_then(|by_sdk| by_sdk.get(sdk));
            let status = sdk_evidence
                .map(|item| item.status.as_str())
                .unwrap_or("not_started");
            statuses.push(status);
            sdk_values.push(status.to_owned());
            sdk_values.push(
                sdk_evidence
                    .map(|item| item.evidence.clone())
                    .unwrap_or_default(),
            );
            sdk_values.push(snippet_locator(sdk, &operation.id));
        }
        let overall = if statuses.iter().all(|status| *status == "completed") {
            "completed"
        } else if statuses.contains(&"failed") {
            "failed"
        } else if statuses.contains(&"attempted") {
            "attempted"
        } else if statuses.iter().all(|status| *status == "blocked") {
            "blocked"
        } else {
            "not_started"
        };
        let mut row = vec![
            operation.id.clone(),
            operation.method.clone(),
            operation.path.clone(),
            operation.description.clone(),
            overall.to_owned(),
        ];
        row.extend(sdk_values);
        rows.push(csv_row(&row));
    }
    rows.join("\n") + "\n"
}

fn snippet_locator(sdk: &str, operation_id: &str) -> String {
    match sdk {
        "typescript" => format!("e2e/live-matrix/snippets-typescript.mjs#{operation_id}"),
        "go" => format!("e2e/live-matrix/go/snippets.json#/operations/{operation_id}"),
        "python" => format!("e2e/live-matrix/snippets-python.py#{operation_id}"),
        "ruby" => format!("e2e/live-matrix/snippets-ruby.rb#{operation_id}"),
        "cli" => format!("e2e/live-matrix/snippets-cli.mjs#{operation_id}"),
        _ => unreachable!(),
    }
}

fn csv_row(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| {
            if field.contains([',', '"', '\n', '\r']) {
                format!("\"{}\"", field.replace('"', "\"\""))
            } else {
                field.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn mapping_get<'a>(value: &'a YamlValue, key: &str) -> Result<&'a YamlValue> {
    let mapping = value
        .as_mapping()
        .context("OpenAPI root must be a mapping")?;
    map_get(mapping, key).with_context(|| format!("OpenAPI is missing {key}"))
}

fn map_get<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a YamlValue> {
    mapping.get(YamlValue::String(key.to_owned()))
}

fn required_string(mapping: &Mapping, key: &str, method: &str, path: &str) -> Result<String> {
    optional_string(mapping, key)
        .map(str::to_owned)
        .with_context(|| format!("{method} {path} is missing string {key}"))
}

fn optional_string<'a>(mapping: &'a Mapping, key: &str) -> Option<&'a str> {
    map_get(mapping, key).and_then(YamlValue::as_str)
}

fn collapse_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}
