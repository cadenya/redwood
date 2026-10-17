//! Config-driven post-lowering transforms.

use redwood::config::GeneratorConfig;

fn sample_api() -> redwood::ir::Api {
    let doc = r#"
openapi: 3.1.0
info:
  title: Cadenya API
  version: '1.0'
paths:
  /v1/account:
    get:
      tags: [AccountService, Accounts]
      operationId: AccountService_GetAccount
      responses:
        '200':
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Account'
components:
  schemas:
    Account:
      type: object
      properties:
        info:
          type: string
"#;
    let spec = redwood::openapi::parse(doc).expect("parses");
    redwood::ir::lower::lower(&spec).expect("lowers")
}

#[test]
fn resource_rename_applies() {
    let cfg: GeneratorConfig = toml::from_str(
        r#"
[resources]
accounts = "account"
"#,
    )
    .expect("config parses");
    let mut api = sample_api();
    redwood::config::apply(&mut api, &cfg).expect("applies");
    assert_eq!(api.resources[0].name, "account");
}

#[test]
fn unknown_resource_rename_errors() {
    let cfg: GeneratorConfig = toml::from_str("[resources]\nnope = \"x\"").expect("parses");
    let mut api = sample_api();
    assert!(redwood::config::apply(&mut api, &cfg).is_err());
}

#[test]
fn method_override_applies() {
    let cfg: GeneratorConfig = toml::from_str(
        r#"
[methods]
"AccountService_GetAccount" = "whoami"
"#,
    )
    .expect("parses");
    let mut api = sample_api();
    redwood::config::apply(&mut api, &cfg).expect("applies");
    assert_eq!(api.resources[0].operations[0].name, "whoami");
}

#[test]
fn sse_skip_events_apply() {
    let cfg: GeneratorConfig = toml::from_str(
        r#"
[sse]
skip_events = ["ping", "open"]
"#,
    )
    .expect("parses");
    let mut api = sample_api();
    redwood::config::apply(&mut api, &cfg).expect("applies");
    assert_eq!(api.sse_skip_events, vec!["ping", "open"]);
}

#[test]
fn sse_skip_events_reject_duplicates_and_blanks() {
    let cfg: GeneratorConfig =
        toml::from_str("[sse]\nskip_events = [\"ping\", \"ping\"]").expect("parses");
    let mut api = sample_api();
    let err = redwood::config::apply(&mut api, &cfg).unwrap_err();
    assert!(err.to_string().contains("twice"), "{err}");

    let cfg: GeneratorConfig = toml::from_str("[sse]\nskip_events = [\" \"]").expect("parses");
    let mut api = sample_api();
    assert!(redwood::config::apply(&mut api, &cfg).is_err());
}

#[test]
fn method_override_accepts_path_key() {
    let cfg: GeneratorConfig = toml::from_str(
        r#"
[methods]
"GET /v1/account" = "whoami"
"#,
    )
    .expect("parses");
    let mut api = sample_api();
    redwood::config::apply(&mut api, &cfg).expect("applies");
    assert_eq!(api.resources[0].operations[0].name, "whoami");
}

#[test]
fn path_key_method_must_match() {
    // Right path, wrong verb: no operation matches, so the override is a
    // config error rather than a silent no-op.
    let cfg: GeneratorConfig = toml::from_str(
        r#"
[methods]
"POST /v1/account" = "whoami"
"#,
    )
    .expect("parses");
    let mut api = sample_api();
    let err = redwood::config::apply(&mut api, &cfg).unwrap_err();
    assert!(err.to_string().contains("matches no operation"), "{err}");
}

#[test]
fn dotted_resource_rename_nests_and_keeps_ident() {
    let cfg: GeneratorConfig = toml::from_str(
        r#"
[resources]
accounts = "workspaces.accounts"
"#,
    )
    .expect("parses");
    let mut api = sample_api();
    // Give the sample a second, top-level resource to nest under.
    api.resources.push(redwood::ir::Resource {
        name: "workspaces".into(),
        ident: "workspaces".into(),
        parent: None,
        description: None,
        operations: vec![],
    });
    redwood::config::apply(&mut api, &cfg).expect("applies");
    let nested = &api.resources[0];
    assert_eq!(nested.name, "accounts");
    assert_eq!(nested.parent.as_deref(), Some("workspaces"));
    assert_eq!(nested.ident, "accounts", "type identity is stable");
    assert_eq!(nested.path(), "workspaces.accounts");
}

#[test]
fn nesting_under_unknown_parent_errors() {
    let cfg: GeneratorConfig =
        toml::from_str("[resources]\naccounts = \"nope.accounts\"").expect("parses");
    let mut api = sample_api();
    let err = redwood::config::apply(&mut api, &cfg).unwrap_err();
    assert!(
        err.to_string().contains("not a top-level resource"),
        "{err}"
    );
}

#[test]
fn type_mapping_renames_declaration_and_references() {
    let cfg: GeneratorConfig = toml::from_str(
        r#"
[mapping]
"Account" = "CustomerAccount"
"#,
    )
    .expect("parses");
    let mut api = sample_api();
    redwood::config::apply(&mut api, &cfg).expect("applies");
    assert!(api.types.contains_key("CustomerAccount"));
    assert!(!api.types.contains_key("Account"));
    // The operation's response reference follows the rename.
    let op = &api.resources[0].operations[0];
    match &op.response {
        redwood::ir::ResponseKind::Json(redwood::ir::Ty::Named(n)) => {
            assert_eq!(n, "CustomerAccount")
        }
        other => panic!("unexpected response: {other:?}"),
    }
}

