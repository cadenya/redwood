//! CLI backend: a urfave/cli v3 command tree over the generated Go SDK.
//! `<binary> <resource> <method> [own-id] [--flags]` — every scalar leaf of a
//! request body is a typed flag (see `ir::plan`), subtrees also take
//! YAML/JSON documents, and responses print as JSON, YAML, or tables.

use std::fmt::Write as _;

use heck::{ToKebabCase, ToSnakeCase};

use crate::backends::cli_inputs::{self, Plans};
use crate::backends::{Backend, FileSet};
use crate::config::{CliAuthConfig, CliConfig};
use crate::ir::plan::InputKind;
use crate::ir::*;

pub struct CliBackend {
    pub config: CliConfig,
}

impl Backend for CliBackend {
    fn name(&self) -> &'static str {
        "cli"
    }

    fn generate(&self, api: &Api) -> anyhow::Result<FileSet> {
        crate::backends::golang::install_config_casings(&self.config.special_casings);
        let default_mode = validate_display_config(api, &self.config.display)?;
        validate_aliases(api, &self.config)?;
        // Surface per-operation layering errors (duplicate headers across
        // layers, writeOnly paths) as generation failures, not panics.
        for resource in &api.resources {
            for op in &resource.operations {
                let columns = effective_columns(op, &self.config.display)?;
                let model = match (&op.pagination, &op.response) {
                    (Some(page), _) => Some(page.item_ty.clone()),
                    (None, ResponseKind::Json(ty)) => Some(ty.clone()),
                    _ => None,
                };
                if let Some(ty) = model {
                    applicable_columns(api, &ty, &columns)?;
                }
            }
        }
        let display = (default_mode, &self.config.display);
        // Body plans: naming collisions and unused renames fail here, with
        // the operation named, before any file is emitted.
        let plans = cli_inputs::plan_all(api, &self.config)?;
        let module = self.module_path(api);
        let sdk_module = self.sdk_module(api);
        let binary = self.binary_name(api);
        let mut files = FileSet::new();
        files.insert(
            "go.mod".into(),
            emit_go_mod(&module, &sdk_module, &self.config),
        );
        files.insert(
            "main.go".into(),
            emit_main(api, &sdk_module, &binary, &self.config),
        );
        files.insert("helpers.go".into(), emit_helpers(api, &sdk_module));
        files.insert("internal/commands/body.go".into(), RT_BODY.to_string());
        files.insert(
            "internal/commands/body_test.go".into(),
            RT_BODY_TEST.to_string(),
        );
        files.insert(
            "internal/commands/conversion.go".into(),
            RT_CONVERSION.to_string(),
        );
        files.insert("schemas.go".into(), emit_schemas(api, &binary, &plans));
        if let Some(auth) = &self.config.auth {
            validate_auth_config(api, auth)?;
            files.insert("auth.go".into(), emit_auth(api, &binary, auth));
        }
        if !api.client_params.is_empty() {
            files.insert(
                "config.go".into(),
                emit_config(api, &binary, self.config.auth.is_some()),
            );
        }
        for resource in &api.resources {
            files.insert(
                format!("cmd_{}.go", resource.ident),
                emit_resource_command(api, resource, &module, &sdk_module, &display, &plans),
            );
            for op in &resource.operations {
                if op.has_params() {
                    files.insert(
                        format!(
                            "internal/commands/{}_{}_conv.go",
                            resource.ident,
                            op.name.to_snake_case()
                        ),
                        emit_operation_conversion(
                            api,
                            resource,
                            op,
                            &sdk_module,
                            plans.get(&op.id),
                        ),
                    );
                }
            }
        }
        files.insert("api.md".into(), emit_api_md(api, &binary, &plans));
        files.insert(
            "README.md".into(),
            emit_readme(api, &binary, &module, &self.config, &plans),
        );
        Ok(files)
    }
}

// ---- docs --------------------------------------------------------------------

/// Command-grammar reference: subcommand path, positional, and flags.
fn emit_api_md(api: &Api, binary: &str, plans: &Plans) -> String {
    let mut out = format!(
        "# {binary} CLI reference\n\nRequest bodies are built from typed flags; any subtree also takes a YAML/JSON document (`@file`, `-` for stdin, or a literal; at most ONE input per invocation may read stdin) and `-f <doc>` supplies the whole body. Document flags are listed under `schema <command>`. See README.md for usage patterns.\n"
    );
    for resource in &api.resources {
        let cmd_path: String = resource
            .path()
            .split('.')
            .map(command_name)
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(out, "\n## {binary} {cmd_path}\n").unwrap();
        for op in &resource.operations {
            if let Some(s) = &op.summary {
                writeln!(out, "{}\n", s.trim().lines().next().unwrap_or("")).unwrap();
            }
            let mut line = format!("{binary} {cmd_path} {}", command_name(&op.name));
            for p in &op.positionals {
                write!(line, " <{}>", flag_name(&p.wire_name)).unwrap();
            }
            for p in op.path_params.iter().chain(op.query_params.iter()) {
                let required = p.required && client_param(api, &p.wire_name).is_none();
                write!(
                    line,
                    " {}",
                    flag_grammar(api, &p.wire_name, &p.ty, required)
                )
                .unwrap();
            }
            if let Some(plan) = plans.get(&op.id) {
                write!(line, " {}", cli_inputs::body_grammar(plan)).unwrap();
            }
            if matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_))) {
                write!(line, " [--last-event-id <id>]").unwrap();
            }
            writeln!(out, "```sh\n{line}\n```").unwrap();
        }
    }
    out
}

/// Grammar for one flag, faithful to what the urfave/cli parser accepts:
/// booleans take no space-separated value (`--flag` or `--flag=false`), and
/// slice flags are repeatable (comma splitting is intentionally disabled so
/// values — JSON documents especially — can contain commas).
fn flag_grammar(api: &Api, wire_name: &str, ty: &Ty, required: bool) -> String {
    let flag = flag_name(wire_name);
    let kind = classify(api, ty);
    let core = match kind {
        FlagKind::Bool => format!("--{flag}[=true|false]"),
        FlagKind::Json => format!("--{flag} <JSON>"),
        FlagKind::JsonSlice => format!("--{flag} <JSON>"),
        FlagKind::StrSlice => format!("--{flag} <value>"),
        _ => format!("--{flag} <value>"),
    };
    let repeat = matches!(kind, FlagKind::StrSlice | FlagKind::JsonSlice);
    match (required, repeat) {
        (true, true) => format!("{core}..."),
        (true, false) => core,
        (false, true) => format!("[{core}]..."),
        (false, false) => format!("[{core}]"),
    }
}

