//! Body plan: how a request body is presented as flags. Every rule is over
//! the IR (no vocabulary from any real spec), so this fixture is an
//! unrelated "orchard" API that exercises each node shape once.

use redwood::ir::plan::{body_plan, BodyPlan, InputKind, PlanOptions};
use redwood::ir::{self, Operation, Ty};

const SPEC: &str = r#"
openapi: 3.1.0
info:
  title: Orchard API
  version: '1.0'
servers:
  - url: https://api.orchard.example
paths:
  /v1/groves/{groveId}/trees:
    post:
      tags: [TreeService]
      summary: Plant a tree
      operationId: TreeService_CreateTree
      parameters:
        - name: groveId
          in: path
          required: true
          schema: { type: string }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateTreeRequest'
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Tree'
  /v1/groves/{groveId}/trees/{id}:
    patch:
      tags: [TreeService]
      summary: Update a tree
      operationId: TreeService_UpdateTree
      parameters:
        - name: groveId
          in: path
          required: true
          schema: { type: string }
        - name: id
          in: path
          required: true
          schema: { type: string }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/UpdateTreeRequest'
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Tree'
  /v1/groves/{groveId}/trees/{treeId}/grafts:
    post:
      tags: [TreeService]
      summary: Add a graft
      operationId: TreeService_AddGraft
      parameters:
        - name: groveId
          in: path
          required: true
          schema: { type: string }
        - name: treeId
          in: path
          required: true
          schema: { type: string }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/GraftSource'
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Tree'
  /v1/groves/{groveId}/trees/{treeId}:prune:
    post:
      tags: [TreeService]
      summary: Prune a tree
      operationId: TreeService_PruneTree
      parameters:
        - name: groveId
          in: path
          required: true
          schema: { type: string }
        - name: treeId
          in: path
          required: true
          schema: { type: string }
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/PruneTreeRequest'
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Tree'
components:
  schemas:
    Tree:
      type: object
      properties:
        id: { type: string }
    CreateTreeRequest:
      type: object
      required: [metadata, spec]
      properties:
        groveId: { type: string, readOnly: true }
        metadata:
          $ref: '#/components/schemas/CreateMetadata'
        spec:
          $ref: '#/components/schemas/TreeSpec'
    UpdateTreeRequest:
      type: object
      properties:
        groveId: { type: string, readOnly: true }
        id: { type: string, readOnly: true }
        metadata:
          $ref: '#/components/schemas/CreateMetadata'
        spec:
          $ref: '#/components/schemas/TreeSpec'
        updateMask: { type: string, format: field-mask }
    PruneTreeRequest:
      type: object
      properties:
        file: { type: string }
    CreateMetadata:
      type: object
      required: [name]
      properties:
        name: { type: string, description: "Display name." }
        externalId: { type: string }
        labels:
          type: object
          additionalProperties: { type: string }
    TreeSpec:
      type: object
      required: [rootstock]
      properties:
        description: { type: string }
        rootstock:
          $ref: '#/components/schemas/Rootstock'
        pruning:
          $ref: '#/components/schemas/PruningPolicy'
        tags:
          type: array
          items: { type: string }
        sensors:
          type: array
          items:
            $ref: '#/components/schemas/Sensor'
        secrets:
          type: array
          items:
            $ref: '#/components/schemas/Secret'
        branches:
          type: array
          items:
            $ref: '#/components/schemas/Branch'
        traits:
          type: object
          additionalProperties: {}
        shape: {}
        season:
          $ref: '#/components/schemas/Season'
        growth:
          $ref: '#/components/schemas/GrowthMode'
        lineage:
          $ref: '#/components/schemas/Lineage'
    Rootstock:
      oneOf:
        - $ref: '#/components/schemas/RootstockSeedVariant'
        - $ref: '#/components/schemas/RootstockCloneVariant'
      discriminator:
        propertyName: type
        mapping:
          seed: '#/components/schemas/RootstockSeedVariant'
          clone: '#/components/schemas/RootstockCloneVariant'
    RootstockSeedVariant:
      type: object
      required: [type, seed]
      properties:
        type: { type: string, enum: [seed] }
        seed:
          $ref: '#/components/schemas/SeedRootstock'
    SeedRootstock:
      type: object
      properties:
        source: { type: string }
        depth: { type: integer, format: int32 }
        origin:
          $ref: '#/components/schemas/Origin'
    RootstockCloneVariant:
      type: object
      required: [type, clone]
      properties:
        type: { type: string, enum: [clone] }
        clone:
          $ref: '#/components/schemas/CloneRootstock'
    CloneRootstock:
      type: object
      required: [clone]
      properties:
        clone: { type: string }
    Origin:
      oneOf:
        - $ref: '#/components/schemas/OriginNurseryVariant'
        - $ref: '#/components/schemas/OriginWildVariant'
      discriminator:
        propertyName: type
        mapping:
          nursery: '#/components/schemas/OriginNurseryVariant'
          wild: '#/components/schemas/OriginWildVariant'
    OriginNurseryVariant:
      type: object
      required: [type, nursery]
      properties:
        type: { type: string, enum: [nursery] }
        nursery:
          $ref: '#/components/schemas/Nursery'
    Nursery:
      type: object
      properties:
        name: { type: string }
    OriginWildVariant:
      type: object
      required: [type, wild]
      properties:
        type: { type: string, enum: [wild] }
        wild:
          $ref: '#/components/schemas/Wild'
    Wild:
      type: object
      properties:
        region: { type: string }
    PruningPolicy:
      type: object
      properties:
        maxHeight: { type: number, format: float }
        enabled: { type: boolean }
    Sensor:
      type: object
      required: [kind]
      properties:
        kind: { type: string }
        threshold: { type: integer }
    Secret:
      type: object
      properties:
        name: { type: string }
        value: { type: string }
    Branch:
      type: object
      properties:
        label: { type: string }
        origin:
          $ref: '#/components/schemas/Origin'
    Season:
      type: string
      enum: [SEASON_UNSPECIFIED, SEASON_SPRING, SEASON_AUTUMN, SEASON_SPRING]
    GrowthMode:
      type: string
      enum: [FAST, SLOW]
    Lineage:
      type: object
      properties:
        parent:
          $ref: '#/components/schemas/Lineage'
        note: { type: string }
    GraftSource:
      oneOf:
        - $ref: '#/components/schemas/GraftScion'
        - $ref: '#/components/schemas/GraftCutting'
      discriminator:
        propertyName: type
        mapping:
          scion: '#/components/schemas/GraftScion'
          cutting: '#/components/schemas/GraftCutting'
    GraftScion:
      type: object
      required: [type, scionId]
      properties:
        type: { type: string, enum: [scion] }
        scionId: { type: string }
    GraftCutting:
      type: object
      required: [type, cutting]
      properties:
        type: { type: string, enum: [cutting] }
        cutting:
          $ref: '#/components/schemas/Cutting'
    Cutting:
      type: object
      properties:
        length: { type: integer }
