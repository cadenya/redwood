//! Red-green tests for OpenAPI -> IR lowering, written before the
//! implementation. Each test isolates one edge case the generator must
//! survive to produce production-grade SDKs.

use redwood::ir::{self, Auth, HttpMethod, ResponseKind, Shape, Ty};

/// Wrap schema/path fragments in a minimal valid spec and lower it.
fn lower(paths: &str, schemas: &str) -> ir::Api {
    let doc = format!(
        r#"
openapi: 3.1.0
info:
  title: Cadenya API
  version: '1.0'
servers:
  - url: https://api.cadenya.com
paths:
{paths}
components:
  schemas:
{schemas}
  securitySchemes:
    bearerAuth:
      type: http
      scheme: bearer
security:
  - bearerAuth: []
"#
    );
    let spec = redwood::openapi::parse(&doc).expect("spec parses");
    ir::lower::lower(&spec).expect("spec lowers")
}

fn decl<'a>(api: &'a ir::Api, name: &str) -> &'a ir::TypeDecl {
    api.types.get(name).unwrap_or_else(|| {
        panic!(
            "type {name} exists, have: {:?}",
            api.types.keys().collect::<Vec<_>>()
        )
    })
}

const NO_PATHS: &str = "  {}";

#[test]
fn discriminated_oneof_becomes_tagged_union() {
    let api = lower(
        NO_PATHS,
        r#"
    AIProviderConfig:
      oneOf:
        - $ref: '#/components/schemas/AIProviderConfig_Openai'
        - $ref: '#/components/schemas/AIProviderConfig_Openrouter'
      discriminator:
        propertyName: type
        mapping:
          openai: '#/components/schemas/AIProviderConfig_Openai'
          openrouter: '#/components/schemas/AIProviderConfig_Openrouter'
    AIProviderConfig_Openai:
      type: object
      properties:
        orgId:
          type: string
    AIProviderConfig_Openrouter:
      type: object
      properties:
        route:
          type: string
"#,
    );
    let Shape::Union(u) = &decl(&api, "AIProviderConfig").shape else {
        panic!("expected union");
    };
    let disc = u.discriminator.as_ref().expect("discriminated");
    assert_eq!(disc.property, "type");
    let tags: Vec<_> = u.variants.iter().map(|v| v.tag.as_deref()).collect();
    assert_eq!(tags, vec![Some("openai"), Some("openrouter")]);
    assert!(matches!(&u.variants[0].ty, Ty::Named(n) if n == "AIProviderConfig_Openai"));
}

#[test]
fn undiscriminated_oneof_is_untagged_union() {
    let api = lower(
        NO_PATHS,
        r#"
    Value:
      oneOf:
        - type: string
        - type: integer
          format: int64
"#,
    );
    let Shape::Union(u) = &decl(&api, "Value").shape else {
        panic!("expected union");
    };
    assert!(u.discriminator.is_none());
    assert_eq!(u.variants.len(), 2);
    assert!(u.variants.iter().all(|v| v.tag.is_none()));
}

#[test]
fn allof_single_ref_wrapper_collapses_to_ref() {
    // The protoc-gen-openapi pattern: allOf [$ref] used only to attach a
    // description/readOnly to a referenced type.
    let api = lower(
        NO_PATHS,
        r#"
    Key:
      type: object
      properties:
        info:
          readOnly: true
          allOf:
            - $ref: '#/components/schemas/KeyInfo'
          description: Server-populated info.
    KeyInfo:
      type: object
      properties:
        count:
          type: integer
          format: int32
"#,
    );
    let Shape::Struct(s) = &decl(&api, "Key").shape else {
        panic!("expected struct");
    };
    let info = &s.fields[0];
    assert_eq!(info.wire_name, "info");
    assert_eq!(info.ty, Ty::Named("KeyInfo".into()));
    assert!(info.read_only);
    assert_eq!(info.description.as_deref(), Some("Server-populated info."));
}

#[test]
fn allof_of_multiple_objects_merges_fields() {
    let api = lower(
        NO_PATHS,
        r#"
    Base:
      type: object
      required: [id]
      properties:
        id:
          type: string
    Extended:
      allOf:
        - $ref: '#/components/schemas/Base'
        - type: object
          properties:
            name:
              type: string
"#,
    );
    let Shape::Struct(s) = &decl(&api, "Extended").shape else {
        panic!("expected merged struct");
    };
    let names: Vec<_> = s.fields.iter().map(|f| f.wire_name.as_str()).collect();
    assert_eq!(names, vec!["id", "name"]);
    assert!(s.fields[0].required, "required carried through the merge");
}

