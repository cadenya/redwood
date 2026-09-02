use redwood::backends::{
    cli::CliBackend, golang::GoBackend, manifest::ManifestBackend, openapi_export::OpenApiBackend,
    python::PythonBackend, typescript::TypeScriptBackend, Backend,
};
use redwood::config::{
    CliConfig, GeneratorConfig, GoConfig, PythonConfig, RubyConfig, TypeScriptConfig,
};
use redwood::ir::{self, Shape, Ty};

const SPEC: &str = include_str!("fixtures/int64.yml");

fn api() -> ir::Api {
    let spec = redwood::openapi::parse(SPEC).expect("int64 fixture parses");
    ir::lower::lower(&spec).expect("int64 fixture lowers")
}

#[test]
fn int64_survives_lowering_and_uses_a_wide_conformance_sample() {
    let api = api();
    let resource_id = api.types.get("ResourceId").expect("ResourceId type");
    assert!(matches!(resource_id.shape, Shape::Alias(Ty::Int64)));

    let retrieve = &api.resources[0].operations[0];
    assert_eq!(retrieve.positionals[0].ty, Ty::Named("ResourceId".into()));
    assert!(matches!(retrieve.query_params[0].ty, Ty::Int64));
    assert!(matches!(retrieve.query_params[1].ty, Ty::Int32));

    let manifest: serde_json::Value = serde_json::from_str(
        &ManifestBackend::default()
            .generate(&api)
            .expect("manifest generates")["manifest.json"],
    )
    .expect("manifest is JSON");
    let op = &manifest["operations"][0];
    assert_eq!(op["positionals"][0]["sample"], 4_294_967_296_i64);
    assert_eq!(op["queryParams"][0]["sample"], 4_294_967_296_i64);
    assert_eq!(op["queryParams"][1]["sample"], 1);
}

#[test]
fn go_preserves_int64_models_params_and_numeric_path_ids() {
    let api = api();
    let files = GoBackend {
        config: GoConfig::default(),
    }
    .generate(&api)
    .expect("Go generates");

    assert!(files["types.go"].contains("type ResourceID = int64"));
    let resource = &files["resource_things.go"];
    assert!(resource.contains("thingID ResourceID"), "{resource}");
    assert!(resource.contains("RelatedID *int64"), "{resource}");
    assert!(resource.contains("SmallCount *int32"), "{resource}");
    assert!(resource.contains("fmt.Sprint(thingID)"), "{resource}");
    assert!(
        files["conformance/main.go"].contains("Retrieve(ctx, 4294967296"),
        "{}",
        files["conformance/main.go"]
    );
    assert!(files["testdata/golden/ThingsService_GetThing.json"].contains("/v1/things/4294967296"));
}

#[test]
fn cli_uses_width_specific_flags_accessors_and_positional_parsing() {
    let api = api();
    let files = CliBackend {
        config: CliConfig::default(),
    }
    .generate(&api)
    .expect("CLI generates");
    let command = &files["cmd_things.go"];

    assert!(command.contains("&cli.Int64Flag{Name: \"related-id\""));
    assert!(command.contains("cmd.Int64(\"related-id\")"));
    assert!(command.contains("&cli.Int32Flag{Name: \"small-count\""));
    assert!(command.contains("cmd.Int32(\"small-count\")"));
    assert!(command.contains("strconv.ParseInt(cmd.Args().Get(0), 10, 64)"));
    assert!(command.contains("<thing-id> must be a valid int64"));

    let update = command
        .split("Name:  \"update\"")
        .nth(1)
        .expect("update command");
    assert!(update.contains("&cli.Int64Flag{Name: \"owner-id\""));
    assert!(update.contains("cmd.Int64(\"owner-id\")"));
    assert!(update.contains("&cli.Int32Flag{Name: \"small-count\""));
    assert!(update.contains("cmd.Int32(\"small-count\")"));
}

#[test]
fn typed_sdk_docs_and_openapi_samples_use_numeric_int64_ids() {
    let api = api();
    let ts = TypeScriptBackend {
        config: TypeScriptConfig::default(),
    }
    .generate(&api)
    .expect("TypeScript generates");
    assert!(ts["api.md"].contains("retrieve(thingId: ResourceId"));
    assert!(ts["src/resources/things.ts"].contains("client.things.retrieve(4294967296"));
    assert!(ts["src/core/http.ts"].contains("value: QueryPrimitive | undefined"));

    let py = PythonBackend {
        config: PythonConfig::default(),
    }
    .generate(&api)
    .expect("Python generates");
    assert!(py["api.md"].contains("retrieve(thing_id: int"));
    assert!(py["widenumbers/resources/things.py"].contains("thing_id: int"));

    let exported = OpenApiBackend {
        spec_source: SPEC.to_string(),
        ts_config: TypeScriptConfig::default(),
        go_config: GoConfig::default(),
        py_config: PythonConfig::default(),
        rb_config: RubyConfig::default(),
        cli_config: CliConfig::default(),
    }
    .generate(&api)
    .expect("OpenAPI samples generate");
    let samples = &exported["openapi.yml"];
    assert!(samples.contains("retrieve(4294967296"), "{samples}");
    assert!(samples.contains("things retrieve 4294967296"), "{samples}");
    assert!(samples.contains("/v1/things/4294967296"), "{samples}");
}

#[test]
fn fixture_config_keeps_generated_go_and_cli_modules_adjacent() {
    let config: GeneratorConfig =
        toml::from_str(include_str!("fixtures/int64.toml")).expect("fixture config parses");
    assert_eq!(
        config.lang.go.module_path.as_deref(),
        Some("example.com/wide-numbers-go")
    );
    assert_eq!(config.lang.cli.sdk_replace.as_deref(), Some("../go"));
}
