//! Generator configuration.
//!
//! `redwood.toml` carries spec-wide policy (client name, method-name
//! overrides); each language gets its own `<lang>.config.toml` with package
//! metadata. Both files are optional — every setting has a derived default.

use std::path::Path;

use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde::Deserialize;

/// SSE transport policy shared by every generated target.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SseConfig {
    /// Event names (the SSE `event:` field) that are transport
    /// housekeeping — streams skip them without decoding. Their `id:`
    /// fields still advance the resume checkpoint.
    #[serde(default)]
    pub skip_events: Vec<String>,
}

/// Spec-wide policy: `redwood.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GeneratorConfig {
    #[serde(default)]
    pub api: ApiConfig,
    #[serde(default)]
    pub sse: SseConfig,
    /// operationId -> SDK method name, for when inference gets it wrong.
    /// e.g. `"AccountService_GetAccount" = "retrieve"`
    #[serde(default)]
    pub methods: IndexMap<String, String>,
    /// Inferred resource name -> preferred name (snake_case), e.g.
    /// `accounts = "account"`. A dotted target nests the resource under a
    /// parent accessor: `agent_variations = "agents.variations"` (one level;
    /// type names keep deriving from the original identity, so they stay
    /// unique).
    #[serde(default)]
    pub resources: IndexMap<String, String>,
    /// operationId -> ordered list of path-parameter wire names passed
    /// POSITIONALLY (Stainless positional_params). `[]` makes every param
    /// named. Params not listed stay named; client params are never
    /// positional.
    #[serde(default)]
    pub positional: IndexMap<String, Vec<String>>,
    /// Spec type name -> SDK type name, for re-labeling generation artifacts
    /// (protobuf-shaped names, awkward contextual enums) across every
    /// backend. e.g. `"AgentVariationSpec_CompactionConfig" = "CompactionConfig"`.
    #[serde(default)]
    pub mapping: IndexMap<String, String>,
    /// Per-language configuration, e.g. `[lang.ruby]` / `[lang.go]` — the
    /// single-file replacement for the legacy `<lang>.config.toml` files
    /// (which `--lang-config` can still override explicitly).
    #[serde(default)]
    pub lang: LangConfigs,
}

/// `[lang.*]` sections of redwood.toml.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LangConfigs {
    #[serde(default)]
    pub typescript: TypeScriptConfig,
    #[serde(default)]
    pub go: GoConfig,
    #[serde(default)]
    pub python: PythonConfig,
    #[serde(default)]
    pub ruby: RubyConfig,
    #[serde(default)]
    pub cli: CliConfig,
}

/// An override key selects an operation either by its path definition —
/// `"get /v1/things/{id}"` (canonical, mirrors the endpoint syntax) — or by
/// bare operationId (back-compat).
pub(crate) fn key_matches(key: &str, op: &crate::ir::Operation) -> bool {
    match key.split_once(' ') {
        Some((method, path)) => {
            method.eq_ignore_ascii_case(op.http_method.as_str()) && path == op.path
        }
        None => key == op.id,
    }
}