fn emit_readme(api: &Api, binary: &str, module: &str, config: &CliConfig, plans: &Plans) -> String {
    let name = &api.name;
    // Every example below is derived STRUCTURALLY from the IR - the
    // generator knows nothing about any particular schema.
    let cmd_path = |r: &Resource| -> String {
        r.path()
            .split('.')
            .map(command_name)
            .collect::<Vec<_>>()
            .join(" ")
    };
    let ops = || {
        api.resources
            .iter()
            .flat_map(|r| r.operations.iter().map(move |o| (r, o)))
    };
    let list_cmd = ops()
        .find(|(_, o)| o.pagination.is_some() && o.positionals.is_empty())
        .map(|(r, o)| format!("{} {}", cmd_path(r), command_name(&o.name)))
        .unwrap_or_else(|| "<resource> list".to_string());
    let retrieve_cmd = ops()
        .find(|(_, o)| {
            !o.positionals.is_empty() && o.body_fields.is_empty() && o.pagination.is_none()
        })
        .map(|(r, o)| {
            format!(
                "{} {}{}",
                cmd_path(r),
                command_name(&o.name),
                o.positionals
                    .iter()
                    .map(|p| format!(" <{}>", flag_name(&p.wire_name)))
                    .collect::<String>()
            )
        })
        .unwrap_or_else(|| "<resource> retrieve <id>".to_string());
    // The create example drives required scalar leaves from the plan; the
    // same command with `-f` shows the document route.
    let example = crate::backends::doc_example_op(api).map(|(r, o)| {
        let leaves: Vec<String> = plans
            .get(&o.id)
            .map(|plan| {
                plan.inputs
                    .iter()
                    .filter(|i| i.required && matches!(i.kind, InputKind::Leaf(_)))
                    .take(3)
                    .map(|i| format!("--{} <{}>", i.flag, i.flag))
                    .collect()
            })
            .unwrap_or_default();
        (format!("{} {}", cmd_path(r), command_name(&o.name)), leaves)
    });
    let create_cmd = match &example {
        Some((cmd, leaves)) if !leaves.is_empty() => format!("{cmd} {}", leaves.join(" ")),
        Some((cmd, _)) => format!("{cmd} -f body.yml"),
        None => "<resource> create --name <name>".to_string(),
    };
    let create_file_cmd = match &example {
        Some((cmd, _)) => format!("{cmd} -f body.yml"),
        None => "<resource> create -f body.yml".to_string(),
    };
    let dry_run_cmd = match &example {
        Some((cmd, leaves)) if !leaves.is_empty() => {
            format!("{cmd} {} --dry-run > body.yml", leaves.join(" "))
        }
        Some((cmd, _)) => format!("{cmd} -f body.yml --dry-run"),
        None => "<resource> create --name <name> --dry-run > body.yml".to_string(),
    };
    let bool_flag = ops()
        .flat_map(|(_, o)| o.query_params.iter())
        .find(|q| matches!(classify(api, &q.ty), FlagKind::Bool))
        .map(|q| flag_name(&q.wire_name))
        .unwrap_or_else(|| "flag".to_string());
    let (repeat_cmd, repeat_flag) = ops()
        .find_map(|(r, o)| {
            o.query_params
                .iter()
                .find(|q| matches!(classify(api, &q.ty), FlagKind::StrSlice))
                .map(|q| {
                    (
                        format!("{} {}", cmd_path(r), command_name(&o.name)),
                        flag_name(&q.wire_name),
                    )
                })
        })
        .unwrap_or_else(|| ("<resource> list".to_string(), "flag".to_string()));
    let stream_cmd = ops()
        .find(|(_, o)| matches!(o.response, ResponseKind::Sse(_)) && o.pagination.is_none())
        .map(|(r, o)| {
            let pos: String = o
                .positionals
                .iter()
                .map(|p| format!(" <{}>", flag_name(&p.wire_name)))
                .collect();
            format!("{} {}{pos}", cmd_path(r), command_name(&o.name))
        })
        .unwrap_or_else(|| "<resource> stream".to_string());
    let ws_env_line = api
        .client_params
        .first()
        .map(|c| {
            format!(
                "- `{}` — default {} for commands that take one (or `{} config set {}`)\n",
                c.env_var,
                c.wire_name.to_kebab_case(),
                binary,
                c.wire_name.to_kebab_case()
            )
        })
        .unwrap_or_default();
    let base_env = format!("{}_BASE_URL", api.name.to_uppercase());
    let debug_env = format!("{}_DEBUG", api.name.to_uppercase());
    let api_config_line = match api.auth {
        Auth::None => String::new(),
        Auth::Basic => format!(
            "- `{username_env}` — HTTP Basic Auth username (or `--username`)\n- `{password_env}` — HTTP Basic Auth password (or `--password`)\n",
            username_env = api.basic_username_env,
            password_env = api.basic_password_env,
        ),
        _ if config.auth.is_some() => format!(
            "- `{api_env}` — API key (or `--api-key`; without either, the stored login from `{binary} auth login` is used)\n",
            api_env = api.api_key_env
        ),
        _ => format!(
            "- `{api_env}` — API key (or `--api-key`)\n",
            api_env = api.api_key_env
        ),
    };
    // Packaged-install instructions are opt-in per project ([lang.cli.install]);
    // `go install` from source is always documented as the fallback.
    let install_section = match &config.install {
        Some(install) => {
            let mut section = String::from("\n");
            if let Some(tap) = &install.homebrew_tap {
                section.push_str(&format!(
                    "macOS / Linux (Homebrew):\n\n```sh\nbrew install {tap}/{binary}\n```\n\n"
                ));
            }
            if let Some(pkg) = &install.chocolatey_package {
                section.push_str(&format!(
                    "Windows (Chocolatey):\n\n```powershell\nchoco install {pkg}\n```\n\n"
                ));
            }
            if let Some(url) = &install.releases_url {
                section.push_str(&format!(
                    "Prebuilt archives for every platform (with checksums) are attached to each\n[GitHub release]({url}). Or build from source:\n\n"
                ));
            } else {
                section.push_str("Or build from source:\n\n");
            }
            // The template supplies the blank line before the code fence.
            section.truncate(section.trim_end().len());
            section.push('\n');
            section
        }
        None => String::from("\n"),
    };
    let auth_section = if config.auth.is_some() {
        format!(
            r#"
## Logging in

```sh
{binary} auth login      # opens the browser; approve, and the credential is stored
{binary} auth status     # is a credential stored for the active profile?
{binary} auth logout     # delete the stored credential
```

`auth login` runs a device-authorization flow: it prints a one-time code,
opens the approval page in your browser (`--no-browser` to print the URL
instead), and stores the resulting credential in `~/.{binary}/credentials`
(mode 0600) once you approve. The credential is never printed.

Resolution order for every command: `--api-key` flag, then the environment,
then the stored login. `auth status` names the source in effect.
`--profile <name>` selects an alternate credentials
profile for any command, including `auth login` itself; stored defaults
(`config set`) follow the same profile. For testing against
a preview deployment, `{auth_base_env}` rebases both auth endpoints onto a
different origin (paths are kept).
"#,
            auth_base_env = format!("{}_AUTH_BASE_URL", api.name.to_uppercase())
        )
    } else {
        String::new()
    };
    let config_section = api
        .client_params
        .first()
        .map(|c| {
            let key = c.wire_name.to_kebab_case();
            format!(
                r#"
## Stored defaults

```sh
{binary} config set {key} <value>   # store a per-profile default
{binary} config list                # effective values and their sources
{binary} config unset {key}         # remove the stored default
```

Resolution order for {key}: `--{key}` flag, then `{env}`, then the stored
default{login_tail}. An explicit source always wins; `config list` names the
source in effect.
"#,
                env = c.env_var,
                login_tail = if config.auth.is_some() {
                    ", then the login's single authorized workspace"
                } else {
                    ""
                }
            )
        })
        .unwrap_or_default();
    format!(
        r#"# {binary} — the {name} CLI

Command-line interface for the {name} API, generated by redwood over the
Go SDK.

## Install
{install_section}
```sh
go install {module}@latest
```

(During development the module resolves the SDK via a local `replace`;
release builds pin a published SDK version.)

## Configuration

{api_config_line}{ws_env_line}- `--base-url` / `{base_env}` — override the API endpoint
- `--debug` / `{debug_env}` — dump every HTTP exchange (redacted credentials) to stderr
- `{binary} --version` prints the binary release version plus the API contract version it was generated against
{auth_section}{config_section}
## Usage

```sh
{binary} {list_cmd}
{binary} {retrieve_cmd}
{binary} {create_cmd}
```

Responses print as indented JSON. List commands print one page plus
`nextCursor`; pass `--cursor` to continue.

## Discovering command schemas

`{binary} schema` lists every command, and `{binary} schema {list_cmd}`
prints that command's invocation contract: positional arguments, every
flag with its wire path and enum values, and a JSON Schema — `$defs`
included — for every document input. Built for coding assistants and
scripts that need to construct requests without guessing.

## Flag forms

Boolean flags take no space-separated value: `--{bool_flag}` enables,
`--{bool_flag}=false` disables explicitly (`--{bool_flag} false` is a
usage error — `false` would parse as a positional argument).

Repeatable flags (shown as `[--flag <value>]...` in the reference) take
one value per occurrence; comma splitting is disabled so values may
contain commas:

```sh
{binary} {repeat_cmd} --{repeat_flag} a --{repeat_flag} b
```

Enum flags accept a short form (the value without its shared prefix,
lowercase) as well as the wire value; `--help` lists the short forms.

## Request bodies

Every scalar field of a request body is a flag, named by its path with
the `metadata`/`spec` envelopes dropped. String maps are repeatable
`KEY=VALUE` flags, scalar lists repeat a value, and a discriminated
union collapses onto its arms: `--<arm>-<field>` flags select the arm,
so the tag flag is only needed when none of the arm's fields are set.

```sh
{binary} {create_cmd}
```

Three input sources layer, deepest wins — a whole-body file, a document
for one subtree, and individual flags:

```sh
{binary} {create_file_cmd}                     # YAML or JSON, @path or - for stdin
{binary} {create_file_cmd} --name staging      # a flag overrides the file
```

Document inputs (`<doc>` in the reference) take a YAML/JSON literal,
`@path`, or `-` for stdin (at most one input per invocation reads stdin —
plain string flags accept `@path`/`-` too, so secrets stay out of shell
history; write `@@` for a literal leading `@`). Fields the request does not
accept — `id`, timestamps, state from a pasted `get` — are dropped with a
warning (`--strict` makes that an error), so `get --display yaml`, edit,
`update -f` round-trips. Enum values inside documents accept short forms.

Flat list items take `key=value,...` shorthand (`{{name, value}}` items
also accept `NAME=VALUE`); untyped maps take `KEY=VALUE` for strings and
`KEY:=JSON` for typed values. Anything deeper is a document.

`--dry-run` prints the assembled body instead of sending it — the fastest
way to learn the file format is to build with flags once and keep it:

```sh
{binary} {dry_run_cmd}
```

Partial updates derive their field mask from the flags and documents
supplied; pass the mask flag explicitly to override.

## Streaming

```sh
{binary} {stream_cmd}
# resume after a disconnect (an explicitly empty value clears the checkpoint):
{binary} {stream_cmd} --last-event-id <id>
```

Streamed events are NDJSON: one compact JSON document per line, so each
stdout line parses independently (`jq -c`, line readers, `while read`).
Nothing but event JSON is written to stdout. Ctrl-C closes the connection
cleanly. Ordinary single-response commands keep indented JSON.

## Display modes

`--display json|yaml|table|extended` (root or per-command). `yaml` prints
the same document as YAML (handy with `update -f`). `table` renders the configured columns via
aligned text for objects and list pages — a page shows only the current
page, with the next cursor reported on stderr so stdout rows stay clean.
`extended` prints the same columns as psql-style vertical records. Missing
or null values render `-`; embedded newlines/tabs are escaped; composite
values print compact JSON. Streaming commands accept only json (NDJSON);
commands with no applicable configured columns reject table/extended with
a usage error.

## Exit codes

- `0` success
- `1` API or transport error (message on stderr)
- `2` usage error (bad/missing arguments), before any network I/O
- `130` interrupted (Ctrl-C / SIGTERM), e.g. while following a stream

## Reference

See [api.md](api.md) for every command and flag.
"#
    )
}

impl CliBackend {
    fn module_path(&self, api: &Api) -> String {
        self.config
            .module_path
            .clone()
            .unwrap_or_else(|| format!("example.com/{}-cli", api.name.to_lowercase()))
    }
    fn sdk_module(&self, api: &Api) -> String {
        self.config
            .sdk_module
            .clone()
            .unwrap_or_else(|| format!("example.com/{}-go", api.name.to_lowercase()))
    }
    fn binary_name(&self, api: &Api) -> String {
        self.config
            .binary_name
            .clone()
            .unwrap_or_else(|| api.name.to_lowercase())
    }
}

// ---- naming ----------------------------------------------------------------

pub(crate) fn flag_name(wire: &str) -> String {
    wire.to_kebab_case()
}

pub(crate) fn command_name(name: &str) -> String {
    name.to_kebab_case()
}

/// Go-side accessors reuse the SDK backend's exported-name rules.
fn go_name(name: &str) -> String {
    super::golang::exported_name(name)
}

// ---- flag classification -----------------------------------------------------

enum FlagKind {
    /// Plain string flag (string, enum, timestamp, bytes, literal).
    Str,
    Bool,
    Int32,
    Int64,
    Float32,
    Float64,
    /// Repeated string flag holding plain strings.
    StrSlice,
    /// Repeated string flag; each item is a JSON document.
    JsonSlice,
    /// Single string flag holding a JSON document.
    Json,
}

/// Validate the display configuration: mode value, column shape per layer,
/// group references, and that every method key matches an operation.
fn validate_display_config(
    api: &Api,
    config: &crate::config::CliDisplayConfig,
) -> anyhow::Result<String> {
    let default_mode = config.default.clone().unwrap_or_else(|| "json".into());
    if !matches!(
        default_mode.as_str(),
        "json" | "yaml" | "table" | "extended"
    ) {
        anyhow::bail!(
            "[lang.cli.display] default must be json, yaml, table, or extended (got {default_mode:?})"
        );
    }
    validate_columns("[lang.cli.display]", &config.columns)?;
    for (name, group) in &config.groups {
        validate_columns(&format!("[lang.cli.display.groups.{name}]"), &group.columns)?;
    }
    let ops = || api.resources.iter().flat_map(|r| r.operations.iter());
    for (key, method) in &config.methods {
        if !ops().any(|op| crate::config::key_matches(key, op)) {
            anyhow::bail!(
                "[lang.cli.display.methods] {key:?} matches no operation (use \"<method> <path>\" or an operationId)"
            );
        }
        if let Some(group) = &method.group {
            if !config.groups.contains_key(group) {
                anyhow::bail!(
                    "[lang.cli.display.methods] {key:?} references unknown group {group:?}"
                );
            }
        }
        validate_columns(
            &format!("[lang.cli.display.methods.{key}]"),
            &method.columns,
        )?;
    }
    Ok(default_mode)
}

/// [lang.cli.aliases]: every alias must target an existing command path and
/// must not shadow an existing top-level command. Aliases resolve resources
/// by their COMMAND names (kebab), matching what the user types.
fn validate_aliases(api: &Api, config: &CliConfig) -> anyhow::Result<()> {
    let mut top_level: Vec<String> = api
        .resources
        .iter()
        .filter(|r| r.parent.is_none())
        .map(|r| command_name(&r.name))
        .collect();
    top_level.push("schema".into());
    if config.auth.is_some() {
        top_level.push("auth".into());
    }
    if !api.client_params.is_empty() {
        top_level.push("config".into());
    }
    for (alias, target) in &config.aliases {
        if top_level.iter().any(|c| c == alias) {
            anyhow::bail!(
                "[lang.cli.aliases] alias {alias:?} collides with an existing top-level command"
            );
        }
        let segments: Vec<&str> = target.split_whitespace().collect();
        let found = match segments.as_slice() {
            [res, op] => api.resources.iter().any(|r| {
                r.parent.is_none()
                    && command_name(&r.name) == *res
                    && r.operations.iter().any(|o| command_name(&o.name) == *op)
            }),
            [parent, child, op] => api.resources.iter().any(|r| {
                r.parent.as_deref().map(command_name).as_deref() == Some(parent)
                    && command_name(&r.name) == *child
                    && r.operations.iter().any(|o| command_name(&o.name) == *op)
            }),
            _ => false,
        };
        if !found {
            anyhow::bail!("[lang.cli.aliases] alias {alias:?} targets unknown command {target:?}");
        }
    }
    Ok(())
}

fn validate_columns(
    context: &str,
    columns: &[crate::config::CliDisplayColumn],
) -> anyhow::Result<()> {
    for column in columns {
        if column.header.trim().is_empty() {
            anyhow::bail!("{context} column headers must be non-blank");
        }
        if column.truncate == Some(0) {
            anyhow::bail!(
                "{context} column {:?}: truncate must be at least 1 (omit it to never truncate)",
                column.header
            );
        }
        let path = column.path.strip_prefix('.').unwrap_or(&column.path);
        if path.is_empty()
            || !path.split('.').all(|seg| {
                !seg.is_empty() && seg.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            })
        {
            anyhow::bail!(
                "{context} path {:?} must be dot-separated object keys (e.g. \".metadata.id\")",
                column.path
            );
        }
    }
    Ok(())
}

/// One resolved display column: header, wire path segments, and the
/// optional table-mode width cap.
#[derive(Debug, Clone)]
struct DisplayColumn {
    header: String,
    segments: Vec<String>,
    truncate: Option<u32>,
}

