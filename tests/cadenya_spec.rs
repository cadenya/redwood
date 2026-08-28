//! End-to-end guard against the real Cadenya spec: the full pipeline must
//! lower it, and the invariants that make the SDK production-shaped must hold.

use redwood::backends::{docs::DocsBackend, typescript::TypeScriptBackend, Backend};
use redwood::ir::{self, ResponseKind, Shape};

fn api() -> ir::Api {
    let source = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/api-spec.yml"))
        .expect("spec fixture present");
    let spec = redwood::openapi::parse(&source).expect("spec parses");
    ir::lower::lower(&spec).expect("spec lowers")
}

#[test]
fn lowers_the_full_cadenya_spec() {
    let api = api();
    let op_count: usize = api.resources.iter().map(|r| r.operations.len()).sum();
    assert!(
        op_count >= 95,
        "expected all 95+ operations, got {op_count}"
    );
    assert!(api.types.len() > 100, "expected the full schema set");
}

#[test]
fn finds_the_streaming_and_paginated_operations() {
    let api = api();
    let ops: Vec<_> = api
        .resources
        .iter()
        .flat_map(|r| r.operations.iter())
        .collect();
    assert!(
        ops.iter()
            .any(|o| matches!(o.response, ResponseKind::Sse(_))),
        "the events:stream endpoint must lower to SSE"
    );
    let paginated = ops.iter().filter(|o| o.pagination.is_some()).count();
    assert!(
        paginated >= 10,
        "expected many cursor-paginated lists, got {paginated}"
    );
}

#[test]
fn discriminated_unions_survive() {
    let api = api();
    let discriminated = api
        .types
        .values()
        .filter(|d| matches!(&d.shape, Shape::Union(u) if u.discriminator.is_some()))
        .count();
    assert!(
        discriminated >= 5,
        "expected the oneOf+discriminator schemas, got {discriminated}"
    );
}

#[test]
fn typescript_and_docs_backends_generate() {
    let api = api();
    let ts = TypeScriptBackend {
        config: Default::default(),
    }
    .generate(&api)
    .expect("typescript generates");
    assert!(ts.contains_key("src/types.ts"));
    assert!(ts.contains_key("src/client.ts"));
    assert!(ts.len() > 25);

    let docs = DocsBackend.generate(&api).expect("docs generate");
    let api_md = &docs["api.md"];
    assert!(api_md.contains("client.objectives.streamEvents(objectiveId, { ...params }) -> Stream&lt;ObjectiveEvent&gt;"));
    assert!(api_md.contains("Page&lt;Objective&gt;"));
}

/// A discriminated-union request body (proto `oneof` at the top of the
/// message) is a set of mutually exclusive typed flags — one per variant,
/// named after the variant's payload field — never an opaque `--body` blob.
#[test]
fn cli_lowers_oneof_body_to_mutually_exclusive_flags() {
    let api = api();
    let files = redwood::backends::cli::CliBackend {
        config: Default::default(),
    }
    .generate(&api)
    .expect("cli generates");
    let cmd = &files["cmd_agent_variations.go"];
    let add = cmd
        .split("\"add-assignment\"")
        .nth(1)
        .expect("add-assignment command emitted");
    let add = add.split("\"remove-assignment\"").next().unwrap();
    // One typed flag per arm, plus the tag flag for the union itself.
    for flag in ["type", "tool-id", "tool-set-id", "sub-agent-id"] {
        assert!(
            add.contains(&format!("&cli.StringFlag{{Name: \"{flag}\"")),
            "typed --{flag} flag"
        );
    }
    assert!(
        !add.contains("Name: \"body\""),
        "no opaque --body flag: {add}"
    );
    assert!(
        add.contains("commands.ConvertAgentVariationsAddAssignment(cmd, &converted)"),
        "command delegates conversion: {add}"
    );
    let conversion = &files["internal/commands/agent_variations_add_assignment_conv.go"];
    // The arm is inferred from the flag used; conflicts and a missing arm
    // are usage errors raised by the generated operation converter.
    assert!(conversion.contains("_body.resolveUnion(unionSpec{Flag: \"type\", Path: []string{}, Discriminator: \"type\", Required: true, Inferable: true"), "{conversion}");
    assert!(
        conversion.contains("{Tag: \"toolSetId\", Keys: []string{\"toolSetId\"}"),
        "{conversion}"
    );
    assert!(
        conversion.contains("_body.set(\"tool-set-id\", []string{\"toolSetId\"}, _v)"),
        "{conversion}"
    );
    assert!(
        conversion.contains("cmd.String(\"tool-set-id\")"),
        "converter reads urfave directly: {conversion}"
    );
    assert!(
        conversion.contains("values[\"body\"] = _body.body"),
        "{conversion}"
    );

    let api_md = &files["api.md"];
    let usage = api_md
        .lines()
        .find(|l| l.contains(" add-assignment "))
        .expect("add-assignment usage line");
    assert!(
        usage.contains("--type <tool-id|tool-set-id|sub-agent-id> [--tool-id <value>] [--tool-set-id <value>] [--sub-agent-id <value>] [-f <doc>] [--dry-run]"),
        "{usage}"
    );
    assert!(!usage.contains("--body"), "{usage}");
}