/// Apply spec-wide policy to a lowered API. Overrides that reference
/// nonexistent operations or resources are errors, not no-ops.
pub fn apply(api: &mut crate::ir::Api, cfg: &GeneratorConfig) -> Result<()> {
    if let Some(name) = &cfg.api.name {
        api.name = name.clone();
        // Env-var defaults derive from the client name, so they follow the
        // rename (an explicit env_var/webhook_env_var below still wins).
        api.api_key_env = format!("{}_API_KEY", name.to_uppercase());
        api.webhook_env = format!("{}_WEBHOOK_SECRET", name.to_uppercase());
    }
    if let Some(base_url) = &cfg.api.base_url {
        api.base_url = base_url.clone();
    }
    {
        let mut seen = std::collections::BTreeSet::new();
        for name in &cfg.sse.skip_events {
            if name.trim().is_empty() {
                anyhow::bail!("[sse] skip_events entries must be non-empty event names");
            }
            if name.chars().any(|c| c.is_control()) {
                anyhow::bail!("[sse] skip_events entry {name:?} contains control characters");
            }
            if !seen.insert(name.as_str()) {
                anyhow::bail!("[sse] skip_events lists {name:?} twice");
            }
        }
        api.sse_skip_events = cfg.sse.skip_events.clone();
    }
    for (key, method_name) in &cfg.methods {
        let mut found = false;
        for resource in &mut api.resources {
            for op in &mut resource.operations {
                if key_matches(key, op) {
                    op.name = method_name.clone();
                    found = true;
                }
            }
        }
        anyhow::ensure!(
            found,
            "[methods] override matches no operation: {key} (use \"<method> <path>\" or an operationId)"
        );
    }
    if let Some(env) = &cfg.api.env_var {
        api.api_key_env = env.clone();
    }
    if let Some(env) = &cfg.api.webhook_env_var {
        api.webhook_env = env.clone();
    }
    if let Some(n) = cfg.api.max_retries {
        api.max_retries = n;
    }
    api.auth_optional = cfg.api.auth_optional;
    // client_params is validated as rigorously as [methods]/[resources]/
    // [mapping]: a typo here would otherwise emit a public no-op option, and
    // a duplicate emits uncompilable SDKs in every language.
    {
        use heck::ToSnakeCase;
        let mut seen_normalized: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        // Built-in client options in every target's constructor surface.
        let reserved = [
            "api_key",
            "base_url",
            "webhook_secret",
            "max_retries",
            "timeout",
        ];
        for wire_name in &cfg.api.client_params {
            let normalized = wire_name.to_snake_case();
            if !seen_normalized.insert(normalized.clone()) {
                anyhow::bail!(
                    "client_params: {wire_name:?} duplicates another entry after \
                     identifier normalization ({normalized})"
                );
            }
            if reserved.contains(&normalized.as_str()) {
                anyhow::bail!(
                    "client_params: {wire_name:?} collides with the built-in \
                     client option {normalized}"
                );
            }
            let mut matched = 0usize;
            let mut non_string: Vec<String> = Vec::new();
            for resource in &api.resources {
                for op in &resource.operations {
                    for p in &op.path_params {
                        if p.wire_name == *wire_name {
                            matched += 1;
                            if !matches!(p.ty, crate::ir::Ty::String) {
                                non_string.push(op.id.clone());
                            }
                        }
                    }
                }
            }
            if matched == 0 {
                anyhow::bail!(
                    "client_params: {wire_name:?} matches no operation PATH \
                     parameter — a client default that nothing resolves is a \
                     public no-op (check for a typo)"
                );
            }
            if !non_string.is_empty() {
                anyhow::bail!(
                    "client_params: {wire_name:?} must be a string path \
                     parameter everywhere; non-string occurrences in: {}",
                    non_string.join(", ")
                );
            }
            let env_var = format!(
                "{}_{}",
                heck::ToShoutySnakeCase::to_shouty_snake_case(api.name.as_str()),
                heck::ToShoutySnakeCase::to_shouty_snake_case(wire_name.as_str()),
            );
            api.client_params.push(crate::ir::ClientParam {
                wire_name: wire_name.clone(),
                env_var,
            });
        }
    }
    for (from, to) in &cfg.resources {
        let resource = api
            .resources
            .iter_mut()
            .find(|r| &r.name == from)
            .with_context(|| format!("[resources] rename references unknown resource {from}"))?;
        match to.split_once('.') {
            Some((parent, leaf)) => {
                anyhow::ensure!(
                    !leaf.contains('.'),
                    "[resources] {from}: nesting is one level deep, got {to}"
                );
                resource.name = leaf.to_string();
                resource.parent = Some(parent.to_string());
            }
            None => resource.name = to.clone(),
        }
    }
    // Validate nesting AFTER all renames so a parent may itself be renamed.
    let top_level: Vec<String> = api
        .resources
        .iter()
        .filter(|r| r.parent.is_none())
        .map(|r| r.name.clone())
        .collect();
    for resource in &api.resources {
        if let Some(parent) = &resource.parent {
            anyhow::ensure!(
                top_level.iter().any(|n| n == parent),
                "[resources] {}.{}: parent {parent} is not a top-level resource",
                parent,
                resource.name
            );
        }
    }

    // Public-doc lint: internal contract-production commentary must not ship
    // as consumer documentation in ANY backend.
    {
        let terms = [
            "gnostic",
            "buf.validate",
            "overlay.yaml",
            "protoc",
            "transcoder",
        ];
        let allowed = &cfg.api.doc_lint_allow;
        let mut hits: Vec<String> = Vec::new();
        let mut scan = |context: &str, text: &Option<String>| {
            if let Some(text) = text {
                let lower = text.to_lowercase();
                for term in terms {
                    if lower.contains(term) && !allowed.iter().any(|a| a.eq_ignore_ascii_case(term))
                    {
                        hits.push(format!("{context}: description mentions {term:?}"));
                    }
                }
            }
        };
        for r in &api.resources {
            for op in &r.operations {
                scan(&format!("{}.{}", r.path(), op.name), &op.description);
                scan(&format!("{}.{} summary", r.path(), op.name), &op.summary);
                for p in op.path_params.iter().chain(op.query_params.iter()) {
                    scan(
                        &format!("{}.{} param {}", r.path(), op.name, p.wire_name),
                        &p.description,
                    );
                }
                for f in &op.body_fields {
                    scan(
                        &format!("{}.{} field {}", r.path(), op.name, f.wire_name),
                        &f.description,
                    );
                }
            }
        }
        for decl in api.types.values() {
            scan(&format!("type {}", decl.name), &decl.description);
            if let crate::ir::Shape::Struct(st) = &decl.shape {
                for f in &st.fields {
                    scan(
                        &format!("type {}.{}", decl.name, f.wire_name),
                        &f.description,
                    );
                }
            }
        }
        if !hits.is_empty() {
            let report = hits.join("\n  ");
            match cfg.api.doc_lint.as_deref() {
                Some("deny") => anyhow::bail!(
                    "doc lint: internal vocabulary in public descriptions \
                     (fix the source spec or allowlist genuine terms):\n  {report}"
                ),
                _ => eprintln!(
                    "warning: doc lint: internal vocabulary in public descriptions \
                     (set doc_lint = \"deny\" to enforce):\n  {report}"
                ),
            }
        }
    }

    // Positional overrides: rebuild each op's positional list from the
    // configured wire names, in the given order. The default inference has
    // already claimed the own-id param, so restore the full pool first.
    for (key, names) in &cfg.positional {
        let mut found = false;
        for resource in &mut api.resources {
            for op in &mut resource.operations {
                if !key_matches(key, op) {
                    continue;
                }
                found = true;
                let mut pool: Vec<crate::ir::Param> = Vec::new();
                pool.append(&mut op.positionals);
                pool.append(&mut op.path_params);
                let mut seen = std::collections::BTreeSet::new();
                let mut positionals = Vec::new();
                for name in names {
                    anyhow::ensure!(
                        seen.insert(name.as_str()),
                        "[positional] {key}: {name} listed twice"
                    );
                    anyhow::ensure!(
                        !cfg.api.client_params.iter().any(|c| c == name),
                        "[positional] {key}: {name} is a client param and \
                         cannot be positional"
                    );
                    let idx = pool
                        .iter()
                        .position(|p| &p.wire_name == name)
                        .with_context(|| {
                            format!(
                                "[positional] {key}: {name} is not a path \
                             parameter of this operation"
                            )
                        })?;
                    let chosen = pool.remove(idx);
                    anyhow::ensure!(
                        matches!(chosen.ty, crate::ir::Ty::String),
                        "[positional] {key}: {name} is not a string path parameter; \
                         non-string positionals are not supported yet"
                    );
                    positionals.push(chosen);
                }
                op.positionals = positionals;
                op.path_params = pool;
            }
        }
        anyhow::ensure!(
            found,
            "[positional] matches no operation: {key} (use \"<method> <path>\" or an operationId)"
        );
    }

    // Overrides are only safe if the RESULTING symbol graph is unambiguous.
    // Validate in the normalized (snake) namespace every backend derives its
    // casing from — camel/kebab/keyword variants collapse the same way.
    {
        use heck::ToSnakeCase;
        let mut accessors: std::collections::BTreeSet<(Option<String>, String)> =
            std::collections::BTreeSet::new();
        for r in &api.resources {
            let key = (
                r.parent.as_ref().map(|p| p.to_snake_case()),
                r.name.to_snake_case(),
            );
            anyhow::ensure!(
                accessors.insert(key),
                "[resources] two resources normalize to the same accessor {}{}",
                r.parent
                    .as_deref()
                    .map(|p| format!("{p}."))
                    .unwrap_or_default(),
                r.name
            );
        }
        for r in &api.resources {
            if let Some(parent) = &r.parent {
                if let Some(p) = api
                    .resources
                    .iter()
                    .find(|x| x.parent.is_none() && &x.name == parent)
                {
                    anyhow::ensure!(
                        !p.operations
                            .iter()
                            .any(|o| o.name.to_snake_case() == r.name.to_snake_case()),
                        "[resources] child accessor {}.{} collides with an operation of \
                         the same name on its parent",
                        parent,
                        r.name
                    );
                }
            }
            let mut ops: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for o in &r.operations {
                anyhow::ensure!(
                    ops.insert(o.name.to_snake_case()),
                    "[methods] resource {} has two operations named {} after \
                     normalization — rename one",
                    r.path(),
                    o.name
                );
            }
        }
    }

    apply_type_mapping(api, &cfg.mapping)?;
    Ok(())
}

