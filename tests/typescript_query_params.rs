use redwood::backends::{typescript::TypeScriptBackend, Backend};

const DOTTED_QUERY_SPEC: &str = include_str!("../e2e/fixtures/dotted-query.yml");

#[test]
fn dotted_query_params_are_nested_and_forwarded() {
    let spec = redwood::openapi::parse(DOTTED_QUERY_SPEC).expect("fixture parses");
    let api = redwood::ir::lower::lower(&spec).expect("fixture lowers");
    let files = TypeScriptBackend {
        config: Default::default(),
    }
    .generate(&api)
    .expect("TypeScript generates");
    let resource = files
        .get("src/resources/report.ts")
        .expect("report resource emitted");

    assert!(
        resource.contains(
            "range?: {\n    start?: string;\n    end?: string;\n  };\n  filters?: {\n    resourceId?: string;\n    states?: Array<string>;\n  };"
        ),
        "dotted query leaves should form nested params objects:\n{resource}"
    );
    assert!(
        resource.contains(
            "query: { range: params?.range, filters: params?.filters, interval: params?.interval, groupBy: params?.groupBy }"
        ),
        "nested params objects should reach RequestSpec.query:\n{resource}"
    );
    assert!(!resource.contains("'range.start'?:"));
    assert!(!resource.contains("params?.['range.start']"));
}