/// Effective columns for one operation: the global base, then the
/// referenced group, then endpoint-specific columns, in declaration order.
/// A repeated header across layers is a config error.
fn effective_columns(
    op: &Operation,
    config: &crate::config::CliDisplayConfig,
) -> anyhow::Result<Vec<DisplayColumn>> {
    let mut layers: Vec<&[crate::config::CliDisplayColumn]> = vec![&config.columns];
    for (key, method) in &config.methods {
        if !crate::config::key_matches(key, op) {
            continue;
        }
        if let Some(group) = &method.group {
            layers.push(&config.groups[group].columns);
        }
        layers.push(&method.columns);
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut columns = Vec::new();
    for layer in layers {
        for column in layer {
            let header = column.header.trim().to_string();
            if !seen.insert(header.clone()) {
                anyhow::bail!(
                    "operation {}: display column header {header:?} appears in more than one layer",
                    op.id
                );
            }
            let path = column.path.strip_prefix('.').unwrap_or(&column.path);
            columns.push(DisplayColumn {
                header,
                segments: path.split('.').map(String::from).collect(),
                truncate: column.truncate,
            });
        }
    }
    Ok(columns)
}

/// Column indexes that statically apply to a response model, walking the
/// OUTPUT view (a writeOnly field can never become a display column — it is
/// a generation error to configure one).
fn applicable_columns(api: &Api, ty: &Ty, columns: &[DisplayColumn]) -> anyhow::Result<Vec<usize>> {
    fn resolve(api: &Api, ty: &Ty, segments: &[String]) -> anyhow::Result<bool> {
        if segments.is_empty() {
            return Ok(true);
        }
        let mut current = ty.clone();
        // Aliases resolve transparently; only struct fields advance a path.
        for _ in 0..8 {
            match &current {
                Ty::Named(n) => match api.types.get(n).map(|d| &d.shape) {
                    Some(Shape::Alias(inner)) => current = inner.clone(),
                    Some(Shape::Struct(st)) => {
                        let Some(field) = st.fields.iter().find(|f| f.wire_name == segments[0])
                        else {
                            return Ok(false);
                        };
                        if field.write_only {
                            anyhow::bail!(
                                "[lang.cli.display] path segment {:?} is a writeOnly field and can never be displayed",
                                segments[0]
                            );
                        }
                        return resolve(api, &field.ty, &segments[1..]);
                    }
                    // A union is transparent when the path resolves on EVERY
                    // arm — the discriminator tag always does, so `.data.type`
                    // is a legal column over a discriminated payload.
                    Some(Shape::Union(u)) => {
                        if u.variants.is_empty() {
                            return Ok(false);
                        }
                        for v in &u.variants {
                            if !resolve(api, &v.ty, segments)? {
                                return Ok(false);
                            }
                        }
                        return Ok(true);
                    }
                    _ => return Ok(false),
                },
                _ => return Ok(false),
            }
        }
        Ok(false)
    }
    let mut applicable = Vec::new();
    for (index, column) in columns.iter().enumerate() {
        if resolve(api, ty, &column.segments)? {
            applicable.push(index);
        }
    }
    Ok(applicable)
}

/// Declared enum values for a flag's type (through aliases and lists), so
/// help can show choices and the parser can reject invalid values OFFLINE.
fn enum_values(api: &Api, ty: &Ty) -> Option<Vec<String>> {
    match ty {
        Ty::List(inner) => enum_values(api, inner),
        Ty::Literal(v) => Some(vec![v.clone()]),
        Ty::Named(n) => {
            let mut current = n.as_str();
            for _ in 0..8 {
                match api.types.get(current).map(|d| &d.shape) {
                    Some(Shape::Enum(e)) => return Some(e.values.clone()),
                    Some(Shape::Alias(Ty::Named(next))) => current = next,
                    _ => return None,
                }
            }
            None
        }
        _ => None,
    }
}

fn classify(api: &Api, ty: &Ty) -> FlagKind {
    match ty {
        Ty::String | Ty::Literal(_) | Ty::Timestamp | Ty::Bytes => FlagKind::Str,
        Ty::Bool => FlagKind::Bool,
        Ty::Int32 => FlagKind::Int32,
        Ty::Int64 => FlagKind::Int64,
        Ty::Float => FlagKind::Float32,
        Ty::Double => FlagKind::Float64,
        Ty::Json | Ty::Map(_) => FlagKind::Json,
        Ty::List(inner) => match classify(api, inner) {
            FlagKind::Str => FlagKind::StrSlice,
            _ => FlagKind::JsonSlice,
        },
        Ty::Named(n) => {
            let mut current = n.as_str();
            for _ in 0..8 {
                match api.types.get(current).map(|d| &d.shape) {
                    Some(Shape::Enum(_)) => return FlagKind::Str,
                    Some(Shape::Alias(Ty::Named(next))) => current = next,
                    Some(Shape::Alias(inner)) => {
                        let inner = inner.clone();
                        return classify(api, &inner);
                    }
                    _ => return FlagKind::Json,
                }
            }
            FlagKind::Json
        }
    }
}

// ---- go.mod ------------------------------------------------------------------

fn emit_go_mod(module: &str, sdk_module: &str, config: &CliConfig) -> String {
    // go-sse is an indirect dependency through the SDK's public stream
    // types; with a local `replace` the consumer module must declare it
    // itself or the first clean `go build` fails asking for `go mod tidy`.
    let toml_dep = if config.auth.is_some() {
        // The credentials file is TOML; parsing it by hand is how quoting
        // bugs become credential bugs.
        "\n\tgithub.com/pelletier/go-toml/v2 v2.2.4"
    } else {
        ""
    };
    // YAML for document inputs and `--display yaml` (JSON semantics: a
    // YAML document converts to the JSON the API receives).
    let mut out = format!(
        "module {module}\n\ngo 1.22\n\nrequire (\n\tgithub.com/urfave/cli/v3 v3.3.8{toml_dep}\n\tsigs.k8s.io/yaml v1.6.0\n\t{sdk_module} v0.0.0\n)\n\nrequire github.com/tmaxmax/go-sse v0.11.0 // indirect\n"
    );
    if let Some(replace) = &config.sdk_replace {
        write!(out, "\nreplace {sdk_module} => {replace}\n").unwrap();
    }
    out
}

// ---- schemas.go ---------------------------------------------------------------

/// One embedded MARKDOWN document per command describing exactly how to
/// invoke it: a usage line, flag bullets (enum values inline), and a Types
/// section with each reachable named type's JSON Schema in a ```json fence
/// ($refs name the sibling types directly — no JSON-pointer noise). Built
/// for coding assistants: `<binary> schema widgets create` answers "what
/// does this command take" in prose a human can also skim.
fn emit_schemas(api: &Api, binary: &str, plans: &Plans) -> String {
    use indexmap::IndexMap;
    use serde_json::Value;

    use cli_inputs::ty_schema;

    // One flag bullet: grammar, requiredness, description, enum values or a
    // pointer at the Types section.
    fn flag_bullet(
        api: &Api,
        wire_name: &str,
        ty: &Ty,
        required: bool,
        description: Option<&str>,
        defs: &mut IndexMap<String, Value>,
        inline_schemas: &mut IndexMap<String, Value>,
    ) -> String {
        let grammar = flag_grammar(api, wire_name, ty, required);
        let mut bullet = format!("- `{grammar}`");
        if required {
            bullet.push_str(" (required)");
        }
        if let Some(desc) = description {
            let one = first_line(desc);
            if !one.is_empty() {
                bullet.push_str(" — ");
                bullet.push_str(&one);
            }
        }
        let kind = classify(api, ty);
        let value_ty = match ty {
            Ty::List(inner) => inner.as_ref(),
            other => other,
        };
        if matches!(kind, FlagKind::Json | FlagKind::JsonSlice) {
            match value_ty {
                Ty::Named(name) => {
                    ty_schema(api, value_ty, defs);
                    bullet.push_str(&format!(" JSON — see `{name}` under Types."));
                }
                Ty::Json => bullet.push_str(" Arbitrary JSON."),
                other => {
                    let schema = ty_schema(api, other, defs);
                    inline_schemas.insert(format!("--{}", flag_name(wire_name)), schema);
                    bullet.push_str(&format!(
                        " JSON — see `--{}` under Types.",
                        flag_name(wire_name)
                    ));
                }
            }
        } else if let Ty::Named(name) = value_ty {
            if let Some(decl) = api.types.get(name) {
                if let Shape::Enum(e) = &decl.shape {
                    let values = e
                        .values
                        .iter()
                        .map(|v| format!("`{v}`"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    bullet.push_str(&format!(" One of: {values}."));
                }
            }
        }
        bullet
    }

    let mut entries: Vec<(String, String, String)> = Vec::new(); // key, summary, markdown
    for resource in &api.resources {
        let cmd_path: String = resource
            .path()
            .split('.')
            .map(command_name)
            .collect::<Vec<_>>()
            .join(" ");
        for op in &resource.operations {
            let key = format!("{cmd_path} {}", command_name(&op.name));
            let summary = op.summary.as_deref().map(first_line).unwrap_or_default();
            let mut defs: IndexMap<String, Value> = IndexMap::new();
            let mut inline_schemas: IndexMap<String, Value> = IndexMap::new();

            let mut doc = format!("# {binary} {key}\n");
            if !summary.is_empty() {
                doc.push_str(&format!("\n{summary}\n"));
            }

            // Usage line: the exact grammar api.md documents.
            let mut line = format!("{binary} {key}");
            for p in &op.positionals {
                write!(line, " <{}>", flag_name(&p.wire_name)).unwrap();
            }
            for p in op.path_params.iter().chain(op.query_params.iter()) {
                let required = p.required && client_param(api, &p.wire_name).is_none();
                write!(
                    line,
                    " {}",
                    flag_grammar(api, &p.wire_name, &p.ty, required)
                )
                .unwrap();
            }
            let plan = plans.get(&op.id);
            if let Some(plan) = plan {
                write!(line, " {}", cli_inputs::body_grammar(plan)).unwrap();
            }
            if matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_))) {
                write!(line, " [--last-event-id <id>]").unwrap();
            }
            doc.push_str(&format!("\n```sh\n{line}\n```\n"));

            if !op.positionals.is_empty() {
                doc.push_str("\n## Arguments\n\n");
                for p in &op.positionals {
                    let mut bullet = format!("- `<{}>`", flag_name(&p.wire_name));
                    if let Some(d) = &p.description {
                        let one = first_line(d);
                        if !one.is_empty() {
                            bullet.push_str(" — ");
                            bullet.push_str(&one);
                        }
                    }
                    doc.push_str(&bullet);
                    doc.push('\n');
                }
            }

            let mut flag_bullets = Vec::new();
            for p in op.path_params.iter().chain(op.query_params.iter()) {
                let required = p.required && client_param(api, &p.wire_name).is_none();
                flag_bullets.push(flag_bullet(
                    api,
                    &p.wire_name,
                    &p.ty,
                    required,
                    p.description.as_deref(),
                    &mut defs,
                    &mut inline_schemas,
                ));
            }
            if let Some(plan) = plan {
                flag_bullets.extend(cli_inputs::flag_bullets(api, plan, &mut defs));
            }
            if !flag_bullets.is_empty() {
                doc.push_str("\n## Flags\n\n");
                for b in &flag_bullets {
                    doc.push_str(b);
                    doc.push('\n');
                }
                doc.push_str(
                    "\nDocument inputs (`<doc>`) accept a YAML or JSON literal, `@path`, or `-` (stdin; one input per invocation). Sources layer file < document < flag, deeper wins. Repeatable flags take one value per occurrence. Enum values accept the short forms listed.\n",
                );
            }

            if !inline_schemas.is_empty() || !defs.is_empty() {
                doc.push_str(
                    "\n## Types\n\nSchemas are JSON Schema; a `$ref` names a sibling type in this section.\n",
                );
                for (flag, schema) in &inline_schemas {
                    let rendered = serde_json::to_string_pretty(schema).expect("schema serializes");
                    doc.push_str(&format!("\n### `{flag}`\n\n```json\n{rendered}\n```\n"));
                }
                for (name, schema) in &defs {
                    let mut header = format!("\n### {name}\n\n");
                    if let Some(desc) = api.types.get(name).and_then(|d| d.description.as_deref()) {
                        let one = first_line(desc);
                        if !one.is_empty() {
                            header.push_str(&format!("{one}\n\n"));
                        }
                    }
                    let rendered = serde_json::to_string_pretty(schema).expect("schema serializes");
                    doc.push_str(&format!("{header}```json\n{rendered}\n```\n"));
                }
            }

            entries.push((key, summary.to_string(), doc));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    // The bare `schema` index: one markdown bullet per command.
    let mut index = format!("# {binary} commands\n\nRun `{binary} schema <command path>` for any command's full contract.\n\n");
    for (key, summary, _) in &entries {
        if summary.is_empty() {
            writeln!(index, "- `{key}`").unwrap();
        } else {
            writeln!(index, "- `{key}` — {summary}").unwrap();
        }
    }

    let mut out = String::from(
        "// Code generated by redwood. DO NOT EDIT.\npackage main\n\nimport (\n\t\"context\"\n\t\"fmt\"\n\t\"strings\"\n\n\tcli \"github.com/urfave/cli/v3\"\n)\n\n// opSchemas maps a command path to its invocation contract as markdown.\nvar opSchemas = map[string]string{\n",
    );
    for (key, _, doc) in &entries {
        writeln!(out, "\t{}: {},", go_quote(key), go_quote(doc)).unwrap();
    }
    out.push_str("}\n\nconst schemaIndex = ");
    out.push_str(&go_quote(&index));
    out.push_str(
        r#"

func schemaCommand() *cli.Command {
	return &cli.Command{
		Name:      "schema",
		Usage:     "Describe a command's arguments and flag schemas as markdown",
		ArgsUsage: "[command path, e.g. `widgets create`]",
		Action: func(_ context.Context, cmd *cli.Command) error {
			key := strings.Join(cmd.Args().Slice(), " ")
			if key == "" {
				fmt.Print(schemaIndex)
				return nil
			}
			doc, ok := opSchemas[key]
			if !ok {
				return cli.Exit(fmt.Sprintf("unknown command %q (run `schema` with no arguments to list commands)", key), 2)
			}
			fmt.Print(doc)
			return nil
		},
	}
}
"#,
    );
    out
}

/// Escape a string as a double-quoted Go string literal.
pub(crate) fn go_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

// ---- auth.go -----------------------------------------------------------------

fn validate_auth_config(api: &Api, auth: &CliAuthConfig) -> anyhow::Result<()> {
    if matches!(api.auth, Auth::None) {
        anyhow::bail!(
            "[lang.cli.auth]: the API declares no security scheme, so a \
             login command has no credential to store"
        );
    }
    if matches!(api.auth, Auth::Basic) {
        anyhow::bail!(
            "[lang.cli.auth]: device-authorization login yields a single token and is not compatible with HTTP Basic Auth"
        );
    }
    for (label, value) in [
        (
            "device_authorization_endpoint",
            &auth.device_authorization_endpoint,
        ),
        ("token_endpoint", &auth.token_endpoint),
    ] {
        let local = value.starts_with("http://127.0.0.1") || value.starts_with("http://localhost");
        if !(value.starts_with("https://") || local) {
            anyhow::bail!(
                "[lang.cli.auth] {label} must be https (http:// is allowed \
                 only for localhost testing): {value}"
            );
        }
    }
    if auth.client_id.trim().is_empty() {
        anyhow::bail!("[lang.cli.auth] client_id must be non-empty");
    }
    if let Some(wire) = &auth.workspaces_param {
        if !api.client_params.iter().any(|c| &c.wire_name == wire) {
            anyhow::bail!(
                "[lang.cli.auth] workspaces_param {wire:?} is not a declared \
                 [api] client_params entry"
            );
        }
    }
    Ok(())
}

/// RFC 8628 device-authorization login: `auth login/logout/status` plus the
/// credentials file the rest of the CLI falls back to. Placeholder-substituted
/// rather than format!-escaped: this is a vendored Go source, not a projection
/// of the IR, and doubled braces would make it unreadable.
fn emit_auth(api: &Api, binary: &str, auth: &CliAuthConfig) -> String {
    AUTH_GO
        .replace("__BINARY__", binary)
        .replace("__CLIENT_ID__", &auth.client_id)
        .replace("__DEVICE_ENDPOINT__", &auth.device_authorization_endpoint)
        .replace("__TOKEN_ENDPOINT__", &auth.token_endpoint)
        .replace(
            "__AUTH_BASE_ENV__",
            &format!("{}_AUTH_BASE_URL", api.name.to_uppercase()),
        )
        .replace("__API_KEY_ENV__", &api.api_key_env)
}

/// config.go: persistent per-profile client defaults for [api]
/// client_params — `<binary> config set workspace-id <id>` — consulted by
/// newClient below the flag and the environment, above login inference.
fn emit_config(api: &Api, binary: &str, has_auth: bool) -> String {
    let entries = api
        .client_params
        .iter()
        .map(|c| {
            format!(
                "\t\"{flag}\": {{wire: \"{wire}\", env: \"{env}\"}},\n",
                flag = flag_name(&c.wire_name),
                wire = c.wire_name,
                env = c.env_var
            )
        })
        .collect::<String>();
    // The login-derived source line only exists when a login can exist.
    let login_source = if has_auth {
        r#"	if prof := storedProfile(cmd); prof != nil && key == workspacesConfigKey && len(prof.Workspaces) == 1 {
		return prof.Workspaces[0].ID, "stored login (single authorized workspace)"
	}
"#
    } else {
        ""
    };
    let workspaces_key = if has_auth {
        format!(
            "// workspacesConfigKey is the client param the stored login can default\n// when exactly one workspace was authorized.\nconst workspacesConfigKey = \"{}\"\n\n",
            api.client_params
                .first()
                .map(|c| flag_name(&c.wire_name))
                .unwrap_or_default()
        )
    } else {
        String::new()
    };
    CONFIG_GO
        .replace("__ENTRIES__", &entries)
        .replace("__LOGIN_SOURCE__", login_source)
        .replace("__WORKSPACES_KEY__", &workspaces_key)
        .replace("__BINARY__", binary)
}

const CONFIG_GO: &str = r#"// Code generated by redwood. DO NOT EDIT.
package main

// Persistent client defaults. `__BINARY__ config set workspace-id <id>`
// stores a per-profile default consulted by every command, below the
// command-line flag and the environment variable and above any
// login-derived default: an explicit source always wins.

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"

	toml "github.com/pelletier/go-toml/v2"
	cli "github.com/urfave/cli/v3"
)

