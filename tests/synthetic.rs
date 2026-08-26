//! Schema-abstraction acceptance gate: generate every target from a
//! deliberately unrelated synthetic spec and assert no vocabulary from the
//! primary fixture leaks into the output. Backends must be schema-generic;
//! the only identifiers in generated artifacts come from the input spec or
//! explicit target configuration.

use redwood::backends::Backend;

const SYNTHETIC_SPEC: &str = r#"
openapi: 3.1.0
info:
  title: AcmeInventory API
  version: '2.3'
servers:
  - url: https://api.acme-inventory.example
paths:
  /v1/depots:
    get:
      tags: [DepotService]
      summary: List depots
      operationId: DepotService_ListDepots
      parameters:
        - name: limit
          in: query
          schema: { type: integer, format: int32 }
        - name: labels
          in: query
          schema: { type: array, items: { type: string } }
        - name: verbose
          in: query
          schema: { type: boolean }
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ListDepotsResponse'
    post:
      tags: [DepotService]
      summary: Create a depot
      operationId: DepotService_CreateDepot
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateDepotRequest'
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Depot'
  /v1/health:
    get:
      tags: [HealthService]
      summary: Health check
      operationId: HealthService_GetHealth
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Health'
  /v1/depots/{id}:
    get:
      tags: [DepotService]
      summary: Get a depot
      operationId: DepotService_GetDepot
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Depot'
    delete:
      tags: [DepotService]
      summary: Delete a depot
      operationId: DepotService_DeleteDepot
      parameters:
        - name: id
          in: path
          required: true
          schema: { type: string }
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema: { type: object }
components:
  schemas:
    Health:
      type: object
      properties:
        status: { type: string }
    Depot:
      type: object
      properties:
        id: { type: string }
        region:
          $ref: '#/components/schemas/DepotRegion'
    DepotRegion:
      type: string
      enum: [REGION_UNSPECIFIED, REGION_NORTH, REGION_SOUTH]
    CreateDepotRequest:
      type: object
      required: [displayName]
      properties:
        displayName: { type: string }
        region:
          $ref: '#/components/schemas/DepotRegion'
    ListDepotsResponse:
      type: object
      properties:
        items:
          type: array
          items:
            $ref: '#/components/schemas/Depot'
"#;

/// Vocabulary from the primary fixture that must never appear in output
/// generated from an unrelated spec (unless supplied via configuration —
/// these tests use default configs, which carry none of it).
const FORBIDDEN: &[&str] = &[
    "cadenya",
    "agent",
    "objective",
    "workspace",
    "tool-set",
    "toolset",
    "openai",
    "openrouter",
];

#[test]
fn backends_are_schema_agnostic() {
    let spec = redwood::openapi::parse(SYNTHETIC_SPEC).expect("synthetic spec parses");
    let api = redwood::ir::lower::lower(&spec).expect("synthetic spec lowers");

    let backends: Vec<Box<dyn Backend>> = vec![
        Box::new(redwood::backends::typescript::TypeScriptBackend {
            config: Default::default(),
        }),
        Box::new(redwood::backends::golang::GoBackend {
            config: Default::default(),
        }),
        Box::new(redwood::backends::python::PythonBackend {
            config: Default::default(),
        }),
        Box::new(redwood::backends::ruby::RubyBackend {
            config: Default::default(),
        }),
        Box::new(redwood::backends::cli::CliBackend {
            config: Default::default(),
        }),
        Box::new(redwood::backends::docs::DocsBackend),
        Box::new(redwood::backends::manifest::ManifestBackend),
    ];

    for backend in &backends {
        let files = backend
            .generate(&api)
            .unwrap_or_else(|e| panic!("{} generates from synthetic spec: {e}", backend.name()));
        assert!(!files.is_empty(), "{} emitted files", backend.name());
        // The synthetic spec has NO security scheme: nothing generated may
        // demand or even mention an API key (a required-but-unused
        // credential makes every public-API SDK unusable), and no README
        // may advertise webhooks/streaming/phantom resources the IR lacks.
        for (path, contents) in &files {
            for needle in [
                "Missing API key",
                "missing API key",
                "WithAPIKey",
                "api-key",
                "someResource",
                "some_resource",
                "SomeResource",
                "Webhooks",
            ] {
                assert!(
                    !contents.contains(needle),
                    "{}:{path} mentions {needle:?} for a no-auth, no-webhook spec",
                    backend.name()
                );
            }
        }
        if backend.name() == "typescript" {
            // A schema named like a resource ("Health" + /v1/health) is
            // ordinary OpenAPI naming: the resource class must yield so
            // the package compiles and the model keeps the natural name.
            let resource = files
                .get("src/resources/health.ts")
                .expect("health resource emitted");
            assert!(
                resource.contains("class HealthResource"),
                "resource class must be collision-suffixed, got:\n{}",
                &resource[..resource.len().min(400)]
            );
        }
        for (path, contents) in &files {
            // The User-Agent header is generic HTTP vocabulary, not fixture
            // vocabulary — mask it so "agent" doesn't false-positive.
            let lower = contents
                .to_lowercase()
                .replace("user-agent", "")
                .replace("user_agent", "")
                .replace("useragent", "");
            for word in FORBIDDEN {
                assert!(
                    !lower.contains(word),
                    "{}:{path} leaks fixture vocabulary {word:?} into output \
                     generated from an unrelated spec",
                    backend.name()
                );
            }
        }
    }
}
