use redwood::backends::{typescript::TypeScriptBackend, Backend};

const SPEC: &str = include_str!("../e2e/fixtures/union-body.yml");

fn api() -> redwood::ir::Api {
    let spec = redwood::openapi::parse(SPEC).unwrap();
    let mut api = redwood::ir::lower::lower(&spec).unwrap();
    let cfg = toml::from_str(include_str!("../e2e/fixtures/union-body.toml")).unwrap();
    redwood::config::apply(&mut api, &cfg).unwrap();
    api
}

#[test]
fn union_params_and_examples_are_direct_for_every_operation() {
    let files = TypeScriptBackend {
        config: Default::default(),
    }
    .generate(&api())
    .unwrap();
    let assignment = &files["src/resources/assignment.ts"];
    for method in ["Add", "Remove"] {
        assert!(
            assignment.contains(&format!(
                "export type Assignment{method}Params = AssignmentRequest & {{"
            )),
            "{assignment}"
        );
    }
    assert!(assignment.contains("workspaceId?: string;"));
    assert!(!assignment.contains("body: AssignmentRequest"));
    assert!(!assignment.contains("{ body:"));
    assert!(assignment.contains("type: 'toolId'"));
    assert!(files["src/resources/submission.ts"]
        .contains("export type SubmissionSubmitParams = AssignmentRequest;"));
    assert!(files["src/resources/batch.ts"].contains("body: Array<string>;"));
}

#[test]
fn openapi_typescript_samples_use_direct_union_bodies() {
    let backend = redwood::backends::openapi_export::OpenApiBackend {
        spec_source: SPEC.into(),
        ts_config: Default::default(),
        go_config: Default::default(),
        py_config: Default::default(),
        rb_config: Default::default(),
        cli_config: Default::default(),
    };
    let files = backend.generate(&api()).unwrap();
    let doc: serde_yaml::Value = serde_yaml::from_str(files.values().next().unwrap()).unwrap();
    for method in ["post", "delete"] {
        let samples = doc["paths"]["/v1/workspaces/{workspaceId}/assignments"][method]
            ["x-codeSamples"]
            .as_sequence()
            .unwrap();
        let sample = samples
            .iter()
            .find(|s| s["lang"].as_str() == Some("typescript"))
            .unwrap()["source"]
            .as_str()
            .unwrap();
        assert!(sample.contains("\"type\": \"toolId\""), "{sample}");
        assert!(!sample.contains("body:"), "{sample}");
    }
}

#[test]
fn cadenya_assignment_uses_input_union_with_workspace_intersection() {
    let spec = redwood::openapi::parse(include_str!("../api-spec.yml")).unwrap();
    let mut api = redwood::ir::lower::lower(&spec).unwrap();
    let cfg = toml::from_str(include_str!("../redwood.toml")).unwrap();
    redwood::config::apply(&mut api, &cfg).unwrap();
    let files = TypeScriptBackend {
        config: Default::default(),
    }
    .generate(&api)
    .unwrap();
    let resource = &files["src/resources/agent-variations.ts"];
    assert!(resource.contains("export type AgentVariationAddAssignmentParams = AddAgentVariationAssignmentRequestParam & {"));
    assert!(resource.contains("body: wireAddAgentVariationAssignmentRequest(_body)"));
}