/// Display columns may reach THROUGH a discriminated union when the path
/// resolves on every arm (the discriminator tag always does), and a column
/// may opt into table-mode truncation — extended output stays complete.
#[test]
fn cli_display_columns_resolve_through_unions_and_truncate() {
    let api = api();
    let config: redwood::config::CliConfig = toml::from_str(
        r#"
[display]
columns = [{ header = "ID", path = ".metadata.id" }]

[display.methods."get /v1/workspaces/{workspaceId}/objectives/{objectiveId}/events"]
columns = [
  { header = "TYPE", path = ".data.type" },
  { header = "DATA", path = ".data", truncate = 30 },
]
"#,
    )
    .expect("config parses");
    let files = redwood::backends::cli::CliBackend { config }
        .generate(&api)
        .expect("cli generates");
    let cmd = &files["cmd_objectives.go"];
    let list_events = cmd
        .split("\"list-events\"")
        .nth(1)
        .and_then(|s| s.split("_columns := ").nth(1))
        .and_then(|s| s.lines().next())
        .expect("list-events columns");
    assert!(
        list_events.contains("{header: \"TYPE\", path: []string{\"data\", \"type\"}}"),
        "{list_events}"
    );
    assert!(
        list_events.contains("{header: \"DATA\", path: []string{\"data\"}, truncate: 30}"),
        "{list_events}"
    );
    let helpers = &files["helpers.go"];
    assert!(
        helpers.contains("truncate int"),
        "column carries a truncate width"
    );
}

/// Client params ([api] client_params) get a persistent-defaults surface:
/// `config set/get/unset/list` stores per-profile defaults consulted by
/// every command below the flag and the environment, above login inference.
#[test]
fn cli_generates_config_defaults_for_client_params() {
    let mut api = api();
    let cfg: redwood::config::GeneratorConfig =
        toml::from_str(include_str!("../redwood.toml")).expect("config parses");
    redwood::config::apply(&mut api, &cfg).expect("config applies");
    let files = redwood::backends::cli::CliBackend {
        config: cfg.lang.cli,
    }
    .generate(&api)
    .expect("cli generates");
    let config_go = files.get("config.go").expect("config.go emitted");
    assert!(config_go.contains("\"workspace-id\""), "kebab key present");
    assert!(
        config_go.contains("\"workspaceId\""),
        "wire spelling accepted"
    );
    assert!(
        config_go.contains("CADENYA_WORKSPACE_ID"),
        "env named in sources"
    );
    let main_go = &files["main.go"];
    assert!(
        main_go.contains("configCommand(),"),
        "config command registered"
    );
    // newClient consults the stored default only when neither the flag nor
    // the environment supplied one (flag > env > stored default).
    assert!(
        main_go.contains("if !cmd.Root().IsSet(\"workspace-id\") && os.Getenv(\"CADENYA_WORKSPACE_ID\") == \"\" {\n\t\tif v := storedConfigValue(cmd, \"workspace-id\"); v != \"\" {"),
        "{main_go}"
    );
    // Login-derived single-workspace inference yields to the stored default.
    assert!(
        main_go.contains("storedConfigValue(cmd, \"workspace-id\") == \"\""),
        "login inference must not outrank a stored default: {main_go}"
    );
}

