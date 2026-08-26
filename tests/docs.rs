//! Per-language docs: every target ships a native README + api.md rendered
//! through its own formatters — no cross-language syntax, no api.md for the
//! manifest target.

use redwood::backends::Backend;

fn lowered() -> redwood::ir::Api {
    let source = include_str!("../api-spec.yml");
    let spec = redwood::openapi::parse(source).expect("spec parses");
    let mut api = redwood::ir::lower::lower(&spec).expect("lowers");
    let cfg: redwood::config::GeneratorConfig =
        toml::from_str(include_str!("../redwood.toml")).expect("config parses");
    redwood::config::apply(&mut api, &cfg).expect("config applies");
    api
}

fn docs_for(backend: &dyn Backend) -> (String, String) {
    let files = backend.generate(&lowered()).expect("generates");
    let api_md = files.get("api.md").expect("api.md emitted").clone();
    let readme = files.get("README.md").expect("README.md emitted").clone();
    assert_enum_examples_valid(&api_md);
    assert_enum_examples_valid(&readme);
    (api_md, readme)
}

/// Every enum-shaped literal quoted in the docs must be a member of a
/// generated enum — a hand-written sample once shipped a value the enum
/// does not contain, and users copy getting-started examples verbatim.
fn assert_enum_examples_valid(doc: &str) {
    let api = lowered();
    let known: std::collections::HashSet<&str> = api
        .types
        .values()
        .filter_map(|d| match &d.shape {
            redwood::ir::Shape::Enum(e) => Some(e.values.iter().map(String::as_str)),
            _ => None,
        })
        .flatten()
        .collect();
    let env_prefix = format!("{}_", api.name.to_uppercase());
    for segment in doc.split(['"', '\'']).skip(1).step_by(2) {
        let enumish = segment.len() > 8
            && segment.contains('_')
            && !segment.starts_with(&env_prefix) // env vars, not enum values
            && segment
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
        if enumish {
            assert!(
                known.contains(segment),
                "doc example uses '{segment}', which is not a value of any generated enum"
            );
        }
    }
}

#[test]
fn go_docs_are_native() {
    let backend = redwood::backends::golang::GoBackend {
        config: redwood::config::GoConfig::default(),
    };
    let (api_md, readme) = docs_for(&backend);
    // Nested accessor + real Go signature with ctx and error return.
    assert!(
        api_md.contains("client.Agents().Variations()."),
        "{}",
        &api_md[..500]
    );
    assert!(api_md.contains("ctx context.Context"));
    assert!(api_md.contains("(*Stream[ObjectiveEvent], error)"));
    // No TypeScript syntax.
    assert!(!api_md.contains("{ ...params }"));
    assert!(!api_md.contains("Promise<"));
    assert!(readme.contains("go get"));
    assert!(readme.contains("WithLastEventID"));
}

#[test]
fn typescript_docs_are_native() {
    let backend = redwood::backends::typescript::TypeScriptBackend {
        config: redwood::config::TypeScriptConfig::default(),
    };
    let (api_md, readme) = docs_for(&backend);
    assert!(api_md.contains("client.agents.variations."));
    assert!(api_md.contains("Promise<Page<Objective>>"));
    assert!(!api_md.contains("ctx context.Context"));
    assert!(readme.contains("npm install"));
    // The stream metadata-envelope distinction the live reviewer requested.
    assert!(readme.contains("stream.events()"));
}

#[test]
fn python_docs_are_native() {
    let backend = redwood::backends::python::PythonBackend {
        config: redwood::config::PythonConfig::default(),
    };
    let (api_md, readme) = docs_for(&backend);
    assert!(api_md.contains("client.agents.variations."));
    assert!(api_md.contains("workspace_id"));
    assert!(!api_md.contains("Promise<"));
    assert!(!api_md.contains("{ ...params }"));
    assert!(readme.contains("pip install"));
    // The nested-request example is sampler-generated from the IR (never a
    // hand-written literal): assert its shape, not spec-specific names.
    assert!(
        readme.contains(".create("),
        "sampled create example present"
    );
}

#[test]
fn ruby_docs_are_native() {
    let backend = redwood::backends::ruby::RubyBackend {
        config: redwood::config::RubyConfig::default(),
    };
    let (api_md, readme) = docs_for(&backend);
    assert!(api_md.contains("client.agents.variations."));
    assert!(api_md.contains("workspace_id:"));
    assert!(!api_md.contains("Promise<"));
    assert!(readme.contains("gem install"));
    assert!(readme.contains("Webhooks.verify"));
}

#[test]
fn cli_docs_are_command_grammar() {
    let backend = redwood::backends::cli::CliBackend {
        config: redwood::config::CliConfig::default(),
    };
    let (api_md, readme) = docs_for(&backend);
    assert!(api_md.contains("agents variations create"));
    assert!(api_md.contains("--workspace-id"));
    assert!(api_md.contains("--last-event-id"));
    // Grammar faithful to the parser: booleans take no space-separated
    // value, slice flags are marked repeatable (optional and required).
    assert!(api_md.contains("[--include-info[=true|false]]"));
    assert!(!api_md.contains("--include-info <value>"));
    assert!(api_md.contains("[--names <value>]..."));
    assert!(api_md.contains("--content <JSON>..."));
    // Commands, not method calls.
    assert!(!api_md.contains("client."));
    assert!(readme.contains("Exit codes"));
    assert!(readme.contains("--include-info=false"));
}

#[test]
fn manifest_ships_no_user_docs() {
    let backend = redwood::backends::manifest::ManifestBackend;
    let files = backend.generate(&lowered()).expect("generates");
    assert!(!files.contains_key("api.md"));
    assert!(!files.contains_key("README.md"));
}