type configKeyInfo struct {
	wire string // wire spelling, accepted as an alias ("workspaceId")
	env  string // environment variable that outranks the stored default
}

// configKeys: canonical (kebab flag) key -> info. Generated from
// [api] client_params.
var configKeys = map[string]configKeyInfo{
__ENTRIES__}

__WORKSPACES_KEY__// canonicalConfigKey accepts the flag spelling ("workspace-id") or the
// wire spelling ("workspaceId").
func canonicalConfigKey(raw string) (string, bool) {
	if _, ok := configKeys[raw]; ok {
		return raw, true
	}
	for flag, info := range configKeys {
		if info.wire == raw {
			return flag, true
		}
	}
	return "", false
}

func configKeyList() []string {
	keys := make([]string, 0, len(configKeys))
	for k := range configKeys {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	return keys
}

func unknownConfigKey(raw string) error {
	return cli.Exit(fmt.Sprintf("unknown config key %q (valid: %s)", raw, joinKeys(configKeyList())), 2)
}

func joinKeys(keys []string) string {
	out := ""
	for i, k := range keys {
		if i > 0 {
			out += ", "
		}
		out += k
	}
	return out
}

func configFilePath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolving home directory: %w", err)
	}
	return filepath.Join(home, ".__BINARY__", "config"), nil
}

// configActiveProfile mirrors the credentials profile so defaults follow
// the same --profile switch (and is self-contained for auth-less builds).
func configActiveProfile(cmd *cli.Command) string {
	if p := cmd.Root().String("profile"); p != "" {
		return p
	}
	return "default"
}

func loadConfigDefaults() (map[string]map[string]string, error) {
	path, err := configFilePath()
	if err != nil {
		return nil, err
	}
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return map[string]map[string]string{}, nil
	}
	if err != nil {
		return nil, err
	}
	profiles := map[string]map[string]string{}
	if err := toml.Unmarshal(data, &profiles); err != nil {
		return nil, fmt.Errorf("parsing %s: %w", path, err)
	}
	return profiles, nil
}

func saveConfigDefaults(profiles map[string]map[string]string) error {
	path, err := configFilePath()
	if err != nil {
		return err
	}
	for name, kv := range profiles {
		if len(kv) == 0 {
			delete(profiles, name)
		}
	}
	if len(profiles) == 0 {
		if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	data, err := toml.Marshal(profiles)
	if err != nil {
		return err
	}
	// Same torn-write discipline as the credentials file: private temp
	// file, then rename over the destination.
	tmp, err := os.CreateTemp(filepath.Dir(path), ".config-*")
	if err != nil {
		return err
	}
	defer os.Remove(tmp.Name())
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmp.Name(), path)
}

// storedConfigValue returns the active profile's stored default for a
// canonical key. Best effort: an unreadable file means "unset", never a
// hard error in the middle of an unrelated command.
func storedConfigValue(cmd *cli.Command, key string) string {
	profiles, err := loadConfigDefaults()
	if err != nil {
		return ""
	}
	return profiles[configActiveProfile(cmd)][key]
}

// effectiveConfigValue resolves a key the way newClient will, returning the
// value and a human-readable source. MIRRORS newClient — keep in sync.
func effectiveConfigValue(cmd *cli.Command, key string) (string, string) {
	info := configKeys[key]
	if cmd.Root().IsSet(key) {
		return cmd.Root().String(key), "--" + key + " flag"
	}
	if v := os.Getenv(info.env); v != "" {
		if storedConfigValue(cmd, key) != "" {
			return v, "$" + info.env + " (environment) — overrides the stored default"
		}
		return v, "$" + info.env + " (environment)"
	}
	if v := storedConfigValue(cmd, key); v != "" {
		return v, fmt.Sprintf("stored default (profile %q)", configActiveProfile(cmd))
	}
__LOGIN_SOURCE__	return "", "unset"
}

func configCommand() *cli.Command {
	return &cli.Command{
		Name:  "config",
		Usage: "Persistent per-profile defaults for client-level parameters",
		Commands: []*cli.Command{
			{
				Name:      "set",
				Usage:     "Store a default value for a key",
				ArgsUsage: "<key> <value>",
				Action:    configSet,
			},
			{
				Name:      "get",
				Usage:     "Print a key's stored default (empty if unset)",
				ArgsUsage: "<key>",
				Action:    configGet,
			},
			{
				Name:      "unset",
				Usage:     "Remove a key's stored default",
				ArgsUsage: "<key>",
				Action:    configUnset,
			},
			{
				Name:   "list",
				Usage:  "Show every key's effective value and where it comes from",
				Action: configList,
			},
		},
	}
}

func configSet(_ context.Context, cmd *cli.Command) error {
	if cmd.Args().Len() != 2 {
		return cli.Exit("expected exactly 2 positional argument(s) (<key> <value>)", 2)
	}
	key, ok := canonicalConfigKey(cmd.Args().Get(0))
	if !ok {
		return unknownConfigKey(cmd.Args().Get(0))
	}
	value := cmd.Args().Get(1)
	if value == "" {
		return cli.Exit("value must not be empty; use `config unset` to remove a default", 2)
	}
	profiles, err := loadConfigDefaults()
	if err != nil {
		return err
	}
	profile := configActiveProfile(cmd)
	if profiles[profile] == nil {
		profiles[profile] = map[string]string{}
	}
	profiles[profile][key] = value
	if err := saveConfigDefaults(profiles); err != nil {
		return err
	}
	fmt.Printf("Set %s = %s (profile %q).\n", key, value, profile)
	if info := configKeys[key]; os.Getenv(info.env) != "" {
		fmt.Printf("Note: %s is set in this environment and takes precedence over this default.\n", info.env)
	}
	return nil
}

func configGet(_ context.Context, cmd *cli.Command) error {
	if cmd.Args().Len() != 1 {
		return cli.Exit("expected exactly 1 positional argument(s) (<key>)", 2)
	}
	key, ok := canonicalConfigKey(cmd.Args().Get(0))
	if !ok {
		return unknownConfigKey(cmd.Args().Get(0))
	}
	fmt.Println(storedConfigValue(cmd, key))
	return nil
}

func configUnset(_ context.Context, cmd *cli.Command) error {
	if cmd.Args().Len() != 1 {
		return cli.Exit("expected exactly 1 positional argument(s) (<key>)", 2)
	}
	key, ok := canonicalConfigKey(cmd.Args().Get(0))
	if !ok {
		return unknownConfigKey(cmd.Args().Get(0))
	}
	profiles, err := loadConfigDefaults()
	if err != nil {
		return err
	}
	profile := configActiveProfile(cmd)
	if _, ok := profiles[profile][key]; !ok {
		fmt.Printf("No stored default for %s (profile %q).\n", key, profile)
		return nil
	}
	delete(profiles[profile], key)
	if err := saveConfigDefaults(profiles); err != nil {
		return err
	}
	fmt.Printf("Unset %s (profile %q).\n", key, profile)
	return nil
}

func configList(_ context.Context, cmd *cli.Command) error {
	for _, key := range configKeyList() {
		value, source := effectiveConfigValue(cmd, key)
		if value == "" {
			fmt.Printf("%s: unset\n", key)
			continue
		}
		fmt.Printf("%s: %s (%s)\n", key, value, source)
	}
	return nil
}
"#;