/// [lang.cli.aliases]: a top-level spelling for a nested command
/// (`whoami` -> `profiles whoami`), validated against the IR at
/// generation time so a dangling alias is a build error, not a runtime 404.
#[test]
fn cli_top_level_aliases_are_generated_and_validated() {
    let api = || {
        let mut api = api();
        let cfg: redwood::config::GeneratorConfig =
            toml::from_str(include_str!("../redwood.toml")).expect("config parses");
        redwood::config::apply(&mut api, &cfg).expect("config applies");
        api
    };
    let config = |aliases: &str| -> redwood::config::CliConfig {
        toml::from_str(&format!("[aliases]\n{aliases}")).expect("cli config parses")
    };
    let files = redwood::backends::cli::CliBackend {
        config: config("whoami = \"profiles whoami\""),
    }
    .generate(&api())
    .expect("cli generates");
    let main_go = &files["main.go"];
    assert!(
        main_go.contains("addAlias(root, \"whoami\", \"profiles\", \"whoami\")"),
        "{main_go}"
    );
    assert!(
        files["helpers.go"].contains("func addAlias"),
        "alias helper emitted"
    );

    // Dangling target: generation error naming the alias.
    let err = redwood::backends::cli::CliBackend {
        config: config("who = \"profiles nope\""),
    }
    .generate(&api())
    .expect_err("dangling alias must fail generation");
    assert!(err.to_string().contains("who"), "{err}");

    // Colliding with an existing top-level command: generation error.
    let err = redwood::backends::cli::CliBackend {
        config: config("profiles = \"profiles whoami\""),
    }
    .generate(&api())
    .expect_err("alias colliding with a resource must fail generation");
    assert!(err.to_string().contains("profiles"), "{err}");
}

// ---- body plans over the real spec ------------------------------------------

fn plan_for(api: &ir::Api, id: &str) -> redwood::ir::plan::BodyPlan {
    use redwood::ir::plan::{body_plan, PlanOptions};
    let op = api
        .resources
        .iter()
        .flat_map(|r| r.operations.iter())
        .find(|o| o.id == id)
        .unwrap_or_else(|| panic!("{id}"));
    let opts = PlanOptions {
        reserved: op
            .path_params
            .iter()
            .chain(op.query_params.iter())
            .map(|p| heck::ToKebabCase::to_kebab_case(p.wire_name.as_str()))
            .collect(),
        ..PlanOptions::default()
    };
    body_plan(api, op, &opts)
        .expect("plans")
        .expect("has a body")
}

#[test]
fn every_body_plans_without_collisions() {
    use redwood::ir::plan::{body_plan, PlanOptions};
    let api = api();
    let mut planned = 0;
    for op in api.resources.iter().flat_map(|r| r.operations.iter()) {
        let opts = PlanOptions {
            reserved: op
                .path_params
                .iter()
                .chain(op.query_params.iter())
                .map(|p| heck::ToKebabCase::to_kebab_case(p.wire_name.as_str()))
                .collect(),
            ..PlanOptions::default()
        };
        if body_plan(&api, op, &opts)
            .unwrap_or_else(|e| panic!("{}: {e}", op.id))
            .is_some()
        {
            planned += 1;
        }
    }
    assert!(
        planned >= 40,
        "expected every create/update/action body to plan, got {planned}"
    );
}

