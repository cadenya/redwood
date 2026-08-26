//! Direction-aware model gate: a shared schema with readOnly and writeOnly
//! properties, used in BOTH a request and a response, must generate distinct
//! input/output views in every backend. Input accepts only username+password
//! (id is server-owned); output promises only id+username (password is an
//! input secret).

use redwood::backends::Backend;

const DIRECTIONAL_SPEC: &str = r#"
openapi: 3.1.0
info:
  title: NeutralDirectory API
  version: '1.0'
servers:
  - url: https://api.neutral-directory.example
paths:
  /v1/groups:
    post:
      tags: [GroupService]
      summary: Create a group
      operationId: GroupService_CreateGroup
      requestBody:
        required: true
        content:
          application/json:
            schema:
              $ref: '#/components/schemas/CreateGroupRequest'
      responses:
        '200':
          description: OK
          content:
            application/json:
              schema:
                $ref: '#/components/schemas/Group'
components:
  schemas:
    CreateGroupRequest:
      type: object
      required: [user]
      properties:
        user:
          $ref: '#/components/schemas/User'
    Group:
      type: object
      required: [user]
      properties:
        user:
          $ref: '#/components/schemas/User'
    User:
      type: object
      required: [id, username, password]
      properties:
        id:
          type: string
          readOnly: true
        username:
          type: string
        password:
          type: string
          writeOnly: true
"#;

fn lowered() -> redwood::ir::Api {
    let spec = redwood::openapi::parse(DIRECTIONAL_SPEC).expect("parses");
    redwood::ir::lower::lower(&spec).expect("lowers")
}

#[test]
fn divergence_is_detected_transitively() {
    let api = lowered();
    let divergent = api.divergent_types();
    assert!(divergent.contains("User"), "direct flags");
    assert!(
        divergent.contains("Group"),
        "transitive through a referencing type"
    );
}

#[test]
fn typescript_splits_input_and_output_views() {
    let api = lowered();
    let backend = redwood::backends::typescript::TypeScriptBackend {
        config: Default::default(),
    };
    let files = backend.generate(&api).expect("generates");
    let types = files.get("src/types.ts").expect("types.ts");

    // Output interface: id + username, never password.
    let user = section(types, "export interface User {", "}");
    assert!(user.contains("id: string"), "{user}");
    assert!(user.contains("username: string"), "{user}");
    assert!(!user.contains("password"), "output leaks writeOnly: {user}");

    // Input view: username + password required, never id.
    let user_param = section(types, "export interface UserParam {", "}");
    assert!(user_param.contains("username: string"), "{user_param}");
    assert!(user_param.contains("password: string"), "{user_param}");
    assert!(
        !user_param.contains("id"),
        "input accepts readOnly: {user_param}"
    );
}

#[test]
fn python_splits_input_and_output_views() {
    let api = lowered();
    let backend = redwood::backends::python::PythonBackend {
        config: Default::default(),
    };
    let files = backend.generate(&api).expect("generates");
    let pkg = files
        .keys()
        .find(|k| k.ends_with("/types.py"))
        .expect("types.py")
        .clone();
    let types = files.get(&pkg).unwrap();

    let user_param = section(
        types,
        "UserParam = TypedDict(\"UserParam\", {",
        ", total=False)",
    );
    assert!(user_param.contains("\"username\""), "{user_param}");
    assert!(user_param.contains("\"password\""), "{user_param}");
    assert!(
        !user_param.contains("\"id\""),
        "input accepts readOnly: {user_param}"
    );

    let user = section(types, "class User:", "@staticmethod");
    assert!(user.contains("id: str"), "{user}");
    assert!(!user.contains("password"), "output leaks writeOnly: {user}");
}

#[test]
fn go_emits_param_struct_without_read_only() {
    let api = lowered();
    let backend = redwood::backends::golang::GoBackend {
        config: Default::default(),
    };
    let files = backend.generate(&api).expect("generates");
    let types = files.get("types.go").expect("types.go");

    let user = section(types, "type User struct {", "}");
    assert!(user.contains("ID"), "{user}");
    assert!(!user.contains("Password"), "output leaks writeOnly: {user}");

    let user_param = section(types, "type UserParam struct {", "}");
    assert!(user_param.contains("Password"), "{user_param}");
    assert!(
        !user_param.contains("ID "),
        "input accepts readOnly: {user_param}"
    );
}

#[test]
fn ruby_model_and_encoder_follow_direction() {
    let api = lowered();
    let backend = redwood::backends::ruby::RubyBackend {
        config: Default::default(),
    };
    let files = backend.generate(&api).expect("generates");
    let types_path = files
        .keys()
        .find(|k| k.ends_with("/types.rb"))
        .expect("types.rb")
        .clone();
    let types = files.get(&types_path).unwrap();

    let user = section(types, "class User", "end");
    assert!(user.contains(":id"), "{user}");
    assert!(!user.contains("password"), "output leaks writeOnly: {user}");
    // Encoder drops the server-owned key on the way out.
    assert!(
        types.contains("DROP_USER") && types.contains("\"id\""),
        "readOnly drop set missing"
    );
}

#[test]
fn read_only_plus_write_only_is_rejected() {
    let doc = DIRECTIONAL_SPEC.replace(
        "        password:\n          type: string\n          writeOnly: true",
        "        password:\n          type: string\n          writeOnly: true\n          readOnly: true",
    );
    assert_ne!(doc, DIRECTIONAL_SPEC, "replacement applied");
    let spec = redwood::openapi::parse(&doc).expect("parses");
    let err = redwood::ir::lower::lower(&spec).unwrap_err();
    assert!(
        format!("{err:#}").contains("both readOnly and writeOnly"),
        "{err:#}"
    );
}

#[test]
fn manifest_samples_follow_direction() {
    let api = lowered();
    let backend = redwood::backends::manifest::ManifestBackend;
    let files = backend.generate(&api).expect("generates");
    let manifest: serde_json::Value =
        serde_json::from_str(files.get("manifest.json").unwrap()).unwrap();
    let op = &manifest["operations"][0];
    let body_sample = &op["bodyFields"][0]["sample"];
    assert!(body_sample.get("password").is_some(), "{body_sample}");
    assert!(
        body_sample.get("id").is_none(),
        "request sample carries readOnly: {body_sample}"
    );
    let resp = &op["response"]["sample"]["user"];
    assert!(resp.get("id").is_some(), "{resp}");
    assert!(
        resp.get("password").is_none(),
        "response sample carries writeOnly: {resp}"
    );
}

/// The text between the first occurrence of `start` and the next `end`.
fn section(text: &str, start: &str, end: &str) -> String {
    let from = text
        .find(start)
        .unwrap_or_else(|| panic!("missing {start}"));
    let rest = &text[from + start.len()..];
    let to = rest.find(end).unwrap_or(rest.len());
    rest[..to].to_string()
}