const AUTH_GO: &str = r#"// Code generated by redwood. DO NOT EDIT.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	toml "github.com/pelletier/go-toml/v2"
	cli "github.com/urfave/cli/v3"
)

// RFC 8628 device-authorization login. The endpoints are fixed at
// generation time; the credential is handed over exactly once by the token
// endpoint after browser approval and stored in the credentials file.
const (
	authClientID       = "__CLIENT_ID__"
	authDeviceEndpoint = "__DEVICE_ENDPOINT__"
	authTokenEndpoint  = "__TOKEN_ENDPOINT__"
)

// authEndpoint rebases a generation-time endpoint onto __AUTH_BASE_ENV__
// when set — a testing knob for preview deployments. The override supplies
// scheme, host, and any path prefix; the endpoint keeps its own path.
func authEndpoint(configured string) string {
	base := os.Getenv("__AUTH_BASE_ENV__")
	if base == "" {
		return configured
	}
	u, err := url.Parse(configured)
	if err != nil {
		return configured
	}
	return strings.TrimSuffix(base, "/") + u.Path
}

// credWorkspace mirrors the token response's `workspaces` extension member.
type credWorkspace struct {
	ID   string `json:"id" toml:"id"`
	Name string `json:"name" toml:"name"`
}

type credProfile struct {
	Credential string          `toml:"credential"`
	Workspaces []credWorkspace `toml:"workspaces,omitempty"`
}

func credentialsPath() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", fmt.Errorf("resolving home directory: %w", err)
	}
	return filepath.Join(home, ".__BINARY__", "credentials"), nil
}

func loadCredentials() (map[string]credProfile, error) {
	path, err := credentialsPath()
	if err != nil {
		return nil, err
	}
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return map[string]credProfile{}, nil
	}
	if err != nil {
		return nil, err
	}
	profiles := map[string]credProfile{}
	if err := toml.Unmarshal(data, &profiles); err != nil {
		return nil, fmt.Errorf("parsing %s: %w", path, err)
	}
	return profiles, nil
}

func saveCredentials(profiles map[string]credProfile) error {
	path, err := credentialsPath()
	if err != nil {
		return err
	}
	if len(profiles) == 0 {
		if err := os.Remove(path); err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
		return nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o700); err != nil {
		return err
	}
	data, err := toml.Marshal(profiles)
	if err != nil {
		return err
	}
	// 0600 from the first byte: write a private temp file, then rename over
	// the destination so a concurrent reader never sees a torn write.
	tmp, err := os.CreateTemp(filepath.Dir(path), ".credentials-*")
	if err != nil {
		return err
	}
	defer os.Remove(tmp.Name())
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		return err
	}
	if _, err := tmp.Write(data); err != nil {
		tmp.Close()
		return err
	}
	if err := tmp.Close(); err != nil {
		return err
	}
	return os.Rename(tmp.Name(), path)
}

func activeProfile(cmd *cli.Command) string {
	if p := cmd.Root().String("profile"); p != "" {
		return p
	}
	return "default"
}

// credentialSource names the credential ordinary commands will use, in
// precedence order: --api-key flag, then the environment, then the stored
// login. MIRRORS newClient — keep the two in sync, or status lies.
func credentialSource(cmd *cli.Command) string {
	if cmd.Root().IsSet("api-key") {
		return "--api-key flag"
	}
	if os.Getenv("__API_KEY_ENV__") != "" {
		if storedProfile(cmd) != nil {
			return "$__API_KEY_ENV__ (environment) — overrides the stored login"
		}
		return "$__API_KEY_ENV__ (environment)"
	}
	if storedProfile(cmd) != nil {
		return fmt.Sprintf("stored login (profile %q)", activeProfile(cmd))
	}
	return "none — set $__API_KEY_ENV__ or run `__BINARY__ auth login`"
}

// storedProfile returns the active profile's saved login, or nil. Best
// effort by design: a missing or unreadable credentials file means "not
// logged in", never a hard error in the middle of an unrelated command.
func storedProfile(cmd *cli.Command) *credProfile {
	profiles, err := loadCredentials()
	if err != nil {
		return nil
	}
	if prof, ok := profiles[activeProfile(cmd)]; ok && prof.Credential != "" {
		return &prof
	}
	return nil
}

type deviceAuthorization struct {
	DeviceCode              string `json:"device_code"`
	UserCode                string `json:"user_code"`
	VerificationURI         string `json:"verification_uri"`
	VerificationURIComplete string `json:"verification_uri_complete"`
	ExpiresIn               int    `json:"expires_in"`
	Interval                int    `json:"interval"`
}

type tokenResponse struct {
	AccessToken string          `json:"access_token"`
	TokenType   string          `json:"token_type"`
	Workspaces  []credWorkspace `json:"workspaces"`
	Error       string          `json:"error"`
}

func postAuthForm(ctx context.Context, endpoint string, form url.Values, out any) (int, error) {
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, endpoint, strings.NewReader(form.Encode()))
	if err != nil {
		return 0, err
	}
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.Header.Set("Accept", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	if err != nil {
		return 0, err
	}
	if err := json.Unmarshal(body, out); err != nil {
		return resp.StatusCode, fmt.Errorf("%s answered HTTP %d with a non-JSON body", endpoint, resp.StatusCode)
	}
	return resp.StatusCode, nil
}

// openBrowser starts the platform opener. Failure is not an error: the URL
// is always printed for manual use first.
func openBrowser(target string) {
	switch runtime.GOOS {
	case "darwin":
		_ = exec.Command("open", target).Start()
	case "windows":
		_ = exec.Command("rundll32", "url.dll,FileProtocolHandler", target).Start()
	default:
		_ = exec.Command("xdg-open", target).Start()
	}
}

func authCommand() *cli.Command {
	return &cli.Command{
		Name:  "auth",
		Usage: "Log in and manage stored credentials",
		Commands: []*cli.Command{
			{
				Name:  "login",
				Usage: "Authenticate in the browser and store the credential",
				Flags: []cli.Flag{
					&cli.BoolFlag{Name: "no-browser", Usage: "Print the verification URL instead of opening a browser"},
				},
				Action: authLogin,
			},
			{
				Name:   "logout",
				Usage:  "Delete the stored credential for the active profile",
				Action: authLogout,
			},
			{
				Name:   "status",
				Usage:  "Show the stored login and which credential source commands will use",
				Action: authStatus,
			},
		},
	}
}

func authLogin(ctx context.Context, cmd *cli.Command) error {
	form := url.Values{"client_id": {authClientID}}
	if host, err := os.Hostname(); err == nil {
		name := host
		if u := os.Getenv("USER"); u != "" {
			name = u + "@" + host
		}
		form.Set("device_name", name)
	}
	var grant deviceAuthorization
	status, err := postAuthForm(ctx, authEndpoint(authDeviceEndpoint), form, &grant)
	if err != nil {
		return err
	}
	if status != http.StatusOK || grant.DeviceCode == "" {
		return fmt.Errorf("device authorization failed (HTTP %d)", status)
	}
	verification := grant.VerificationURIComplete
	if verification == "" {
		verification = grant.VerificationURI
	}

	// The one-time code prints BEFORE the browser opens so the user can
	// visually match it against the approval page. The device_code (the
	// poll secret) is never displayed.
	fmt.Printf("Your one-time code: %s\n", grant.UserCode)
	fmt.Printf("Approve this login in your browser: %s\n", verification)
	if !cmd.Bool("no-browser") {
		openBrowser(verification)
	}
	fmt.Println("Waiting for approval...")

	interval := time.Duration(grant.Interval) * time.Second
	if interval <= 0 {
		interval = 5 * time.Second
	}
	expires := time.Duration(grant.ExpiresIn) * time.Second
	if expires <= 0 {
		expires = 15 * time.Minute
	}
	deadline := time.Now().Add(expires)
	tokenForm := url.Values{
		"grant_type":  {"urn:ietf:params:oauth:grant-type:device_code"},
		"device_code": {grant.DeviceCode},
		"client_id":   {authClientID},
	}
	for {
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(interval):
		}
		if time.Now().After(deadline) {
			return errors.New("login expired before it was approved; run login again")
		}
		var token tokenResponse
		status, err := postAuthForm(ctx, authEndpoint(authTokenEndpoint), tokenForm, &token)
		if err != nil {
			if ctx.Err() != nil {
				return err
			}
			// Transient transport failure mid-poll (auth servers recycle
			// keep-alive connections on ~5s idle timers — exactly the
			// slow_down gap): the grant is still pending server-side, so
			// keep polling until the deadline instead of aborting a login
			// the user may already have approved in the browser.
			continue
		}
		switch {
		case status == http.StatusOK && token.AccessToken != "":
			profiles, err := loadCredentials()
			if err != nil {
				return err
			}
			profile := activeProfile(cmd)
			profiles[profile] = credProfile{Credential: token.AccessToken, Workspaces: token.Workspaces}
			if err := saveCredentials(profiles); err != nil {
				return err
			}
			path, _ := credentialsPath()
			fmt.Printf("Logged in. Credential saved to %s (profile %q).\n", path, profile)
			// The freshest login is worthless while an ambient key outranks
			// it — say so NOW, not in a 403 an hour later.
			if os.Getenv("__API_KEY_ENV__") != "" {
				fmt.Println("Note: __API_KEY_ENV__ is set in this environment and takes precedence over the stored login for API commands. Unset it to use this login.")
			}
			return nil
		case token.Error == "authorization_pending":
			// Keep polling.
		case token.Error == "slow_down":
			// RFC 8628 §3.5: add 5 seconds to the interval.
			interval += 5 * time.Second
		case token.Error == "access_denied":
			return errors.New("login was denied in the browser")
		case token.Error == "expired_token":
			return errors.New("login expired before it was approved; run login again")
		default:
			return fmt.Errorf("token endpoint failed (HTTP %d: %q)", status, token.Error)
		}
	}
}

func authLogout(_ context.Context, cmd *cli.Command) error {
	profiles, err := loadCredentials()
	if err != nil {
		return err
	}
	profile := activeProfile(cmd)
	if _, ok := profiles[profile]; !ok {
		fmt.Printf("No stored credential for profile %q.\n", profile)
		return nil
	}
	delete(profiles, profile)
	if err := saveCredentials(profiles); err != nil {
		return err
	}
	fmt.Printf("Logged out profile %q.\n", profile)
	return nil
}

func authStatus(_ context.Context, cmd *cli.Command) error {
	profile := activeProfile(cmd)
	prof := storedProfile(cmd)
	if prof == nil {
		fmt.Printf("Profile %q: not logged in. Run `__BINARY__ auth login`.\n", profile)
	} else {
		fmt.Printf("Profile %q: logged in.\n", profile)
		for _, ws := range prof.Workspaces {
			fmt.Printf("  workspace: %s\n", ws.ID)
		}
	}
	// The line that answers "which key are my commands actually using?"
	fmt.Printf("Credential source: %s.\n", credentialSource(cmd))
	return nil
}
"#;

// ---- main.go -----------------------------------------------------------------