/// Rename spec types across the whole IR: the declaration itself and every
/// `Ty::Named` reference to it (fields, unions, aliases, operation params,
/// bodies, responses, pagination, webhook payloads).
fn apply_type_mapping(api: &mut crate::ir::Api, mapping: &IndexMap<String, String>) -> Result<()> {
    use crate::ir::{ResponseKind, Shape, Ty};

    if mapping.is_empty() {
        return Ok(());
    }
    for (from, to) in mapping {
        anyhow::ensure!(
            api.types.contains_key(from),
            "[mapping] references unknown type {from}"
        );
        anyhow::ensure!(
            !api.types.contains_key(to),
            "[mapping] {from} -> {to}: target name already exists"
        );
    }
    // Two sources collapsing into one target would silently OVERWRITE a
    // declaration while every reference is rewritten — producing a compiling
    // SDK whose advertised types are wrong on the wire.
    {
        let mut target_seen: std::collections::BTreeSet<&String> =
            std::collections::BTreeSet::new();
        for (from, to) in mapping {
            anyhow::ensure!(
                target_seen.insert(to),
                "[mapping] target {to} is used by multiple sources (including {from}); \
                 every mapping target must be unique"
            );
        }
    }

    fn rename_ty(ty: &mut Ty, mapping: &IndexMap<String, String>) {
        match ty {
            Ty::Named(n) => {
                if let Some(to) = mapping.get(n) {
                    *n = to.clone();
                }
            }
            Ty::List(inner) | Ty::Map(inner) => rename_ty(inner, mapping),
            _ => {}
        }
    }

    let old = std::mem::take(&mut api.types);
    for (name, mut decl) in old {
        match &mut decl.shape {
            Shape::Struct(s) => {
                for f in &mut s.fields {
                    rename_ty(&mut f.ty, mapping);
                }
                if let Some(a) = &mut s.additional {
                    rename_ty(a, mapping);
                }
            }
            Shape::Union(u) => {
                for v in &mut u.variants {
                    rename_ty(&mut v.ty, mapping);
                }
            }
            Shape::Alias(ty) => rename_ty(ty, mapping),
            Shape::Enum(_) => {}
        }
        let new_name = mapping.get(&name).cloned().unwrap_or(name);
        decl.name = new_name.clone();
        api.types.insert(new_name, decl);
    }

    for resource in &mut api.resources {
        for op in &mut resource.operations {
            for p in op.positionals.iter_mut() {
                rename_ty(&mut p.ty, mapping);
            }
            for p in op.path_params.iter_mut().chain(op.query_params.iter_mut()) {
                rename_ty(&mut p.ty, mapping);
            }
            for f in &mut op.body_fields {
                rename_ty(&mut f.ty, mapping);
            }
            if let Some(ty) = &mut op.whole_body {
                rename_ty(ty, mapping);
            }
            match &mut op.response {
                ResponseKind::Json(ty) | ResponseKind::Sse(ty) => rename_ty(ty, mapping),
                ResponseKind::Empty => {}
            }
            if let Some(page) = &mut op.pagination {
                rename_ty(&mut page.item_ty, mapping);
            }
        }
    }
    for webhook in &mut api.webhooks {
        rename_ty(&mut webhook.payload, mapping);
    }
    Ok(())
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApiConfig {
    /// Client/brand name; defaults to info.title with " API" stripped.
    pub name: Option<String>,
    /// Base URL override; defaults to the spec's first server.
    pub base_url: Option<String>,
    /// Env var holding the API key; defaults to `<NAME>_API_KEY`.
    pub env_var: Option<String>,
    /// Wire names of params that become client-level defaults, settable at
    /// construction or via `<NAME>_<PARAM>` env vars. e.g. ["workspaceId"].
    #[serde(default)]
    pub client_params: Vec<String>,
    /// Env var for the webhook signing secret; defaults to
    /// `<NAME>_WEBHOOK_SECRET` (Standard Webhooks terminology — a DELIBERATE
    /// migration from the legacy `<NAME>_WEBHOOK_KEY`; set this field to keep
    /// the old name during a transition).
    pub webhook_env_var: Option<String>,
    /// Default automatic retries. Defaults to 0 (no silent retries of
    /// non-idempotent calls).
    pub max_retries: Option<u32>,
    /// Some endpoints are public: the client constructs without a
    /// credential and simply omits the auth header when no key is set,
    /// instead of throwing at construction.
    #[serde(default)]
    pub auth_optional: bool,
    /// Public-doc lint: flags internal contract-authoring vocabulary
    /// (gnostic, buf.validate, overlay.yaml, protoc, transcoder) appearing
    /// in descriptions that ship as SDK documentation. "warn" (default)
    /// prints to stderr; "deny" fails generation. Genuine product terms are
    /// allowlisted via `doc_lint_allow`.
    pub doc_lint: Option<String>,
    #[serde(default)]
    pub doc_lint_allow: Vec<String>,
}

/// Go target: `go.config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoConfig {
    /// Go module path, e.g. "go.cadenya.com/cadenya-go".
    pub module_path: Option<String>,
    /// Package name; defaults to the lowercased client name.
    pub package_name: Option<String>,
    /// SDK release version advertised in the User-Agent (the API contract
    /// version from the spec is a separate token). Defaults to "0.1.0".
    pub sdk_version: Option<String>,
    /// Vendor/product words whose canonical Go casing is mixed rather than
    /// all-caps (e.g. `openai = "OpenAI"`). Schema vocabulary belongs HERE,
    /// never in the generator: only generic language initialisms are built in.
    #[serde(default)]
    pub special_casings: IndexMap<String, String>,
}