"#;

fn api() -> ir::Api {
    let spec = redwood::openapi::parse(SPEC).expect("fixture parses");
    ir::lower::lower(&spec).expect("fixture lowers")
}

fn op<'a>(api: &'a ir::Api, id: &str) -> &'a Operation {
    api.resources
        .iter()
        .flat_map(|r| r.operations.iter())
        .find(|o| o.id == id)
        .unwrap_or_else(|| panic!("operation {id}"))
}

fn plan(api: &ir::Api, id: &str) -> BodyPlan {
    body_plan(api, op(api, id), &PlanOptions::default())
        .expect("plans")
        .expect("has a body")
}

fn flags(plan: &BodyPlan) -> Vec<&str> {
    plan.inputs.iter().map(|i| i.flag.as_str()).collect()
}

fn input<'a>(plan: &'a BodyPlan, flag: &str) -> &'a redwood::ir::plan::Input {
    plan.inputs
        .iter()
        .find(|i| i.flag == flag)
        .unwrap_or_else(|| panic!("flag --{flag} in {:?}", flags(plan)))
}

#[test]
fn envelope_segments_are_elided_and_scalars_become_typed_leaves() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    let name = input(&p, "name");
    assert_eq!(name.path, vec!["metadata", "name"]);
    assert!(matches!(name.kind, InputKind::Leaf(Ty::String)));
    assert!(name.required, "required through a required envelope");
    assert_eq!(name.description.as_deref(), Some("Display name."));
    assert!(matches!(
        input(&p, "external-id").kind,
        InputKind::Leaf(Ty::String)
    ));
    assert!(!input(&p, "external-id").required);
    assert_eq!(input(&p, "description").path, vec!["spec", "description"]);
    // readOnly fields are server-owned: never a flag.
    assert!(!flags(&p).contains(&"grove-id"));
}