fn emit_main(api: &Api, sdk_module: &str, binary: &str, config: &CliConfig) -> String {
    let mut out = format!(
        r#"// Code generated by redwood. DO NOT EDIT.
package main

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/signal"
	"syscall"

	"github.com/urfave/cli/v3"

	sdk "{sdk_module}"
)

// version is the binary release version. Release builds override it with
// -ldflags "-X main.version=<semver>" (goreleaser); this default is the
// generator's configured version and is what `go install` builds report.
// The trailing marker lets release-please bump it in the published repo.
var version = "{version}" // x-release-please-version

func main() {{
	// SIGINT/SIGTERM cancel the command context so long-lived streams close
	// their connections cleanly instead of dying mid-read. The FIRST signal
	// cancels; stop() then restores default handling so a second Ctrl-C
	// terminates the process outright even if shutdown were to hang.
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	go func() {{
		<-ctx.Done()
		stop()
	}}()
	root := &cli.Command{{
		Name:    "{binary}",
		Usage:   "Command-line interface for the {name} API",
		Version: version + " (api {api_version})",
		// Slice flags carry JSON documents; never split values on commas.
		DisableSliceFlagSeparator: true,
		Flags: []cli.Flag{{
			&cli.StringFlag{{Name: "display", Usage: "Output mode for ordinary commands (one of: json, yaml, table, extended); command-local --display overrides"}},
{auth_flags}{profile_flag}			&cli.StringFlag{{Name: "base-url", Usage: "API base URL"}},
			&cli.BoolFlag{{Name: "debug", Sources: cli.EnvVars("{debug_env}"), Usage: "Dump every HTTP exchange (redacted credentials) to stderr"}},
{client_param_flags}		}},
		Commands: []*cli.Command{{
{auth_command}"#,
        name = api.name,
        client_param_flags = api
            .client_params
            .iter()
            .map(|c| {
                format!(
                    "\t\t\t&cli.StringFlag{{Name: \"{flag}\", Usage: \"Default {flag} for commands that take one (default: ${env})\"}},\n",
                    flag = flag_name(&c.wire_name),
                    env = c.env_var
                )
            })
            .collect::<String>(),
        auth_flags = match api.auth {
            Auth::None => String::new(),
            Auth::Basic => format!(
                "\t\t\t&cli.StringFlag{{Name: \"username\", Usage: \"HTTP Basic Auth username (default: ${username_env})\"}},\n\t\t\t&cli.StringFlag{{Name: \"password\", Usage: \"HTTP Basic Auth password (default: ${password_env})\"}},\n",
                username_env = api.basic_username_env,
                password_env = api.basic_password_env,
            ),
            _ if config.auth.is_some() => format!(
                "\t\t\t&cli.StringFlag{{Name: \"api-key\", Usage: \"API key (default: ${env}, then the stored login)\"}},\n",
                env = api.api_key_env
            ),
            _ => format!(
                "\t\t\t&cli.StringFlag{{Name: \"api-key\", Usage: \"API key (default: ${env})\"}},\n",
                env = api.api_key_env
            ),
        },
        profile_flag = if config.auth.is_some() {
            "\t\t\t&cli.StringFlag{Name: \"profile\", Value: \"default\", Usage: \"Stored-credentials profile\"},\n"
        } else {
            ""
        },
        auth_command = {
            let mut cmds = String::from("\t\t\tschemaCommand(),\n");
            if config.auth.is_some() {
                cmds.push_str("\t\t\tauthCommand(),\n");
            }
            if !api.client_params.is_empty() {
                cmds.push_str("\t\t\tconfigCommand(),\n");
            }
            cmds
        },
        version = config.version.as_deref().unwrap_or("0.1.0"),
        debug_env = format!("{}_DEBUG", api.name.to_uppercase()),
        api_version = api.version,
    );
    for resource in api.resources.iter().filter(|r| r.parent.is_none()) {
        writeln!(out, "\t\t\t{}Command(),", lower_camel(&resource.ident)).unwrap();
    }
    write!(
        out,
        r#"		}},
	}}
{alias_calls}	if err := root.Run(ctx, os.Args); err != nil {{
		// An interrupted command (Ctrl-C mid-stream) is not an API failure:
		// exit 130 (128+SIGINT) with no error line, the shell convention.
		if ctx.Err() != nil {{
			os.Exit(130)
		}}
		fmt.Fprintln(os.Stderr, "error:", err)
		// A structured API error often carries the actionable part (e.g.
		// field violations for "validation failed") in details — show it.
		var apiErr *sdk.APIError
		if errors.As(err, &apiErr) && len(apiErr.Details) > 0 {{
			if pretty, jsonErr := json.MarshalIndent(apiErr.Details, "", "  "); jsonErr == nil {{
				fmt.Fprintf(os.Stderr, "details:\n%s\n", pretty)
			}}
		}}
		os.Exit(1)
	}}
}}

func newClient(cmd *cli.Command) (*sdk.Client, error) {{
	// Presence, not truthiness: an explicitly supplied flag is forwarded even
	// when blank so the SDK rejects it and never silently falls back to the
	// ambient environment value.
	opts := []sdk.Option{{}}
{auth_forward}	if cmd.Root().IsSet("base-url") {{
		opts = append(opts, sdk.WithBaseURL(cmd.Root().String("base-url")))
	}}
	if cmd.Root().Bool("debug") {{
		opts = append(opts, sdk.WithDebugLog(os.Stderr))
	}}
{client_param_forward}{config_default_forward}{cred_fallback}	return sdk.NewClient(opts...)
}}
"#,
        alias_calls = config
            .aliases
            .iter()
            .map(|(alias, target)| {
                let segs = target
                    .split_whitespace()
                    .map(|seg| format!(", \"{seg}\""))
                    .collect::<String>();
                format!("\t// Top-level alias ([lang.cli.aliases]), validated at generation.\n\taddAlias(root, \"{alias}\"{segs})\n")
            })
            .collect::<String>(),
        cred_fallback = match (&config.auth, &api.auth) {
            (Some(auth), Auth::Bearer | Auth::ApiKeyHeader(_)) => {
                let workspace_default = auth
                    .workspaces_param
                    .as_deref()
                    .and_then(|wire| api.client_params.iter().find(|c| c.wire_name == wire))
                    .map(|c| {
                        format!(
                            "\t\t\t// Exactly one authorized workspace: its id becomes the\n\t\t\t// default {flag}, unless the flag, env, or a stored\n\t\t\t// `config set` default supplied one.\n\t\t\tif len(prof.Workspaces) == 1 && !cmd.Root().IsSet(\"{flag}\") && os.Getenv(\"{env}\") == \"\" && storedConfigValue(cmd, \"{flag}\") == \"\" {{\n\t\t\t\topts = append(opts, sdk.With{go}(prof.Workspaces[0].ID))\n\t\t\t}}\n",
                            flag = flag_name(&c.wire_name),
                            env = c.env_var,
                            go = crate::backends::golang::go_name(&c.wire_name)
                        )
                    })
                    .unwrap_or_default();
                format!(
                    "\t// Stored-login fallback: lowest precedence, after the --api-key\n\t// flag and the environment. An explicit source always wins.\n\tif !cmd.Root().IsSet(\"api-key\") && os.Getenv(\"{env}\") == \"\" {{\n\t\tif prof := storedProfile(cmd); prof != nil {{\n\t\t\topts = append(opts, sdk.WithAPIKey(prof.Credential))\n{workspace_default}\t\t}}\n\t}}\n",
                    env = api.api_key_env
                )
            }
            _ => String::new(),
        },
        config_default_forward = api
            .client_params
            .iter()
            .map(|c| {
                // Stored default (`config set`): below the flag and the
                // environment — an explicit source always wins.
                format!(
                    "\tif !cmd.Root().IsSet(\"{flag}\") && os.Getenv(\"{env}\") == \"\" {{\n\t\tif v := storedConfigValue(cmd, \"{flag}\"); v != \"\" {{\n\t\t\topts = append(opts, sdk.With{go}(v))\n\t\t}}\n\t}}\n",
                    flag = flag_name(&c.wire_name),
                    env = c.env_var,
                    go = crate::backends::golang::go_name(&c.wire_name)
                )
            })
            .collect::<String>(),
        client_param_forward = api
            .client_params
            .iter()
            .map(|c| {
                // Presence, not truthiness: an explicitly blank root flag
                // reaches the SDK and is rejected there. An operation-local
                // flag still overrides this client default.
                format!(
                    "\tif cmd.Root().IsSet(\"{flag}\") {{\n\t\topts = append(opts, sdk.With{go}(cmd.Root().String(\"{flag}\")))\n\t}}\n",
                    flag = flag_name(&c.wire_name),
                    go = crate::backends::golang::go_name(&c.wire_name)
                )
            })
            .collect::<String>(),
        auth_forward = match api.auth {
            Auth::None => String::new(),
            Auth::Basic => format!(
                "\tif cmd.Root().IsSet(\"username\") || cmd.Root().IsSet(\"password\") {{\n\t\tusername := os.Getenv(\"{username_env}\")\n\t\tif cmd.Root().IsSet(\"username\") {{\n\t\t\tusername = cmd.Root().String(\"username\")\n\t\t}}\n\t\tpassword := os.Getenv(\"{password_env}\")\n\t\tif cmd.Root().IsSet(\"password\") {{\n\t\t\tpassword = cmd.Root().String(\"password\")\n\t\t}}\n\t\topts = append(opts, sdk.WithBasicAuth(username, password))\n\t}}\n",
                username_env = api.basic_username_env,
                password_env = api.basic_password_env,
            ),
            _ => "\tif cmd.Root().IsSet(\"api-key\") {\n\t\topts = append(opts, sdk.WithAPIKey(cmd.Root().String(\"api-key\")))\n\t}\n".to_string(),
        },
    )
    .unwrap();
    out
}

fn lower_camel(snake: &str) -> String {
    let pascal = go_name(snake);
    let mut c = pascal.chars();
    match c.next() {
        Some(first) => first.to_lowercase().collect::<String>() + c.as_str(),
        None => pascal,
    }
}

// ---- helpers.go ----------------------------------------------------------------

const RT_HELPERS: &str = include_str!("../../runtime/cli/helpers.go");
const RT_BODY: &str = include_str!("../../runtime/cli/body.go");
const RT_BODY_TEST: &str = include_str!("../../runtime/cli/body_test.go");
const RT_CONVERSION: &str = include_str!("../../runtime/cli/conversion.go");

fn emit_helpers(_api: &Api, _sdk_module: &str) -> String {
    RT_HELPERS.to_string()
}

fn conversion_stem(resource: &Resource, op: &Operation) -> String {
    format!(
        "{}{}",
        super::golang::exported_name(&resource.ident),
        super::golang::exported_name(&op.name)
    )
}

fn conversion_type_name(resource: &Resource, op: &Operation) -> String {
    format!("{}Conversion", conversion_stem(resource, op))
}

fn conversion_func_name(resource: &Resource, op: &Operation) -> String {
    format!("Convert{}", conversion_stem(resource, op))
}

/// Emit the IR-owned conversion for one operation separately from its urfave
/// command declaration. The generated function is the only layer that knows
/// both flag names/accessors and SDK parameter fields.
fn emit_operation_conversion(
    api: &Api,
    resource: &Resource,
    op: &Operation,
    sdk_module: &str,
    plan: Option<&crate::ir::plan::BodyPlan>,
) -> String {
    let conversion = conversion_type_name(resource, op);
    let function = conversion_func_name(resource, op);
    let params = super::golang::params_type_name_pub(resource, op);
    let mut out = format!(
        "// Code generated by redwood. DO NOT EDIT.\npackage commands\n\nimport (\n\tcli \"github.com/urfave/cli/v3\"\n\n\tsdk \"{sdk_module}\"\n)\n\n"
    );
    if plan.is_some() {
        out.push_str(&cli_inputs::emit_schema_const(
            &cli_inputs::schema_const_name(resource, op),
            &cli_inputs::request_schema_json(api, op),
        ));
        out.push('\n');
    }
    writeln!(
        out,
        "// {conversion} is the typed request produced from one urfave command.\ntype {conversion} struct {{\n\tParams sdk.{params}\n\tBody any\n}}\n"
    )
    .unwrap();
    writeln!(
        out,
        "// {function} reads urfave values according to Redwood's operation IR.\nfunc {function}(cmd *cli.Command, out *{conversion}) error {{"
    )
    .unwrap();
    writeln!(out, "\tvalues := map[string]any{{}}").unwrap();
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        emit_flag_read(api, &p.wire_name, &p.ty, &mut out);
    }
    if let Some(plan) = plan {
        cli_inputs::emit_action(
            op,
            plan,
            &cli_inputs::schema_const_name(resource, op),
            &mut out,
        );
        cli_inputs::emit_merge_into_values(plan, &mut out);
    }
    writeln!(
        out,
        "\tif err := decodeParams(values, &out.Params); err != nil {{\n\t\treturn cli.Exit(err.Error(), 2)\n\t}}"
    )
    .unwrap();
    if let Some(plan) = plan {
        if plan.whole_body {
            writeln!(
                out,
                "\tif _rawBody != nil {{\n\t\tout.Body = _rawBody\n\t}} else {{\n\t\tout.Body = _body.body\n\t}}"
            )
            .unwrap();
        } else {
            writeln!(out, "\tout.Body = _body.body").unwrap();
        }
    }
    writeln!(out, "\treturn nil\n}}").unwrap();
    out
}

// ---- per-resource command files -------------------------------------------------