/// Python target: `python.config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonConfig {
    /// Import package / PyPI name; defaults to the lowercased client name.
    pub package_name: Option<String>,
    /// PyPI release version (also the User-Agent SDK token). Defaults to "0.1.0".
    pub package_version: Option<String>,
    /// License expression for project metadata; defaults to UNLICENSED.
    pub license: Option<String>,
    /// Author names for project metadata.
    pub authors: Option<Vec<String>>,
    /// [project.urls] entries, emitted only when configured.
    pub homepage: Option<String>,
    pub repository: Option<String>,
    pub changelog: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliDisplayColumn {
    /// Column header, e.g. "ID".
    pub header: String,
    /// JSON WIRE path: optional leading dot + dot-separated object keys
    /// (".metadata.id"). Not jq, not JSONPath.
    pub path: String,
    /// Table-mode width cap in characters: longer cells are cut and marked
    /// with an ellipsis so wide payloads (compact JSON of a nested object)
    /// keep the grid readable. Extended output always shows the full value.
    /// Unset means never truncate.
    pub truncate: Option<u32>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliDisplayGroup {
    /// Columns shared by every method that references this group.
    #[serde(default)]
    pub columns: Vec<CliDisplayColumn>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliDisplayMethod {
    /// Optional named group whose columns layer in after the global set.
    pub group: Option<String>,
    /// Endpoint-specific columns, layered in last.
    #[serde(default)]
    pub columns: Vec<CliDisplayColumn>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliDisplayConfig {
    /// Default display mode: "json" (default), "table", or "extended".
    pub default: Option<String>,
    /// Base columns for every command (where they statically resolve).
    #[serde(default)]
    pub columns: Vec<CliDisplayColumn>,
    /// Reusable named column groups ("cliGroups") methods can reference.
    #[serde(default)]
    pub groups: IndexMap<String, CliDisplayGroup>,
    /// Per-endpoint layering, keyed like [methods]/[positional]: a
    /// "<verb> <path>" key (or operationId). Effective columns are
    /// global ++ group ++ endpoint, in declaration order.
    #[serde(default)]
    pub methods: IndexMap<String, CliDisplayMethod>,
}

/// CLI target: `cli.config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliConfig {
    /// Go module path for the CLI, e.g. "go.cadenya.com/cadenya-cli".
    pub module_path: Option<String>,
    /// Binary / root command name; defaults to the lowercased client name.
    pub binary_name: Option<String>,
    /// Module path of the generated Go SDK the CLI drives.
    pub sdk_module: Option<String>,
    /// Optional `replace` target for the SDK module (e.g. "../go").
    pub sdk_replace: Option<String>,
    /// Binary release version reported by `--version` (the API contract
    /// version from the spec is shown as a separate token). Defaults to "0.1.0".
    pub version: Option<String>,
    /// Vendor casing overrides, mirroring the Go SDK's `special_casings`
    /// (the CLI emits Go identifiers that must match the SDK's naming).
    #[serde(default)]
    pub special_casings: IndexMap<String, String>,
    /// Human-readable output configuration ([lang.cli.display]).
    #[serde(default)]
    pub display: CliDisplayConfig,
    /// Browser-assisted login ([lang.cli.auth]): RFC 8628 device
    /// authorization against the configured endpoints, credential stored in
    /// `~/.<binary>/credentials`. Generates `auth login/logout/status` and
    /// a credentials-file fallback in every command's auth resolution.
    pub auth: Option<CliAuthConfig>,
    /// Top-level command aliases ([lang.cli.aliases]): alias name ->
    /// space-separated command path ("whoami" = "profiles whoami"). The
    /// alias appears as its own top-level command sharing the target's
    /// flags and action. Validated against the generated command tree.
    #[serde(default)]
    pub aliases: IndexMap<String, String>,
    /// Packaged-install channels documented in the README ([lang.cli.install]).
    /// The release pipeline that publishes to these lives with the CLI repo;
    /// this only controls what the generated docs tell users to run.
    pub install: Option<CliInstallConfig>,
    /// Request-body input naming ([lang.cli.body]): how body fields become
    /// flags. Every rule is structural; this only tunes names.
    #[serde(default)]
    pub body: CliBodyConfig,
}

/// [lang.cli.body]: options for the body plan (see `ir::plan`).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliBodyConfig {
    /// Envelope segments dropped from flag names; defaults to
    /// `["metadata", "spec"]`. An empty list disables elision.
    pub elide: Option<Vec<String>>,
    /// Singular overrides for repeatable inputs, keyed by the field's wire
    /// name: `"memoryCascade" = "memory-cascade"`.
    #[serde(default)]
    pub singular: IndexMap<String, String>,
    /// Offer enum short forms (`weighted` for `..._WEIGHTED`); default true.
    pub enum_short_forms: Option<bool>,
    /// Per-operation segment renames, keyed like display methods
    /// (`"post /v1/things"` or an operationId), then by dotted wire path:
    /// `"spec.modelConfig" = "model"`. An empty replacement elides the
    /// segment. Unknown operations and unmatched paths are errors.
    #[serde(default)]
    pub rename: IndexMap<String, IndexMap<String, String>>,
}

/// Where the built CLI is published, for the README's Install section.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliInstallConfig {
    /// Homebrew tap in `owner/name` short form (e.g. "cadenya/tools" for the
    /// `cadenya/homebrew-tools` repo): documents `brew install <tap>/<binary>`.
    pub homebrew_tap: Option<String>,
    /// Chocolatey package id: documents `choco install <id>`.
    pub chocolatey_package: Option<String>,
    /// Releases page carrying the prebuilt archives.
    pub releases_url: Option<String>,
}