#[test]
fn envelopes_keep_a_document_input_at_their_root() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    assert!(matches!(input(&p, "metadata").kind, InputKind::Doc(_)));
    assert!(matches!(input(&p, "spec").kind, InputKind::Doc(_)));
    // A nested struct is both a document and a set of leaves.
    let pruning = input(&p, "pruning");
    assert!(matches!(pruning.kind, InputKind::Doc(_)));
    assert!(matches!(
        input(&p, "pruning-max-height").kind,
        InputKind::Leaf(Ty::Float)
    ));
    assert!(matches!(
        input(&p, "pruning-enabled").kind,
        InputKind::Leaf(Ty::Bool)
    ));
    // Documents sort before the leaves they contain (merge order).
    let pos = |f: &str| p.inputs.iter().position(|i| i.flag == f).unwrap();
    assert!(pos("spec") < pos("pruning"));
    assert!(pos("pruning") < pos("pruning-enabled"));
}

#[test]
fn string_maps_become_repeatable_key_value_flags() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    let label = input(&p, "label");
    assert_eq!(label.path, vec!["metadata", "labels"]);
    assert!(matches!(label.kind, InputKind::KvMap(Ty::String)));
}

#[test]
fn lists_split_by_item_shape() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    assert!(matches!(
        input(&p, "tag").kind,
        InputKind::ScalarList(Ty::String)
    ));
    match &input(&p, "sensor").kind {
        InputKind::ShorthandList { fields, .. } => {
            let keys: Vec<&str> = fields.iter().map(|f| f.key.as_str()).collect();
            assert_eq!(keys, vec!["kind", "threshold"]);
            assert!(fields[0].required);
            assert!(matches!(fields[1].ty, Ty::Int64 | Ty::Int32));
        }
        other => panic!("sensor is shorthand, got {other:?}"),
    }
    // {name, value} items additionally accept the NAME=VALUE pair form.
    let secret = input(&p, "secret");
    assert!(matches!(secret.kind, InputKind::ShorthandList { .. }));
    assert_eq!(
        secret
            .pair_form
            .as_ref()
            .map(|(k, v)| (k.as_str(), v.as_str())),
        Some(("name", "value"))
    );
    assert!(input(&p, "sensor").pair_form.is_none());
    // Items with nested structure stay documents.
    assert!(matches!(input(&p, "branch").kind, InputKind::DocList(_)));
    assert!(!flags(&p).iter().any(|f| f.starts_with("branch-")));
}

#[test]
fn opaque_maps_and_untyped_values_take_documents_or_entries() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    assert!(matches!(input(&p, "trait").kind, InputKind::EntryDoc(_)));
    assert!(matches!(
        input(&p, "shape").kind,
        InputKind::EntryDoc(Ty::Json)
    ));
}

#[test]
fn top_level_unions_collapse_into_arm_prefixed_flags_with_a_tag_flag() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    let rootstock = input(&p, "rootstock");
    assert!(matches!(rootstock.kind, InputKind::UnionTag));
    assert!(rootstock.required);
    let union = &p.unions[rootstock.union.expect("tag flag points at its union")];
    assert_eq!(union.path, vec!["spec", "rootstock"]);
    assert_eq!(union.discriminator, "type");
    let tags: Vec<&str> = union.arms.iter().map(|a| a.tag.as_str()).collect();
    assert_eq!(tags, vec!["seed", "clone"]);
    assert!(union.inferable, "arm keys are disjoint");
    assert_eq!(union.arms[0].keys, vec!["seed"]);
    assert_eq!(
        union.arms[0].init_objects,
        vec!["seed"],
        "required struct arm field is initialised on an explicit tag"
    );
    // The union's own segment ("rootstock") is dropped; the arm field
    // ("seed") is the prefix. The arm struct is also a document input.
    let source = input(&p, "seed-source");
    assert_eq!(source.path, vec!["spec", "rootstock", "seed", "source"]);
    assert_eq!(source.arm.as_ref().map(|a| a.tag.as_str()), Some("seed"));
    assert_eq!(source.category, "rootstock = seed");
    assert!(matches!(input(&p, "seed").kind, InputKind::Doc(_)));
    assert!(matches!(
        input(&p, "seed-depth").kind,
        InputKind::Leaf(Ty::Int32)
    ));
    // Consecutive duplicate segments collapse: rootstock.clone.clone -> --clone.
    let clone = input(&p, "clone");
    assert_eq!(clone.path, vec!["spec", "rootstock", "clone", "clone"]);
    assert!(matches!(clone.kind, InputKind::Leaf(Ty::String)));
    // Discriminator literals are stamped, never flags.
    assert!(!flags(&p).contains(&"type"));
    assert!(!flags(&p).iter().any(|f| f.ends_with("-type")));
}