fn emit_resource_command(
    api: &Api,
    resource: &Resource,
    cli_module: &str,
    sdk_module: &str,
    display: &(String, &crate::config::CliDisplayConfig),
    plans: &Plans,
) -> String {
    // Every op command uses fmt for its argument-cardinality usage errors;
    // positional ops also trim-check the ID.
    let needs_fmt = !resource.operations.is_empty();
    let needs_strings = resource.operations.iter().any(|op| {
        !op.positionals.is_empty()
            || op
                .path_params
                .iter()
                .chain(op.query_params.iter())
                .any(|p| p.required && client_param(api, &p.wire_name).is_none())
    });
    let needs_commands = resource.operations.iter().any(Operation::has_params);
    let needs_sdk = resource
        .operations
        .iter()
        .any(|op| matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_))));
    let fmt_import = if needs_fmt { "\t\"fmt\"\n" } else { "" };
    let strings_import = if needs_strings { "\t\"strings\"\n" } else { "" };
    let sdk_import = if needs_sdk {
        format!("\n\tsdk \"{sdk_module}\"\n")
    } else {
        String::new()
    };
    let commands_import = if needs_commands {
        format!("\n\tcommands \"{cli_module}/internal/commands\"\n")
    } else {
        String::new()
    };
    let mut out = format!(
        "// Code generated by redwood. DO NOT EDIT.\npackage main\n\nimport (\n\t\"context\"\n{fmt_import}{strings_import}\n\t\"github.com/urfave/cli/v3\"\n{commands_import}{sdk_import})\n\n",
    );
    let func = lower_camel(&resource.ident);
    writeln!(out, "func {func}Command() *cli.Command {{").unwrap();
    writeln!(out, "\treturn &cli.Command{{").unwrap();
    writeln!(out, "\t\tName:  \"{}\",", command_name(&resource.name)).unwrap();
    // Each command's setup re-reads this into package state; it must be set
    // on every level or a child resets it and comma-splits JSON slice flags.
    writeln!(out, "\t\tDisableSliceFlagSeparator: true,").unwrap();
    if let Some(d) = &resource.description {
        writeln!(out, "\t\tUsage: \"{}\",", escape_go(&first_line(d))).unwrap();
    }
    writeln!(out, "\t\tCommands: []*cli.Command{{").unwrap();
    for op in &resource.operations {
        emit_op_command(api, resource, op, display, plans.get(&op.id), &mut out);
    }
    // Nested resources become subcommand groups.
    for child in api
        .resources
        .iter()
        .filter(|r| r.parent.as_deref() == Some(resource.name.as_str()))
    {
        writeln!(out, "\t\t\t{}Command(),", lower_camel(&child.ident)).unwrap();
    }
    writeln!(out, "\t\t}},").unwrap();
    writeln!(out, "\t}}\n}}").unwrap();
    out
}

/// Collapse a multi-line description into one help line: whole sentences,
/// never cut mid-line, capped at a word boundary.
pub(crate) fn first_line(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= 160 {
        return collapsed;
    }
    let cut = collapsed[..160].rfind(' ').unwrap_or(160);
    format!("{}…", &collapsed[..cut])
}

pub(crate) fn escape_go(text: &str) -> String {
    text.replace('\\', "\\\\").replace('"', "\\\"")
}

fn client_param<'a>(api: &'a Api, wire_name: &str) -> Option<&'a ClientParam> {
    api.client_params.iter().find(|c| c.wire_name == wire_name)
}

fn emit_op_command(
    api: &Api,
    resource: &Resource,
    op: &Operation,
    display: &(String, &crate::config::CliDisplayConfig),
    plan: Option<&crate::ir::plan::BodyPlan>,
    out: &mut String,
) {
    writeln!(out, "\t\t\t{{").unwrap();
    writeln!(out, "\t\t\t\tName:  \"{}\",", command_name(&op.name)).unwrap();
    writeln!(out, "\t\t\t\tDisableSliceFlagSeparator: true,").unwrap();
    let usage = op.summary.as_deref().map(first_line).unwrap_or_default();
    if !usage.is_empty() {
        writeln!(out, "\t\t\t\tUsage: \"{}\",", escape_go(&usage)).unwrap();
    }
    if !op.positionals.is_empty() {
        let operands: Vec<String> = op
            .positionals
            .iter()
            .map(|p| format!("<{}>", flag_name(&p.wire_name)))
            .collect();
        writeln!(out, "\t\t\t\tArgsUsage: \"{}\",", operands.join(" ")).unwrap();
    }

    // Flags.
    let mut flag_specs: Vec<(&Param, bool)> = Vec::new(); // (param, required)
    for p in op.path_params.iter().chain(op.query_params.iter()) {
        let required = p.required && client_param(api, &p.wire_name).is_none();
        flag_specs.push((p, required));
    }
    // The stream-resume flag is a real flag too: a parameterless SSE
    // operation must still accept --last-event-id (capability, not
    // parameter count, decides).
    let is_stream = matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_)));
    let has_flags = true; // every command carries at least --display
    let _ = is_stream;
    if has_flags {
        writeln!(out, "\t\t\t\tFlags: []cli.Flag{{").unwrap();
        writeln!(
            out,
            "\t\t\t\t\t&cli.StringFlag{{Name: \"display\", Usage: \"Output mode (one of: json, yaml, table, extended)\"}},"
        )
        .unwrap();
        for (p, required) in &flag_specs {
            emit_flag(
                api,
                &p.wire_name,
                &p.ty,
                *required,
                p.description.as_deref(),
                out,
            );
        }
        if let Some(plan) = plan {
            cli_inputs::emit_flags(plan, out);
        }
        if is_stream {
            writeln!(
                out,
                "\t\t\t\t\t&cli.StringFlag{{Name: \"last-event-id\", Usage: \"Resume the stream after this event id (an explicitly empty value clears the checkpoint)\"}},"
            )
            .unwrap();
        }
        writeln!(out, "\t\t\t\t}},").unwrap();
    }

    // Action.
    writeln!(
        out,
        "\t\t\t\tAction: func(ctx context.Context, cmd *cli.Command) error {{"
    )
    .unwrap();
    // Enforce exact positional cardinality BEFORE building a client: a stray
    // token next to a mutation must be a usage error (exit 2), not ignored.
    match op.positionals.as_slice() {
        positionals if !positionals.is_empty() => {
            let grammar: String = positionals
                .iter()
                .map(|p| format!("<{}>", flag_name(&p.wire_name)))
                .collect::<Vec<_>>()
                .join(" ");
            writeln!(
                out,
                "\t\t\t\t\tif cmd.Args().Len() != {n} {{\n\t\t\t\t\t\treturn cli.Exit(fmt.Sprintf(\"expected exactly {n} positional argument(s) ({grammar}), got %d\", cmd.Args().Len()), 2)\n\t\t\t\t\t}}",
                n = positionals.len()
            )
            .unwrap();
            // Every token must be usable: `cmd method ""` must not reach
            // the network with an empty path identifier.
            for (i, p) in positionals.iter().enumerate() {
                writeln!(
                    out,
                    "\t\t\t\t\tif strings.TrimSpace(cmd.Args().Get({i})) == \"\" {{\n\t\t\t\t\t\treturn cli.Exit(\"<{}> must not be empty\", 2)\n\t\t\t\t\t}}",
                    flag_name(&p.wire_name)
                )
                .unwrap();
            }
        }
        _ => {
            writeln!(
                out,
                "\t\t\t\t\tif cmd.Args().Len() != 0 {{\n\t\t\t\t\t\treturn cli.Exit(fmt.Sprintf(\"unexpected positional arguments: %v\", cmd.Args().Slice()), 2)\n\t\t\t\t\t}}"
            )
            .unwrap();
        }
    }
    // Display preflight: validate the mode value and reject table/extended
    // where no configured column statically applies (or for streams) BEFORE
    // any transport. json stays the stable machine default.
    {
        let (default_mode, display_config) = display;
        let columns = effective_columns(op, display_config).expect("validated at generate()");
        let model = match (&op.pagination, &op.response) {
            (Some(page), _) => Some(page.item_ty.clone()),
            (None, ResponseKind::Json(ty)) => Some(ty.clone()),
            _ => None,
        };
        let applicable: Vec<usize> = match &model {
            Some(ty) => applicable_columns(api, ty, &columns).expect("validated at generate()"),
            None => Vec::new(),
        };
        // The CONFIGURED default applies only where it can render: streams
        // and void ops fall back to json unless a flag explicitly asks for
        // more, so a table-by-default CLI doesn't brick its own streams.
        let is_stream_op = matches!((&op.pagination, &op.response), (None, ResponseKind::Sse(_)));
        let op_default = if is_stream_op || model.is_none() {
            "json"
        } else {
            default_mode
        };
        let dry_guard = if plan.is_some() {
            " && !cmd.Bool(\"dry-run\")"
        } else {
            ""
        };
        writeln!(
            out,
            "\t\t\t\t\t_display := displayMode(cmd, \"{op_default}\")"
        )
        .unwrap();
        writeln!(
            out,
            "\t\t\t\t\tif !isOneOf(_display, []string{{\"json\", \"yaml\", \"table\", \"extended\"}}) {{\n\t\t\t\t\t\treturn cli.Exit(fmt.Sprintf(\"--display: invalid value %q (valid: json, yaml, table, extended)\", _display), 2)\n\t\t\t\t\t}}"
        )
        .unwrap();
        if is_stream_op {
            writeln!(
                out,
                "\t\t\t\t\tif _display != \"json\" {{\n\t\t\t\t\t\treturn cli.Exit(\"streaming commands support only --display json (one JSON document per event)\", 2)\n\t\t\t\t\t}}"
            )
            .unwrap();
        } else if model.is_none() {
            // Void ops: no payload to render; only json (a no-op) allowed.
            // A dry run prints the body, which any mode can show.
            writeln!(
                out,
                "\t\t\t\t\tif _display != \"json\" && _display != \"yaml\"{dry_guard} {{\n\t\t\t\t\t\treturn cli.Exit(\"this command has no displayable response; use --display json\", 2)\n\t\t\t\t\t}}"
            )
            .unwrap();
        } else if applicable.is_empty() {
            writeln!(
                out,
                "\t\t\t\t\tif _display != \"json\" && _display != \"yaml\"{dry_guard} {{\n\t\t\t\t\t\treturn cli.Exit(\"no display columns apply to this command; use --display json or yaml\", 2)\n\t\t\t\t\t}}"
            )
            .unwrap();
            writeln!(out, "\t\t\t\t\t_columns := []displayColumn(nil)").unwrap();
        } else {
            let literal: Vec<String> = applicable
                .iter()
                .map(|&i| {
                    let column = &columns[i];
                    let segs = column
                        .segments
                        .iter()
                        .map(|seg| format!("\"{seg}\""))
                        .collect::<Vec<_>>()
                        .join(", ");
                    let truncate = column
                        .truncate
                        .map(|n| format!(", truncate: {n}"))
                        .unwrap_or_default();
                    format!(
                        "{{header: \"{}\", path: []string{{{segs}}}{truncate}}}",
                        escape_go(&column.header)
                    )
                })
                .collect();
            writeln!(
                out,
                "\t\t\t\t\t_columns := []displayColumn{{{}}}",
                literal.join(", ")
            )
            .unwrap();
        }
    }

    // Generator-owned required-flag preflight: urfave's Required handling
    // exits 1 through a private error type; the documented usage contract is
    // exit 2, before credential resolution or transport. Client-defaultable
    // params are NOT required here — they resolve from flag/root/env.
    {
        let mut required_flags: Vec<String> = Vec::new();
        for p in op.path_params.iter().chain(op.query_params.iter()) {
            if p.required && client_param(api, &p.wire_name).is_none() {
                required_flags.push(flag_name(&p.wire_name));
            }
        }
        // Body inputs are checked on the assembled body (documents can
        // satisfy them), after every flag has been read.
        if !required_flags.is_empty() {
            writeln!(out, "\t\t\t\t\t_missing := []string{{}}").unwrap();
            for flag in &required_flags {
                writeln!(
                    out,
                    "\t\t\t\t\tif !cmd.IsSet(\"{flag}\") {{\n\t\t\t\t\t\t_missing = append(_missing, \"--{flag}\")\n\t\t\t\t\t}}"
                )
                .unwrap();
            }
            writeln!(
                out,
                "\t\t\t\t\tif len(_missing) > 0 {{\n\t\t\t\t\t\treturn cli.Exit(\"required flag(s) not set: \"+strings.Join(_missing, \", \"), 2)\n\t\t\t\t\t}}"
            )
            .unwrap();
        }
    }

    // Enum flags validate OFFLINE before credentials/transport: config and
    // contract errors must be deterministic without a network call.
    {
        let mut checks: Vec<(String, Vec<String>, bool)> = Vec::new();
        for p in op.path_params.iter().chain(op.query_params.iter()) {
            if let Some(values) = enum_values(api, &p.ty) {
                checks.push((flag_name(&p.wire_name), values, matches!(p.ty, Ty::List(_))));
            }
        }
        for (flag, values, is_slice) in checks {
            let list = values
                .iter()
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(", ");
            if is_slice {
                writeln!(
                    out,
                    "\t\t\t\t\tfor _, _v := range cmd.StringSlice(\"{flag}\") {{\n\t\t\t\t\t\tif !isOneOf(_v, []string{{{list}}}) {{\n\t\t\t\t\t\t\treturn cli.Exit(fmt.Sprintf(\"--{flag}: invalid value %q (valid: {plain})\", _v), 2)\n\t\t\t\t\t\t}}\n\t\t\t\t\t}}",
                    plain = values.join(", ")
                )
                .unwrap();
            } else {
                writeln!(
                    out,
                    "\t\t\t\t\tif cmd.IsSet(\"{flag}\") && !isOneOf(cmd.String(\"{flag}\"), []string{{{list}}}) {{\n\t\t\t\t\t\treturn cli.Exit(fmt.Sprintf(\"--{flag}: invalid value %q (valid: {plain})\", cmd.String(\"{flag}\")), 2)\n\t\t\t\t\t}}",
                    plain = values.join(", ")
                )
                .unwrap();
            }
        }
    }
    // stdin budget preflight: '-' may feed at most ONE JSON argument, and a
    // doomed command must fail before draining/blocking on stdin or building
    // a client.
    {
        let (singles, slices) = match plan {
            Some(plan) => cli_inputs::stdin_inputs(plan),
            None => (Vec::new(), Vec::new()),
        };
        if !singles.is_empty() || !slices.is_empty() {
            let mut expr = String::from("[]string{");
            expr.push_str(
                &singles
                    .iter()
                    .map(|n| format!("cmd.String(\"{n}\")"))
                    .collect::<Vec<_>>()
                    .join(", "),
            );
            expr.push('}');
            writeln!(out, "\t\t\t\t\t_stdinInputs := {expr}").unwrap();
            for n in &slices {
                writeln!(
                    out,
                    "\t\t\t\t\t_stdinInputs = append(_stdinInputs, cmd.StringSlice(\"{n}\")...)"
                )
                .unwrap();
            }
            writeln!(
                out,
                "\t\t\t\t\tif err := stdinBudget(_stdinInputs); err != nil {{\n\t\t\t\t\t\treturn cli.Exit(err.Error(), 2)\n\t\t\t\t\t}}"
            )
            .unwrap();
        }
    }
    for (i, p) in op.positionals.iter().enumerate() {
        writeln!(
            out,
            "\t\t\t\t\tpos{i} := cmd.Args().Get({i}) // {}",
            flag_name(&p.wire_name)
        )
        .unwrap();
    }

    let mut call_args = vec!["ctx".to_string()];
    for (i, _) in op.positionals.iter().enumerate() {
        call_args.push(format!("pos{i}"));
    }
    if op.has_params() {
        let conversion = conversion_type_name(resource, op);
        let function = conversion_func_name(resource, op);
        writeln!(out, "\t\t\t\t\tvar converted commands.{conversion}").unwrap();
        writeln!(
            out,
            "\t\t\t\t\tif err := commands.{function}(cmd, &converted); err != nil {{\n\t\t\t\t\t\treturn err\n\t\t\t\t\t}}"
        )
        .unwrap();
        if plan.is_some() {
            writeln!(
                out,
                "\t\t\t\t\tif cmd.Bool(\"dry-run\") {{\n\t\t\t\t\t\treturn printDocument(_display, converted.Body)\n\t\t\t\t\t}}"
            )
            .unwrap();
        }
        call_args.push("&converted.Params".to_string());
    }
    // Phase two: only a fully validated invocation resolves client
    // configuration — a malformed --metadata must never be masked by a
    // missing-credential error.
    writeln!(out, "\t\t\t\t\tclient, err := newClient(cmd)").unwrap();
    writeln!(
        out,
        "\t\t\t\t\tif err != nil {{\n\t\t\t\t\t\treturn err\n\t\t\t\t\t}}"
    )
    .unwrap();

    let accessor = match &resource.parent {
        Some(parent) => format!("client.{}().{}()", go_name(parent), go_name(&resource.name)),
        None => format!("client.{}()", go_name(&resource.name)),
    };
    let invoke = format!("{accessor}.{}({})", go_name(&op.name), call_args.join(", "));
    match (&op.pagination, &op.response) {
        (Some(_), _) => {
            writeln!(out, "\t\t\t\t\tpage, err := {invoke}").unwrap();
            writeln!(
                out,
                "\t\t\t\t\tif err != nil {{\n\t\t\t\t\t\treturn err\n\t\t\t\t\t}}"
            )
            .unwrap();
            writeln!(
                out,
                "\t\t\t\t\treturn renderDisplay(_display, _columns, true, map[string]any{{\"items\": page.Items, \"nextCursor\": page.NextCursor}})"
            )
            .unwrap();
        }
        (None, ResponseKind::Json(_)) => {
            writeln!(out, "\t\t\t\t\tout, err := {invoke}").unwrap();
            writeln!(
                out,
                "\t\t\t\t\tif err != nil {{\n\t\t\t\t\t\treturn err\n\t\t\t\t\t}}"
            )
            .unwrap();
            writeln!(
                out,
                "\t\t\t\t\treturn renderDisplay(_display, _columns, false, out)"
            )
            .unwrap();
        }
        (None, ResponseKind::Sse(_)) => {
            // IsSet distinguishes an explicitly empty flag (clear the
            // checkpoint) from an unset one (fresh stream).
            writeln!(out, "\t\t\t\t\tstreamOpts := []sdk.RequestOption(nil)").unwrap();
            writeln!(out, "\t\t\t\t\tif cmd.IsSet(\"last-event-id\") {{").unwrap();
            writeln!(
                out,
                "\t\t\t\t\t\tstreamOpts = append(streamOpts, sdk.WithLastEventID(cmd.String(\"last-event-id\")))"
            )
            .unwrap();
            writeln!(out, "\t\t\t\t\t}}").unwrap();
            // Splice into the FINAL call only — the accessor chain contains
            // its own parentheses.
            let invoke_opts = match invoke.rfind(')') {
                Some(pos) => format!("{}, streamOpts...{}", &invoke[..pos], &invoke[pos..]),
                None => invoke.clone(),
            };
            writeln!(out, "\t\t\t\t\tstream, err := {invoke_opts}").unwrap();
            writeln!(
                out,
                "\t\t\t\t\tif err != nil {{\n\t\t\t\t\t\treturn err\n\t\t\t\t\t}}"
            )
            .unwrap();
            writeln!(out, "\t\t\t\t\tdefer stream.Close()").unwrap();
            writeln!(out, "\t\t\t\t\tfor stream.Next() {{").unwrap();
            writeln!(
                out,
                "\t\t\t\t\t\tif err := printJSONLine(stream.Current()); err != nil {{\n\t\t\t\t\t\t\treturn err\n\t\t\t\t\t\t}}"
            )
            .unwrap();
            writeln!(out, "\t\t\t\t\t}}").unwrap();
            writeln!(out, "\t\t\t\t\treturn stream.Err()").unwrap();
        }
        (None, ResponseKind::Empty) => {
            writeln!(out, "\t\t\t\t\treturn {invoke}").unwrap();
        }
    }
    writeln!(out, "\t\t\t\t}},").unwrap();
    writeln!(out, "\t\t\t}},").unwrap();
}