#[test]
fn nullability_30_and_31_styles_both_detected() {
    let api = lower(
        NO_PATHS,
        r#"
    Thing:
      type: object
      properties:
        legacy:
          type: string
          nullable: true
        modern:
          type: ['string', 'null']
        plain:
          type: string
"#,
    );
    let Shape::Struct(s) = &decl(&api, "Thing").shape else {
        panic!("expected struct");
    };
    assert!(s.fields[0].nullable);
    assert!(s.fields[1].nullable);
    assert_eq!(s.fields[1].ty, Ty::String, "null stripped from the type");
    assert!(!s.fields[2].nullable);
}

#[test]
fn inline_object_lifted_with_deterministic_name() {
    let api = lower(
        NO_PATHS,
        r#"
    Agent:
      type: object
      properties:
        settings:
          type: object
          properties:
            retries:
              type: integer
              format: int32
"#,
    );
    let Shape::Struct(s) = &decl(&api, "Agent").shape else {
        panic!("expected struct");
    };
    assert_eq!(s.fields[0].ty, Ty::Named("AgentSettings".into()));
    assert!(matches!(
        &decl(&api, "AgentSettings").shape,
        Shape::Struct(_)
    ));
}

#[test]
fn string_enum_and_scalar_formats() {
    let api = lower(
        NO_PATHS,
        r#"
    Mode:
      type: string
      enum: [MODE_UNSPECIFIED, MODE_FAST]
    Stamps:
      type: object
      properties:
        createdAt:
          type: string
          format: date-time
        big:
          type: integer
          format: int64
        small:
          type: integer
          format: int32
        blob:
          type: string
          format: byte
        anything: {}
"#,
    );
    let Shape::Enum(e) = &decl(&api, "Mode").shape else {
        panic!("expected enum");
    };
    assert_eq!(e.values, vec!["MODE_UNSPECIFIED", "MODE_FAST"]);
    let Shape::Struct(s) = &decl(&api, "Stamps").shape else {
        panic!("expected struct");
    };
    let tys: Vec<_> = s.fields.iter().map(|f| f.ty.clone()).collect();
    assert_eq!(
        tys,
        vec![Ty::Timestamp, Ty::Int64, Ty::Int32, Ty::Bytes, Ty::Json]
    );
}

#[test]
fn single_value_enum_becomes_literal_type() {
    // Discriminated-union variants declare their tag as a one-value enum;
    // that must lower to a literal type so unions narrow naturally.
    let api = lower(
        NO_PATHS,
        r#"
    Variant:
      type: object
      required: [type]
      properties:
        type:
          type: string
          enum: [openai]
"#,
    );
    let Shape::Struct(s) = &decl(&api, "Variant").shape else {
        panic!("expected struct");
    };
    assert_eq!(s.fields[0].ty, Ty::Literal("openai".into()));
}

#[test]
fn additional_properties_becomes_map() {
    let api = lower(
        NO_PATHS,
        r#"
    Labels:
      type: object
      additionalProperties:
        type: string
"#,
    );
    match &decl(&api, "Labels").shape {
        Shape::Alias(Ty::Map(inner)) => assert_eq!(**inner, Ty::String),
        Shape::Struct(s) => {
            assert!(s.fields.is_empty());
            assert_eq!(s.additional, Some(Ty::String));
        }
        other => panic!("expected map-ish shape, got {other:?}"),
    }
}

#[test]
fn recursive_schema_does_not_loop() {
    let api = lower(
        NO_PATHS,
        r#"
    TreeNode:
      type: object
      properties:
        children:
          type: array
          items:
            $ref: '#/components/schemas/TreeNode'
"#,
    );
    let Shape::Struct(s) = &decl(&api, "TreeNode").shape else {
        panic!("expected struct");
    };
    assert_eq!(
        s.fields[0].ty,
        Ty::List(Box::new(Ty::Named("TreeNode".into())))
    );
}