/// RFC 8628 device-authorization login for the generated CLI.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CliAuthConfig {
    /// Device authorization endpoint (RFC 8628 §3.1), e.g.
    /// "https://app.example.com/api/cli/oauth/device_authorization".
    pub device_authorization_endpoint: String,
    /// Token endpoint polled with the device_code grant (RFC 8628 §3.4).
    pub token_endpoint: String,
    /// Public client identifier sent to both endpoints.
    pub client_id: String,
    /// Client-param WIRE NAME that a single-workspace login defaults (the
    /// token response's `workspaces` extension member; exactly one entry
    /// makes its id the param's default). Must be a declared client param.
    pub workspaces_param: Option<String>,
}

/// Ruby target: `ruby.config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RubyConfig {
    /// Gem / require name; defaults to the lowercased client name.
    pub gem_name: Option<String>,
    /// Gem release version (also the User-Agent SDK token). Defaults to "0.1.0".
    pub package_version: Option<String>,
    /// spec.license; defaults to UNLICENSED.
    pub license: Option<String>,
    /// spec.authors; defaults to the API name.
    pub authors: Option<Vec<String>>,
    /// spec.homepage plus gem metadata URIs, emitted only when configured.
    pub homepage: Option<String>,
    pub source_code_uri: Option<String>,
    pub changelog_uri: Option<String>,
}

/// TypeScript target: `typescript.config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeScriptConfig {
    /// npm package name; defaults to lowercased client name.
    pub package_name: Option<String>,
    /// Git repository URL for package.json (npm provenance validates it
    /// against the publishing workflow's repo).
    pub repository: Option<String>,
    pub package_version: Option<String>,
    /// SPDX license identifier for package.json; defaults to UNLICENSED.
    pub license: Option<String>,
    /// Internal package: `"private": true` in package.json (npm refuses to
    /// publish it) and no publishConfig. For git-installed SDKs.
    #[serde(default)]
    pub private: bool,
}

pub fn load<T: serde::de::DeserializeOwned + Default>(path: Option<&Path>) -> Result<T> {
    let Some(path) = path else {
        return Ok(T::default());
    };
    let source = std::fs::read_to_string(path)
        .with_context(|| format!("reading config {}", path.display()))?;
    toml::from_str(&source).with_context(|| format!("parsing config {}", path.display()))
}
