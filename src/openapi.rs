//! Serde model of the subset of OpenAPI 3.0/3.1 we consume.
//!
//! This layer is deliberately dumb: it mirrors the document structure and
//! performs no normalization. All semantics (ref resolution, allOf flattening,
//! naming, nullability) live in `ir::lower`.

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct Spec {
    #[allow(dead_code)]
    pub openapi: String,
    pub info: Info,
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub paths: IndexMap<String, PathItem>,
    /// OpenAPI 3.1 webhooks: event name -> path item describing the delivery.
    #[serde(default)]
    pub webhooks: IndexMap<String, PathItem>,
    #[serde(default)]
    pub components: Components,
    #[serde(default)]
    pub security: Vec<IndexMap<String, Vec<String>>>,
    #[serde(default)]
    pub tags: Vec<Tag>,
}

#[derive(Debug, Deserialize)]
pub struct Info {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    pub version: String,
}

#[derive(Debug, Deserialize)]
pub struct Server {
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Tag {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Components {
    #[serde(default)]
    pub schemas: IndexMap<String, Schema>,
    #[serde(default, rename = "securitySchemes")]
    pub security_schemes: IndexMap<String, SecurityScheme>,
}

#[derive(Debug, Deserialize)]
pub struct SecurityScheme {
    #[serde(rename = "type")]
    pub scheme_type: String,
    #[serde(default)]
    pub scheme: Option<String>,
    #[serde(default, rename = "bearerFormat")]
    pub bearer_format: Option<String>,
    #[serde(default, rename = "in")]
    pub location: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct PathItem {
    /// Reference Objects at this location are parsed ONLY so lowering can
    /// fail loudly — resolving them is not supported yet, and deserializing
    /// a $ref into an empty object silently erases endpoints/types.
    #[serde(default, rename = "$ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub get: Option<Operation>,
    #[serde(default)]
    pub put: Option<Operation>,
    #[serde(default)]
    pub post: Option<Operation>,
    #[serde(default)]
    pub delete: Option<Operation>,
    #[serde(default)]
    pub patch: Option<Operation>,
    /// Parameters shared by every operation on this path.
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    /// Parsed ONLY for the input-surface audit: these verbs are not
    /// generated yet, and silent endpoint loss is never acceptable.
    #[serde(default)]
    pub head: Option<Operation>,
    #[serde(default)]
    pub options: Option<Operation>,
    #[serde(default)]
    pub trace: Option<Operation>,
}

impl PathItem {
    /// Iterate defined operations as (http method, operation).
    pub fn operations(&self) -> impl Iterator<Item = (&'static str, &Operation)> {
        [
            ("get", &self.get),
            ("put", &self.put),
            ("post", &self.post),
            ("delete", &self.delete),
            ("patch", &self.patch),
        ]
        .into_iter()
        .filter_map(|(m, op)| op.as_ref().map(|o| (m, o)))
    }
}

#[derive(Debug, Deserialize)]
pub struct Operation {
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Required for path operations; absent on webhook deliveries.
    #[serde(default, rename = "operationId")]
    pub operation_id: Option<String>,
    #[serde(default)]
    pub parameters: Vec<Parameter>,
    #[serde(default, rename = "requestBody")]
    pub request_body: Option<RequestBody>,
    #[serde(default)]
    pub responses: IndexMap<String, Response>,
    #[serde(default)]
    pub deprecated: bool,
    /// Present-but-empty (`security: []`) means explicitly public and is
    /// distinct from omitted (inherit the root requirement).
    #[serde(default)]
    pub security: Option<Vec<IndexMap<String, Vec<String>>>>,
}

#[derive(Debug, Deserialize)]
pub struct Parameter {
    #[serde(default, rename = "$ref")]
    pub reference: Option<String>,
    pub name: String,
    #[serde(rename = "in")]
    pub location: ParamLocation,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub schema: Option<Schema>,
    /// OpenAPI serialization strategy. Parsed so lowering can REJECT
    /// combinations the runtimes don't faithfully implement — silently
    /// emitting a plausible-but-wrong request is unacceptable.
    #[serde(default)]
    pub style: Option<String>,
    #[serde(default)]
    pub explode: Option<bool>,
    #[serde(default, rename = "allowReserved")]
    pub allow_reserved: bool,
    /// Content-based parameter encoding (mutually exclusive with `schema`).
    #[serde(default)]
    pub content: Option<serde_yaml::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamLocation {
    Path,
    Query,
    Header,
    Cookie,
}

#[derive(Debug, Deserialize)]
pub struct RequestBody {
    /// Reference Objects at this location are parsed ONLY so lowering can
    /// fail loudly — resolving them is not supported yet, and deserializing
    /// a $ref into an empty object silently erases endpoints/types.
    #[serde(default, rename = "$ref")]
    pub reference: Option<String>,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub content: IndexMap<String, MediaType>,
}

#[derive(Debug, Deserialize)]
pub struct Response {
    /// Reference Objects at this location are parsed ONLY so lowering can
    /// fail loudly — resolving them is not supported yet, and deserializing
    /// a $ref into an empty object silently erases endpoints/types.
    #[serde(default, rename = "$ref")]
    pub reference: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub content: IndexMap<String, MediaType>,
}

#[derive(Debug, Deserialize)]
pub struct MediaType {
    #[serde(default)]
    pub schema: Option<Schema>,
}

/// `type:` in 3.1 may be a single string or an array (e.g. `[string, "null"]`).
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum TypeOrTypes {
    One(String),
    Many(Vec<String>),
}

/// `additionalProperties:` may be a bool or a schema.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum AdditionalProperties {
    Bool(bool),
    Schema(Box<Schema>),
}

#[derive(Debug, Clone, Deserialize)]
pub struct Discriminator {
    #[serde(rename = "propertyName")]
    pub property_name: String,
    #[serde(default)]
    pub mapping: IndexMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Schema {
    #[serde(default, rename = "$ref")]
    pub reference: Option<String>,
    #[serde(default, rename = "type")]
    pub schema_type: Option<TypeOrTypes>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub properties: IndexMap<String, Schema>,
    #[serde(default)]
    pub required: Vec<String>,
    #[serde(default)]
    pub items: Option<Box<Schema>>,
    #[serde(default, rename = "additionalProperties")]
    pub additional_properties: Option<AdditionalProperties>,
    #[serde(default, rename = "oneOf")]
    pub one_of: Vec<Schema>,
    #[serde(default, rename = "anyOf")]
    pub any_of: Vec<Schema>,
    #[serde(default, rename = "allOf")]
    pub all_of: Vec<Schema>,
    #[serde(default)]
    pub discriminator: Option<Discriminator>,
    #[serde(default, rename = "enum")]
    pub enum_values: Vec<Value>,
    /// OpenAPI 3.0-style nullability (3.1 uses `type: [T, "null"]`).
    #[serde(default)]
    pub nullable: Option<bool>,
    #[serde(default, rename = "readOnly")]
    pub read_only: Option<bool>,
    #[serde(default, rename = "writeOnly")]
    pub write_only: Option<bool>,
    #[serde(default)]
    pub example: Option<Value>,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub deprecated: Option<bool>,
}

impl Schema {
    pub fn is_read_only(&self) -> bool {
        self.read_only.unwrap_or(false)
    }

    pub fn is_write_only(&self) -> bool {
        self.write_only.unwrap_or(false)
    }

    /// The declared primary type, ignoring "null" entries (3.1 nullability).
    pub fn primary_type(&self) -> Option<&str> {
        match &self.schema_type {
            Some(TypeOrTypes::One(t)) => Some(t.as_str()),
            Some(TypeOrTypes::Many(ts)) => ts.iter().map(String::as_str).find(|t| *t != "null"),
            None => None,
        }
    }

    /// True when nullability is declared either 3.0-style or 3.1-style.
    pub fn is_nullable(&self) -> bool {
        if self.nullable == Some(true) {
            return true;
        }
        matches!(&self.schema_type, Some(TypeOrTypes::Many(ts)) if ts.iter().any(|t| t == "null"))
    }
}

pub fn parse(source: &str) -> anyhow::Result<Spec> {
    Ok(serde_yaml::from_str(source)?)
}