const CRUD_PATHS: &str = r#"
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
  /v1/workspaces/{workspaceId}/objectives:
    get:
      tags: [ObjectiveService, Objectives]
      operationId: ObjectiveService_ListObjectives
      parameters:
        - name: workspaceId
          in: path
          required: true
          schema: { type: string }
        - name: limit
          in: query
          schema: { type: integer, format: int32 }
        - name: cursor
          in: query
          schema: { type: string }
      responses:
        '200':
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/ListObjectivesResponse'
    post:
      tags: [ObjectiveService, Objectives]
      operationId: ObjectiveService_CreateObjective
      parameters:
        - name: workspaceId
          in: path
          required: true
          schema: { type: string }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateObjectiveRequest'
      responses:
        '200':
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Objective'
  /v1/workspaces/{workspaceId}/objectives/{objectiveId}/events:stream:
    get:
      tags: [ObjectiveEventStreamsService, Objectives]
      operationId: ObjectiveEventStreamsService_StreamObjectiveEvents
      parameters:
        - name: workspaceId
          in: path
          required: true
          schema: { type: string }
        - name: objectiveId
          in: path
          required: true
          schema: { type: string }
      responses:
        '200':
          content:
            text/event-stream:
              schema:
                $ref: '#/components/schemas/ObjectiveEvent'
"#;

const CRUD_SCHEMAS: &str = r#"
    Account:
      type: object
      properties:
        info:
          type: string
    Objective:
      type: object
      properties:
        id:
          type: string
    ObjectiveEvent:
      type: object
      properties:
        data:
          type: string
    CreateObjectiveRequest:
      type: object
      required: [spec]
      properties:
        workspaceId:
          readOnly: true
          type: string
        spec:
          type: string
    ListObjectivesResponse:
      type: object
      properties:
        items:
          type: array
          items:
            $ref: '#/components/schemas/Objective'
        pagination:
          $ref: '#/components/schemas/Page'
    Page:
      type: object
      properties:
        nextCursor:
          type: string
"#;

fn crud() -> ir::Api {
    lower(CRUD_PATHS, CRUD_SCHEMAS)
}

fn resource<'a>(api: &'a ir::Api, name: &str) -> &'a ir::Resource {
    api.resources
        .iter()
        .find(|r| r.name == name)
        .unwrap_or_else(|| {
            panic!(
                "resource {name} exists, have: {:?}",
                api.resources.iter().map(|r| &r.name).collect::<Vec<_>>()
            )
        })
}

fn operation<'a>(api: &'a ir::Api, res: &str, op: &str) -> &'a ir::Operation {
    resource(api, res)
        .operations
        .iter()
        .find(|o| o.name == op)
        .unwrap_or_else(|| {
            panic!(
                "operation {res}.{op} exists, have: {:?}",
                resource(api, res)
                    .operations
                    .iter()
                    .map(|o| &o.name)
                    .collect::<Vec<_>>()
            )
        })
}

#[test]
fn resources_group_by_resource_tag_and_get_becomes_retrieve() {
    let api = crud();
    let retrieve = operation(&api, "accounts", "retrieve");
    assert_eq!(retrieve.http_method, HttpMethod::Get);
    assert!(matches!(&retrieve.response, ResponseKind::Json(Ty::Named(n)) if n == "Account"));
}

#[test]
fn method_name_strips_resource_noun_and_own_id_is_positional() {
    let api = crud();
    // StreamObjectiveEvents on resource Objectives -> stream_events
    let stream = operation(&api, "objectives", "stream_events");
    let positional = stream
        .positionals
        .first()
        .expect("objectiveId is positional");
    assert_eq!(positional.wire_name, "objectiveId");
    // workspaceId stays in the params bag
    assert_eq!(stream.path_params.len(), 1);
    assert_eq!(stream.path_params[0].wire_name, "workspaceId");
}

#[test]
fn sse_operation_marked_streaming() {
    let api = crud();
    let stream = operation(&api, "objectives", "stream_events");
    assert!(matches!(&stream.response, ResponseKind::Sse(Ty::Named(n)) if n == "ObjectiveEvent"));
}

#[test]
fn cursor_pagination_detected_from_shape() {
    let api = crud();
    let list = operation(&api, "objectives", "list");
    let page = list.pagination.as_ref().expect("pagination detected");
    assert_eq!(page.item_ty, Ty::Named("Objective".into()));
    assert_eq!(page.items_field, "items");
    assert_eq!(page.cursor_param, "cursor");
    assert_eq!(page.next_cursor_path, "pagination.nextCursor");
}