#[test]
fn nested_unions_keep_their_segment() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    let origin = input(&p, "seed-origin");
    assert!(matches!(origin.kind, InputKind::UnionTag));
    assert!(!origin.required);
    let union = &p.unions[origin.union.unwrap()];
    assert_eq!(union.path, vec!["spec", "rootstock", "seed", "origin"]);
    assert_eq!(
        union.parent_arm.as_ref().map(|a| a.tag.as_str()),
        Some("seed")
    );
    let name = input(&p, "seed-origin-nursery-name");
    assert_eq!(
        name.path,
        vec!["spec", "rootstock", "seed", "origin", "nursery", "name"]
    );
    assert_eq!(name.category, "seed-origin = nursery");
    assert!(flags(&p).contains(&"seed-origin-wild-region"));
    // Unions are listed outermost first so the runtime resolves parents
    // before children.
    assert_eq!(p.unions[0].path, vec!["spec", "rootstock"]);
}

#[test]
fn enums_offer_short_forms_and_drop_unspecified() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    let season = input(&p, "season");
    assert_eq!(
        season.enum_values.as_deref(),
        Some(&["SEASON_SPRING".to_string(), "SEASON_AUTUMN".to_string()][..]),
        "deduplicated, UNSPECIFIED dropped"
    );
    let short: Vec<(&str, &str)> = season
        .enum_short
        .as_ref()
        .unwrap()
        .iter()
        .map(|(s, f)| (s.as_str(), f.as_str()))
        .collect();
    assert_eq!(
        short,
        vec![("spring", "SEASON_SPRING"), ("autumn", "SEASON_AUTUMN")]
    );
    // No common prefix: the short form is the lowercase value.
    let growth = input(&p, "growth");
    let short: Vec<&str> = growth
        .enum_short
        .as_ref()
        .unwrap()
        .iter()
        .map(|(s, _)| s.as_str())
        .collect();
    assert_eq!(short, vec!["fast", "slow"]);

    let opts = PlanOptions {
        enum_short_forms: false,
        ..PlanOptions::default()
    };
    let p = body_plan(&api, op(&api, "TreeService_CreateTree"), &opts)
        .unwrap()
        .unwrap();
    assert!(input(&p, "season").enum_short.is_none());
}

#[test]
fn recursive_types_cut_to_a_document() {
    let api = api();
    let p = plan(&api, "TreeService_CreateTree");
    assert!(matches!(input(&p, "lineage").kind, InputKind::Doc(_)));
    assert!(matches!(
        input(&p, "lineage-note").kind,
        InputKind::Leaf(Ty::String)
    ));
    // lineage.parent is Lineage again: a document, not an infinite walk.
    assert!(matches!(
        input(&p, "lineage-parent").kind,
        InputKind::Doc(_)
    ));
    assert!(!flags(&p).contains(&"lineage-parent-note"));
}

#[test]
fn update_bodies_expose_the_field_mask() {
    let api = api();
    let update = op(&api, "TreeService_UpdateTree");
    assert_eq!(update.update_mask.as_deref(), Some("updateMask"));
    assert_eq!(op(&api, "TreeService_CreateTree").update_mask, None);
    let p = plan(&api, "TreeService_UpdateTree");
    assert!(matches!(
        input(&p, "update-mask").kind,
        InputKind::Leaf(Ty::String)
    ));
    // Nothing is required on a patch whose envelopes are optional.
    assert!(!input(&p, "name").required);
}

