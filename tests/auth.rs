//! Authentication lowering and backend projections that are not exercised by
//! the repository's Bearer-authenticated primary fixture.

use redwood::backends::{
    cli::CliBackend, golang::GoBackend, openapi_export::OpenApiBackend, python::PythonBackend,
    ruby::RubyBackend, typescript::TypeScriptBackend, Backend,
};
use redwood::config::{CliConfig, GoConfig, PythonConfig, RubyConfig, TypeScriptConfig};

const BASIC_SPEC: &str = include_str!("fixtures/basic-auth.yml");

fn basic_api() -> redwood::ir::Api {
    let spec = redwood::openapi::parse(BASIC_SPEC).expect("Basic Auth spec parses");
    redwood::ir::lower::lower(&spec).expect("Basic Auth spec lowers")
}

#[test]
fn basic_auth_is_projected_into_every_client() {
    let api = basic_api();

    let ts = TypeScriptBackend {
        config: TypeScriptConfig::default(),
    }
    .generate(&api)
    .expect("TypeScript generates");
    let ts_client = &ts["src/client.ts"];
    assert!(ts_client.contains("username?: string;"), "{ts_client}");
    assert!(ts_client.contains("password?: string;"), "{ts_client}");
    assert!(
        ts_client.contains("Authorization: basicAuthHeader(username, password)"),
        "{ts_client}"
    );
    assert!(ts_client.contains("new TextEncoder()"), "{ts_client}");
    assert!(!ts_client.contains("apiKey?: string;"), "{ts_client}");

    let go = GoBackend {
        config: GoConfig::default(),
    }
    .generate(&api)
    .expect("Go generates");
    let go_client = &go["client.go"];
    assert!(
        go_client.contains("func WithBasicAuth(username, password string) Option"),
        "{go_client}"
    );
    assert!(
        go_client.contains("base64.StdEncoding.EncodeToString"),
        "{go_client}"
    );
    assert!(go["behavior_test.go"].contains("Basic dGVzdC11c2VyOnRlc3QtcGFzc3dvcmQ="));

    let python = PythonBackend {
        config: PythonConfig::default(),
    }
    .generate(&api)
    .expect("Python generates");
    let py_client = &python["passwords/_client.py"];
    assert!(
        py_client.contains("username: Optional[str] = None"),
        "{py_client}"
    );
    assert!(py_client.contains("base64.b64encode"), "{py_client}");
    assert!(!py_client.contains("api_key: Optional[str]"), "{py_client}");

    let ruby = RubyBackend {
        config: RubyConfig::default(),
    }
    .generate(&api)
    .expect("Ruby generates");
    let rb_client = &ruby["lib/passwords/client.rb"];
    assert!(
        rb_client.contains("def initialize(username: nil, password: nil"),
        "{rb_client}"
    );
    assert!(rb_client.contains("Base64.strict_encode64"), "{rb_client}");
    assert!(ruby["spec/behavior_spec.rb"].contains("Basic dGVzdC11c2VyOnRlc3QtcGFzc3dvcmQ="));

    let cli = CliBackend {
        config: CliConfig::default(),
    }
    .generate(&api)
    .expect("CLI generates");
    let main = &cli["main.go"];
    assert!(main.contains("Name: \"username\""), "{main}");
    assert!(main.contains("Name: \"password\""), "{main}");
    assert!(
        main.contains("sdk.WithBasicAuth(username, password)"),
        "{main}"
    );
    assert!(!main.contains("Name: \"api-key\""), "{main}");
}

#[test]
fn basic_auth_curl_sample_uses_curl_native_credentials() {
    let api = basic_api();
    let files = OpenApiBackend {
        spec_source: BASIC_SPEC.to_string(),
        ts_config: TypeScriptConfig::default(),
        go_config: GoConfig::default(),
        py_config: PythonConfig::default(),
        rb_config: RubyConfig::default(),
        cli_config: CliConfig::default(),
    }
    .generate(&api)
    .expect("OpenAPI export generates");
    let exported = &files["openapi.yml"];
    assert!(exported.contains("--basic"), "{exported}");
    assert!(
        exported.contains("--user \"${PASSWORDS_USERNAME}:${PASSWORDS_PASSWORD}\""),
        "{exported}"
    );
}

#[test]
fn basic_auth_environment_names_can_be_overridden() {
    let mut api = basic_api();
    let config: redwood::config::GeneratorConfig = toml::from_str(
        r#"
[api]
basic_username_env_var = "SERVICE_USER"
basic_password_env_var = "SERVICE_PASS"
"#,
    )
    .expect("config parses");
    redwood::config::apply(&mut api, &config).expect("config applies");
    assert_eq!(api.basic_username_env, "SERVICE_USER");
    assert_eq!(api.basic_password_env, "SERVICE_PASS");
}

#[test]
fn device_login_is_rejected_for_basic_auth() {
    let api = basic_api();
    let config: CliConfig = toml::from_str(
        r#"
[auth]
device_authorization_endpoint = "https://auth.example.test/device"
token_endpoint = "https://auth.example.test/token"
client_id = "passwords-cli"
"#,
    )
    .expect("CLI config parses");
    let err = CliBackend { config }
        .generate(&api)
        .expect_err("single-token device login is incompatible with Basic Auth");
    assert!(err
        .to_string()
        .contains("not compatible with HTTP Basic Auth"));
}