fn emit_flag(
    api: &Api,
    wire: &str,
    ty: &Ty,
    required: bool,
    description: Option<&str>,
    out: &mut String,
) {
    let name = flag_name(wire);
    // urfave's own Required handling errors through a PRIVATE type that
    // exits 1, violating the documented usage contract — requiredness is
    // marked in help here and ENFORCED by a generator-owned exit-2
    // preflight in the action instead.
    let req = "";
    // Enum choices belong in help so users never learn valid values from a
    // failed network call.
    let choices = enum_values(api, ty)
        .map(|v| format!(" (one of: {})", v.join(", ")))
        .unwrap_or_default();
    let usage = {
        let text = description.map(first_line).unwrap_or_default();
        let marker = if required { "Required. " } else { "" };
        if text.is_empty() && choices.is_empty() && marker.is_empty() {
            String::new()
        } else {
            format!(
                ", Usage: \"{marker}{}{}\"",
                escape_go(&text),
                escape_go(&choices)
            )
        }
    };
    let decl = match classify(api, ty) {
        FlagKind::Str => format!("&cli.StringFlag{{Name: \"{name}\"{usage}{req}}}"),
        FlagKind::Bool => format!("&cli.BoolFlag{{Name: \"{name}\"{usage}{req}}}"),
        FlagKind::Int32 => format!("&cli.Int32Flag{{Name: \"{name}\"{usage}{req}}}"),
        FlagKind::Int64 => format!("&cli.Int64Flag{{Name: \"{name}\"{usage}{req}}}"),
        FlagKind::Float32 => format!("&cli.Float32Flag{{Name: \"{name}\"{usage}{req}}}"),
        FlagKind::Float64 => format!("&cli.Float64Flag{{Name: \"{name}\"{usage}{req}}}"),
        FlagKind::StrSlice | FlagKind::JsonSlice => {
            format!("&cli.StringSliceFlag{{Name: \"{name}\"{usage}{req}}}")
        }
        FlagKind::Json => {
            let marker = if required { "Required. " } else { "" };
            format!(
                "&cli.StringFlag{{Name: \"{name}\", Usage: \"{marker}JSON document (literal, @file, or - for stdin)\"{req}}}"
            )
        }
    };
    writeln!(out, "\t\t\t\t\t{decl},").unwrap();
}

fn emit_flag_read(api: &Api, wire: &str, ty: &Ty, out: &mut String) {
    let name = flag_name(wire);
    writeln!(out, "\t\t\t\t\tif cmd.IsSet(\"{name}\") {{").unwrap();
    emit_flag_read_into(api, "values", wire, ty, out);
    writeln!(out, "\t\t\t\t\t}}").unwrap();
}

/// Assign the parsed flag value into `target[wire]` (no IsSet guard).
fn emit_flag_read_into(api: &Api, target: &str, wire: &str, ty: &Ty, out: &mut String) {
    let name = flag_name(wire);
    let values = target;
    match classify(api, ty) {
        FlagKind::Str => {
            writeln!(
                out,
                "\t\t\t\t\t\t{values}[\"{wire}\"] = cmd.String(\"{name}\")"
            )
            .unwrap();
        }
        FlagKind::Bool => {
            writeln!(
                out,
                "\t\t\t\t\t\t{values}[\"{wire}\"] = cmd.Bool(\"{name}\")"
            )
            .unwrap();
        }
        FlagKind::Int32 => {
            writeln!(
                out,
                "\t\t\t\t\t\t{values}[\"{wire}\"] = cmd.Int32(\"{name}\")"
            )
            .unwrap();
        }
        FlagKind::Int64 => {
            writeln!(
                out,
                "\t\t\t\t\t\t{values}[\"{wire}\"] = cmd.Int64(\"{name}\")"
            )
            .unwrap();
        }
        FlagKind::Float32 => {
            writeln!(
                out,
                "\t\t\t\t\t\t{values}[\"{wire}\"] = cmd.Float32(\"{name}\")"
            )
            .unwrap();
        }
        FlagKind::Float64 => {
            writeln!(
                out,
                "\t\t\t\t\t\t{values}[\"{wire}\"] = cmd.Float64(\"{name}\")"
            )
            .unwrap();
        }
        FlagKind::StrSlice => {
            writeln!(
                out,
                "\t\t\t\t\t\t{values}[\"{wire}\"] = cmd.StringSlice(\"{name}\")"
            )
            .unwrap();
        }
        FlagKind::JsonSlice => {
            writeln!(
                out,
                "\t\t\t\t\t\titems, err := jsonSliceArg(\"{name}\", cmd.StringSlice(\"{name}\"))"
            )
            .unwrap();
            writeln!(out, "\t\t\t\t\t\tif err != nil {{\n\t\t\t\t\t\t\treturn cli.Exit(err.Error(), 2)\n\t\t\t\t\t\t}}").unwrap();
            writeln!(out, "\t\t\t\t\t\t{values}[\"{wire}\"] = items").unwrap();
        }
        FlagKind::Json => {
            writeln!(
                out,
                "\t\t\t\t\t\tdoc, err := jsonArg(\"{name}\", cmd.String(\"{name}\"))"
            )
            .unwrap();
            writeln!(out, "\t\t\t\t\t\tif err != nil {{\n\t\t\t\t\t\t\treturn cli.Exit(err.Error(), 2)\n\t\t\t\t\t\t}}").unwrap();
            writeln!(out, "\t\t\t\t\t\t{values}[\"{wire}\"] = doc").unwrap();
        }
    }
}