#[test]
fn whole_body_unions_plan_from_the_root() {
    let api = api();
    let p = plan(&api, "TreeService_AddGraft");
    assert!(p.whole_body, "the assembled body IS the request body");
    let tag = input(&p, "type");
    assert!(matches!(tag.kind, InputKind::UnionTag));
    assert!(tag.required);
    let union = &p.unions[tag.union.unwrap()];
    assert!(union.path.is_empty());
    assert_eq!(union.arms[0].keys, vec!["scionId"]);
    let scion = input(&p, "scion-id");
    assert_eq!(scion.path, vec!["scionId"]);
    assert_eq!(scion.arm.as_ref().map(|a| a.tag.as_str()), Some("scion"));
    assert!(matches!(input(&p, "cutting").kind, InputKind::Doc(_)));
    assert!(matches!(
        input(&p, "cutting-length").kind,
        InputKind::Leaf(_)
    ));
}

#[test]
fn bodyless_operations_have_no_plan() {
    let api = api();
    // No request body on this spec's GET-less fixture: synthesize by
    // checking an op with an empty body plan is None.
    let spec = redwood::openapi::parse(
        r#"
openapi: 3.1.0
info: { title: Ping API, version: '1' }
servers: [{ url: https://ping.example }]
paths:
  /v1/pings:
    get:
      operationId: PingService_ListPings
      responses:
        '200': { description: OK, content: { application/json: { schema: { type: object } } } }
"#,
    )
    .unwrap();
    let ping = ir::lower::lower(&spec).unwrap();
    let list = op(&ping, "PingService_ListPings");
    assert!(body_plan(&ping, list, &PlanOptions::default())
        .unwrap()
        .is_none());
    let _ = api;
}

#[test]
fn renames_and_singular_overrides_apply_by_wire_path() {
    let api = api();
    let mut opts = PlanOptions::default();
    opts.rename.insert("spec.pruning".into(), "prune".into());
    opts.rename
        .insert("metadata.externalId".into(), "ref".into());
    opts.singular.insert("sensors".into(), "probe".into());
    let p = body_plan(&api, op(&api, "TreeService_CreateTree"), &opts)
        .unwrap()
        .unwrap();
    assert!(flags(&p).contains(&"prune-max-height"));
    assert!(flags(&p).contains(&"prune"));
    // A renamed prefix that its child extends collapses (N4): `max` +
    // `max-height` is `--max-height`, not `--max-max-height`.
    let mut overlap = PlanOptions::default();
    overlap.rename.insert("spec.pruning".into(), "max".into());
    let p2 = body_plan(&api, op(&api, "TreeService_CreateTree"), &overlap)
        .unwrap()
        .unwrap();
    assert!(flags(&p2).contains(&"max-height"), "{:?}", flags(&p2));
    assert!(!flags(&p2).contains(&"max-max-height"));
    assert!(flags(&p).contains(&"ref"));
    assert!(flags(&p).contains(&"probe"));
    assert!(!flags(&p).contains(&"pruning-max-height"));

    let mut bad = PlanOptions::default();
    bad.rename.insert("spec.nothing".into(), "x".into());
    let err = body_plan(&api, op(&api, "TreeService_CreateTree"), &bad).unwrap_err();
    assert!(err.to_string().contains("spec.nothing"), "{err}");
}

#[test]
fn elision_is_configurable() {
    let api = api();
    let opts = PlanOptions {
        elide: vec![],
        ..PlanOptions::default()
    };
    let p = body_plan(&api, op(&api, "TreeService_CreateTree"), &opts)
        .unwrap()
        .unwrap();
    assert!(flags(&p).contains(&"metadata-name"));
    // A union directly under the body root still collapses its segment.
    assert!(flags(&p).contains(&"spec-seed-source"));
}

#[test]
fn colliding_flag_names_are_a_generation_error() {
    let spec = redwood::openapi::parse(
        r#"
openapi: 3.1.0
info: { title: Clash API, version: '1' }
servers: [{ url: https://clash.example }]
paths:
  /v1/things:
    post:
      operationId: ThingService_CreateThing
      requestBody:
        required: true
        content:
          application/json:
            schema:
              type: object
              properties:
                metadata:
                  type: object
                  properties:
                    name: { type: string }
                spec:
                  type: object
                  properties:
                    name: { type: string }
      responses:
        '200': { description: OK, content: { application/json: { schema: { type: object } } } }
"#,
    )
    .unwrap();
    let api = ir::lower::lower(&spec).unwrap();
    let err = body_plan(
        &api,
        op(&api, "ThingService_CreateThing"),
        &PlanOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("--name"), "{err}");
    assert!(
        err.contains("metadata.name") && err.contains("spec.name"),
        "{err}"
    );
    assert!(err.contains("rename"), "points at the config fix: {err}");
}

#[test]
fn reserved_flag_names_collide_too() {
    assert!(
        redwood::ir::plan::RESERVED_FLAGS.contains(&"f"),
        "the -f alias occupies the same command-local namespace as body flags"
    );
    let api = api();
    let err = body_plan(
        &api,
        op(&api, "TreeService_PruneTree"),
        &PlanOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("--file") && err.contains("reserved"), "{err}");
}

#[test]
fn fields_shared_between_arms_do_not_identify_an_arm() {
    let spec = redwood::openapi::parse(
        r#"
openapi: 3.1.0
info: { title: Shared API, version: '1' }
servers: [{ url: https://shared.example }]
paths:
  /v1/notes:
    post:
      operationId: NoteService_CreateNote
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/NoteSource'
      responses:
        '200': { description: OK, content: { application/json: { schema: { type: object } } } }
components:
  schemas:
    NoteSource:
      oneOf:
        - $ref: '#/components/schemas/InlineNote'
        - $ref: '#/components/schemas/LinkedNote'
      discriminator:
        propertyName: type
        mapping:
          inline: '#/components/schemas/InlineNote'
          linked: '#/components/schemas/LinkedNote'
    InlineNote:
      type: object
      required: [type]
      properties:
        type: { type: string, enum: [inline] }
        title: { type: string }
        text: { type: string }
    LinkedNote:
      type: object
      required: [type]
      properties:
        type: { type: string, enum: [linked] }
        title: { type: string }
        url: { type: string }
"#,
    )
    .unwrap();
    let api = ir::lower::lower(&spec).unwrap();
    let p = body_plan(
        &api,
        op(&api, "NoteService_CreateNote"),
        &PlanOptions::default(),
    )
    .unwrap()
    .unwrap();
    let union = &p.unions[0];
    assert_eq!(
        union.arms[0].keys,
        vec!["text"],
        "title is shared, so it identifies nothing"
    );
    assert_eq!(union.arms[1].keys, vec!["url"]);
    assert!(union.inferable);
    // The shared field is one input serving both arms.
    assert_eq!(p.inputs.iter().filter(|i| i.flag == "title").count(), 1);
}

#[test]
fn shared_union_fields_must_have_compatible_cli_types() {
    let spec = redwood::openapi::parse(
        r#"
openapi: 3.1.0
info: { title: Shared Types API, version: '1' }
servers: [{ url: https://shared.example }]
paths:
  /v1/values:
    post:
      operationId: ValueService_CreateValue
      requestBody:
        required: true
        content:
          application/json:
            schema: { $ref: '#/components/schemas/Value' }
      responses:
        '200': { description: OK, content: { application/json: { schema: { type: object } } } }
components:
  schemas:
    Value:
      oneOf:
        - $ref: '#/components/schemas/TextValue'
        - $ref: '#/components/schemas/CountValue'
      discriminator:
        propertyName: type
        mapping:
          text: '#/components/schemas/TextValue'
          count: '#/components/schemas/CountValue'
    TextValue:
      type: object
      required: [type, value]
      properties:
        type: { type: string, enum: [text] }
        value: { type: string }
        textOnly: { type: string }
    CountValue:
      type: object
      required: [type, value]
      properties:
        type: { type: string, enum: [count] }
        value: { type: integer }
        countOnly: { type: integer }
"#,
    )
    .unwrap();
    let api = ir::lower::lower(&spec).unwrap();
    let err = body_plan(
        &api,
        op(&api, "ValueService_CreateValue"),
        &PlanOptions::default(),
    )
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("--value") && err.contains("incompatible types"),
        "{err}"
    );
}