#[test]
fn body_fields_flattened_and_readonly_excluded() {
    let api = crud();
    let create = operation(&api, "objectives", "create");
    // workspaceId is readOnly in the body (server fills it from the path),
    // so the only body field is `spec`; workspaceId arrives via path params.
    let body: Vec<_> = create
        .body_fields
        .iter()
        .map(|f| f.wire_name.as_str())
        .collect();
    assert_eq!(body, vec!["spec"]);
    assert!(create.body_fields[0].required);
    assert_eq!(create.path_params[0].wire_name, "workspaceId");
    assert!(create.positionals.is_empty(), "create has no own-id param");
}

#[test]
fn bearer_auth_and_base_url_detected() {
    let api = crud();
    assert_eq!(api.auth, Auth::Bearer);
    assert_eq!(api.base_url, "https://api.cadenya.com");
    assert_eq!(api.name, "Cadenya");
}

#[test]
fn http_basic_auth_detected() {
    let doc = r#"
openapi: 3.1.0
info: { title: Passwords API, version: '1.0' }
paths: {}
components:
  securitySchemes:
    basicAuth: { type: http, scheme: Basic }
security:
  - basicAuth: []
"#;
    let spec = redwood::openapi::parse(doc).expect("spec parses");
    let api = redwood::ir::lower::lower(&spec).expect("spec lowers");
    assert_eq!(api.auth, Auth::Basic);
    assert_eq!(api.basic_username_env, "PASSWORDS_USERNAME");
    assert_eq!(api.basic_password_env, "PASSWORDS_PASSWORD");
}

#[test]
fn webhooks_stanza_lowers_to_events() {
    let doc = r#"
openapi: 3.1.0
info:
  title: Cadenya API
  version: '1.0'
paths: {}
webhooks:
  objective_event.user_message:
    post:
      summary: Objective user message event
      description: Triggered when a user message event occurs
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/ObjectiveEventWebhookData'
      responses:
        '200':
          description: ok
  objective_event.error:
    post:
      summary: Objective error event
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/ObjectiveEventWebhookData'
      responses:
        '200':
          description: ok
components:
  schemas:
    ObjectiveEventWebhookData:
      type: object
      required: [type, timestamp, data]
      properties:
        type:
          type: string
        timestamp:
          type: string
          format: date-time
        data:
          type: object
          properties:
            payload:
              type: string
"#;
    let spec = redwood::openapi::parse(doc).expect("spec parses");
    let api = ir::lower::lower(&spec).expect("spec lowers");
    assert_eq!(api.webhooks.len(), 2);
    let first = &api.webhooks[0];
    assert_eq!(first.name, "objective_event.user_message");
    assert_eq!(
        first.summary.as_deref(),
        Some("Objective user message event")
    );
    assert_eq!(first.payload, Ty::Named("ObjectiveEventWebhookData".into()));
    // The payload envelope has a required string `type` field, so events
    // discriminate on it.
    assert_eq!(first.discriminator_field.as_deref(), Some("type"));
}

#[test]
fn webhook_without_type_field_has_no_discriminator() {
    let doc = r#"
openapi: 3.1.0
info:
  title: T API
  version: '1.0'
paths: {}
webhooks:
  ping:
    post:
      requestBody:
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/Ping'
      responses:
        '200':
          description: ok
components:
  schemas:
    Ping:
      type: object
      properties:
        at:
          type: string
"#;
    let spec = redwood::openapi::parse(doc).expect("spec parses");
    let api = ir::lower::lower(&spec).expect("spec lowers");
    assert_eq!(api.webhooks[0].discriminator_field, None);
}