#[test]
fn type_mapping_to_existing_name_errors() {
    let cfg: GeneratorConfig =
        toml::from_str("[mapping]\n\"Account\" = \"Account\"").expect("parses");
    let mut api = sample_api();
    let err = redwood::config::apply(&mut api, &cfg).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
}

#[test]
fn client_params_get_env_vars_derived_from_api_name() {
    let cfg: GeneratorConfig = toml::from_str(
        r#"
[api]
client_params = ["workspaceId"]
"#,
    )
    .expect("parses");
    // Validation now requires a real matching path parameter, so use the
    // actual spec rather than the minimal hand-built fixture.
    let spec = redwood::openapi::parse(include_str!("../api-spec.yml")).expect("spec parses");
    let mut api = redwood::ir::lower::lower(&spec).expect("lowers");
    api.name = "Cadenya".to_string();
    redwood::config::apply(&mut api, &cfg).expect("applies");
    assert_eq!(api.client_params.len(), 1);
    assert_eq!(api.client_params[0].wire_name, "workspaceId");
    assert_eq!(api.client_params[0].env_var, "CADENYA_WORKSPACE_ID");
}

#[test]
fn client_params_reject_unknown_duplicate_and_reserved() {
    let source = include_str!("../api-spec.yml");
    let spec = redwood::openapi::parse(source).expect("spec parses");

    let cases = [
        (
            "client_params = [\"definitelyNotAPathParam\"]",
            "no operation PATH",
        ),
        (
            "client_params = [\"workspaceId\", \"workspaceId\"]",
            "duplicates",
        ),
        (
            "client_params = [\"workspaceId\", \"workspace_id\"]",
            "duplicates",
        ),
        ("client_params = [\"apiKey\"]", "built-in"),
        ("client_params = [\"username\"]", "built-in"),
        ("client_params = [\"password\"]", "built-in"),
    ];
    for (line, needle) in cases {
        let mut api = redwood::ir::lower::lower(&spec).expect("lowers");
        let cfg: redwood::config::GeneratorConfig =
            toml::from_str(&format!("[api]\n{line}\n")).expect("config parses");
        let err = redwood::config::apply(&mut api, &cfg).expect_err(line);
        assert!(
            err.to_string().contains(needle),
            "{line}: expected {needle:?} in {err}"
        );
    }

    // The valid entry still applies.
    let mut api = redwood::ir::lower::lower(&spec).expect("lowers");
    let cfg: redwood::config::GeneratorConfig =
        toml::from_str("[api]\nclient_params = [\"workspaceId\"]\n").expect("config parses");
    redwood::config::apply(&mut api, &cfg).expect("valid client param applies");
    assert_eq!(api.client_params.len(), 1);
}

#[test]
fn webhook_env_default_and_override() {
    let spec = redwood::openapi::parse(include_str!("../api-spec.yml")).expect("spec parses");
    // Default: Standard Webhooks "secret" naming, derived from the API name.
    let api = redwood::ir::lower::lower(&spec).expect("lowers");
    assert_eq!(api.webhook_env, "CADENYA_WEBHOOK_SECRET");
    // Config override reaches the IR (and therefore every backend).
    let mut api = redwood::ir::lower::lower(&spec).expect("lowers");
    let cfg: redwood::config::GeneratorConfig =
        toml::from_str("[api]\nwebhook_env_var = \"CADENYA_WEBHOOK_KEY\"\n").expect("parses");
    redwood::config::apply(&mut api, &cfg).expect("applies");
    assert_eq!(api.webhook_env, "CADENYA_WEBHOOK_KEY");
}

#[test]
fn positional_overrides_apply_in_order() {
    let spec = redwood::openapi::parse(include_str!("../api-spec.yml")).expect("spec parses");
    let mut api = redwood::ir::lower::lower(&spec).expect("lowers");
    let cfg: redwood::config::GeneratorConfig = toml::from_str(
        "[api]\nclient_params = [\"workspaceId\"]\n\n[positional]\n\"ObjectiveService_GetObjectiveToolCall\" = [\"objectiveId\", \"toolCallId\"]\n\"AIProviderKeyService_ListAIProviderKeys\" = []\n",
    )
    .expect("parses");
    redwood::config::apply(&mut api, &cfg).expect("applies");
    let op = api
        .resources
        .iter()
        .flat_map(|r| r.operations.iter())
        .find(|o| o.id == "ObjectiveService_GetObjectiveToolCall")
        .expect("op exists");
    let names: Vec<&str> = op
        .positionals
        .iter()
        .map(|p| p.wire_name.as_str())
        .collect();
    assert_eq!(names, ["objectiveId", "toolCallId"]);
    let list = api
        .resources
        .iter()
        .flat_map(|r| r.operations.iter())
        .find(|o| o.id == "AIProviderKeyService_ListAIProviderKeys")
        .expect("op exists");
    assert!(list.positionals.is_empty());

    // Unknown param name fails.
    let mut api = redwood::ir::lower::lower(&spec).expect("lowers");
    let cfg: redwood::config::GeneratorConfig =
        toml::from_str("[positional]\n\"ObjectiveService_GetObjectiveToolCall\" = [\"nope\"]\n")
            .expect("parses");
    let err = redwood::config::apply(&mut api, &cfg).expect_err("unknown rejected");
    assert!(format!("{err:#}").contains("not a path"), "{err:#}");
}