#[test]
fn tool_set_create_flattens_to_the_documented_inventory() {
    use redwood::ir::plan::InputKind;
    let api = api();
    let plan = plan_for(&api, "ToolService_CreateToolSet");
    let flags: Vec<&str> = plan.inputs.iter().map(|i| i.flag.as_str()).collect();
    assert_eq!(
        flags,
        vec![
            "metadata",
            "name",
            "external-id",
            "label",
            "spec",
            "description",
            "adapter",
            "mcp",
            "mcp-url",
            "mcp-header",
            "mcp-include-tools",
            "mcp-include-tools-filter",
            "mcp-include-tools-operator",
            "mcp-exclude-tools",
            "mcp-exclude-tools-filter",
            "mcp-exclude-tools-operator",
            "mcp-tool-approvals",
            "mcp-tool-approvals-always",
            "mcp-tool-approvals-only",
            "mcp-tool-approvals-only-filter",
            "mcp-tool-approvals-only-operator",
            "mcp-just-in-time",
            "mcp-just-in-time-enabled",
            "mcp-just-in-time-fail-objective-on-tool-list-error",
            "http",
            "http-base-url",
            "http-header",
            "openapi",
            "openapi-url",
            "openapi-header",
            "openapi-include-tools",
            "openapi-include-tools-filter",
            "openapi-include-tools-operator",
            "openapi-exclude-tools",
            "openapi-exclude-tools-filter",
            "openapi-exclude-tools-operator",
            "openapi-tool-approvals",
            "openapi-tool-approvals-always",
            "openapi-tool-approvals-only",
            "openapi-tool-approvals-only-filter",
            "openapi-tool-approvals-only-operator",
            "openapi-base-url",
            "openapi-server-name",
            "openapi-upload-id",
            "bare",
            "bare-content-timeout",
            "overlay",
        ]
    );
    let by = |f: &str| plan.inputs.iter().find(|i| i.flag == f).unwrap();
    assert!(matches!(
        by("mcp-header").kind,
        InputKind::KvMap(ir::Ty::String)
    ));
    assert!(matches!(by("overlay").kind, InputKind::DocList(_)));
    assert!(matches!(
        by("mcp-include-tools-filter").kind,
        InputKind::DocList(_)
    ));
    assert!(matches!(by("adapter").kind, InputKind::UnionTag));
    assert!(by("adapter").required);
    assert!(
        matches!(by("openapi").kind, InputKind::UnionTag),
        "nested union keeps its segment"
    );
    let adapter = &plan.unions[by("adapter").union.unwrap()];
    assert!(adapter.inferable);
    let tags: Vec<&str> = adapter.arms.iter().map(|a| a.tag.as_str()).collect();
    assert_eq!(tags, vec!["mcp", "http", "openapi", "bare"]);
    assert_eq!(by("mcp-url").category, "adapter = mcp");
    assert_eq!(by("openapi-url").category, "openapi = url");
    let operator = by("mcp-include-tools-operator");
    assert_eq!(
        operator.enum_short.as_ref().unwrap(),
        &vec![
            ("and".to_string(), "OPERATOR_AND".to_string()),
            ("or".to_string(), "OPERATOR_OR".to_string())
        ]
    );
}

#[test]
fn provider_key_create_collapses_both_unions() {
    let api = api();
    let plan = plan_for(&api, "AIProviderKeyService_CreateAIProviderKey");
    let flags: Vec<&str> = plan.inputs.iter().map(|i| i.flag.as_str()).collect();
    for expected in [
        "name",
        "provider",
        "credentials",
        "api-key",
        "header",
        "config",
        "openrouter-region",
        "openai-organization-id",
        "openai-project-id",
        "openai-compatible-base-url",
    ] {
        assert!(
            flags.contains(&expected),
            "missing --{expected} in {flags:?}"
        );
    }
    assert!(!flags.contains(&"credentials-api-key-api-key"));
    let provider = plan.inputs.iter().find(|i| i.flag == "provider").unwrap();
    let shorts: Vec<&str> = provider
        .enum_short
        .as_ref()
        .unwrap()
        .iter()
        .map(|(s, _)| s.as_str())
        .collect();
    assert!(
        shorts.contains(&"anthropic") && shorts.contains(&"openai-compatible"),
        "{shorts:?}"
    );
}

#[test]
fn objective_create_uses_pair_and_shorthand_lists() {
    use redwood::ir::plan::InputKind;
    let api = api();
    let plan = plan_for(&api, "ObjectiveService_CreateObjective");
    let by = |f: &str| plan.inputs.iter().find(|i| i.flag == f).unwrap();
    assert!(matches!(by("secret").kind, InputKind::ShorthandList { .. }));
    assert!(by("secret").pair_form.is_some());
    assert!(matches!(
        by("memory-cascade").kind,
        InputKind::ShorthandList { .. }
    ));
    assert!(matches!(
        by("system-prompt-data").kind,
        InputKind::EntryDoc(_)
    ));
    assert!(matches!(by("pinned-parameter").kind, InputKind::KvMap(_)));
}

#[test]
fn update_bodies_carry_a_derivable_mask() {
    let api = api();
    let update = api
        .resources
        .iter()
        .flat_map(|r| r.operations.iter())
        .find(|o| o.id == "ToolService_UpdateToolSet")
        .unwrap();
    assert_eq!(update.update_mask.as_deref(), Some("updateMask"));

    let files = redwood::backends::cli::CliBackend {
        config: Default::default(),
    }
    .generate(&api)
    .expect("cli generates");
    let command = &files["internal/commands/tool_sets_update_conv.go"];
    let derive = command
        .find("_body.updateMask(\"updateMask\")")
        .expect("mask is derived");
    let finish = command.find("_body.finish(").expect("body is validated");
    assert!(
        derive < finish,
        "a required field mask must be derived before required-field validation"
    );
    assert!(
        command[finish..].contains(", _strict)"),
        "final validation must enforce --strict after unions resolve"
    );
}