/// OpenAPI parameter semantics: operation-level params REPLACE shared Path
/// Item params in place; unsupported locations/serializations fail loudly
/// instead of silently erasing contract requirements.
#[test]
fn parameter_merge_and_faithfulness_gate() {
    let base = |params_shared: &str, params_op: &str| -> String {
        format!(
            r#"
openapi: 3.1.0
info: {{ title: Probe API, version: '1.0' }}
paths:
  /v1/things:
    parameters:
{params_shared}
    get:
      tags: [ThingService]
      operationId: ThingService_ListThings
      parameters:
{params_op}
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema: {{ type: object }}
"#
        )
    };

    // Override: op-level required integer replaces shared optional string.
    let spec = redwood::openapi::parse(&base(
        "      - {name: mode, in: query, schema: {type: string}}",
        "      - {name: mode, in: query, required: true, schema: {type: integer, format: int32}}",
    ))
    .expect("parses");
    let api = redwood::ir::lower::lower(&spec)
        .map_err(|e| format!("{e:#}"))
        .expect("lowers");
    let op = &api.resources[0].operations[0];
    let modes: Vec<_> = op
        .query_params
        .iter()
        .filter(|p| p.wire_name == "mode")
        .collect();
    assert_eq!(modes.len(), 1, "override must replace, not duplicate");
    assert!(modes[0].required);
    assert!(matches!(modes[0].ty, redwood::ir::Ty::Int32));

    // Duplicates WITHIN one list are invalid input.
    let spec = redwood::openapi::parse(&base(
        "      []",
        "      - {name: mode, in: query, schema: {type: string}}\n      - {name: mode, in: query, schema: {type: string}}",
    ))
    .expect("parses");
    let err = redwood::ir::lower::lower(&spec).expect_err("duplicate rejected");
    assert!(format!("{err:#}").contains("duplicate"), "{err:#}");

    // Header parameters fail loudly instead of being erased.
    let spec = redwood::openapi::parse(&base(
        "      []",
        "      - {name: X-Tenant, in: header, required: true, schema: {type: string}}",
    ))
    .expect("parses");
    let err = redwood::ir::lower::lower(&spec).expect_err("header rejected");
    assert!(
        format!("{err:#}").contains("header parameter X-Tenant"),
        "{err:#}"
    );

    // Unimplemented query serialization fails loudly.
    let spec = redwood::openapi::parse(&base(
        "      []",
        "      - {name: tags, in: query, style: pipeDelimited, explode: false, schema: {type: array, items: {type: string}}}",
    ))
    .expect("parses");
    let err = redwood::ir::lower::lower(&spec).expect_err("style rejected");
    assert!(format!("{err:#}").contains("style/explode"), "{err:#}");
}

/// Auth resolves from the effective Security REQUIREMENT: unused component
/// declarations have no effect, and unsupported requirement shapes fail
/// generation instead of silently choosing a scheme. Unsupported verbs also
/// fail rather than silently vanishing.
#[test]
fn security_requirements_and_verb_audit() {
    let with = |root: &str, op_security: &str, extra_verb: &str| -> String {
        format!(
            r#"
openapi: 3.1.0
info: {{ title: Probe API, version: '1.0' }}
paths:
  /v1/things:
    get:
      tags: [ThingService]
      operationId: ThingService_ListThings
{op_security}
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema: {{ type: object }}
{extra_verb}
components:
  securitySchemes:
    bearerAuth: {{ type: http, scheme: bearer }}
{root}
"#
        )
    };

    // Declared-but-unused scheme: NO root requirement -> Auth::None.
    let spec = redwood::openapi::parse(&with("", "", "")).expect("parses");
    let api = redwood::ir::lower::lower(&spec).expect("lowers");
    assert!(
        matches!(api.auth, redwood::ir::Auth::None),
        "{:?}",
        api.auth
    );

    // Root requirement selects the scheme.
    let spec =
        redwood::openapi::parse(&with("security:\n  - bearerAuth: []", "", "")).expect("parses");
    let api = redwood::ir::lower::lower(&spec).expect("lowers");
    assert!(matches!(api.auth, redwood::ir::Auth::Bearer));

    // Operation-level `security: []` in an authenticated API fails loudly.
    let spec = redwood::openapi::parse(&with(
        "security:\n  - bearerAuth: []",
        "      security: []",
        "",
    ))
    .expect("parses");
    let err = redwood::ir::lower::lower(&spec).expect_err("public override rejected");
    assert!(format!("{err:#}").contains("explicitly public"), "{err:#}");

    // Undefined scheme in the requirement fails loudly.
    let spec = redwood::openapi::parse(&with("security:\n  - nope: []", "", "")).expect("parses");
    let err = redwood::ir::lower::lower(&spec).expect_err("undefined scheme rejected");
    assert!(format!("{err:#}").contains("undefined scheme"), "{err:#}");

    // HEAD is audited, never silently dropped.
    let spec = redwood::openapi::parse(&with(
        "",
        "",
        "    head:\n      operationId: ThingService_HeadThings\n      responses:\n        '200': { description: OK }",
    ))
    .expect("parses");
    let err = redwood::ir::lower::lower(&spec).expect_err("HEAD rejected");
    assert!(format!("{err:#}").contains("HEAD operation"), "{err:#}");
}
