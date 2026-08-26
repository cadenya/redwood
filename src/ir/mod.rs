//! The normalized intermediate representation.
//!
//! Every backend (TypeScript, Go, Python, Ruby, CLI, api.md) consumes this
//! tree and nothing else. All OpenAPI quirks — $refs, allOf wrappers, inline
//! anonymous schemas, 3.0 vs 3.1 nullability — are resolved by the time an
//! `Api` exists. Exhaustive matches over `Shape`/`Ty` are what force each
//! backend to handle every edge case.

pub mod lower;

use indexmap::IndexMap;

#[derive(Debug)]
pub struct Api {
    /// Client name, e.g. "Cadenya" (derived from info.title).
    pub name: String,
    pub version: String,
    pub description: Option<String>,
    pub base_url: String,
    pub auth: Auth,
    /// Named types in stable (spec) order. Inline schemas are lifted here
    /// with synthesized, deterministic names during lowering.
    pub types: IndexMap<String, TypeDecl>,
    pub resources: Vec<Resource>,
    pub webhooks: Vec<Webhook>,
    /// SSE event NAMES (the `event:` field) that are transport
    /// housekeeping (e.g. "ping", "open"): streams skip them without
    /// decoding, though their `id:` fields still advance the resume
    /// checkpoint. Configured via [sse] skip_events.
    pub sse_skip_events: Vec<String>,
    /// Params (usually prominent path params like workspaceId) that may be
    /// defaulted at client construction or via an env var. Config-driven.
    pub client_params: Vec<ClientParam>,
    /// Env var consulted for the API key.
    pub api_key_env: String,
    /// Env var consulted for the webhook signing key.
    pub webhook_env: String,
    /// Default automatic retries for retryable failures. 0 disables —
    /// the right default when mutating endpoints are not idempotent.
    pub max_retries: u32,
    /// Some endpoints are public: constructing without a credential is
    /// legal, and the auth header is omitted when no key is set.
    pub auth_optional: bool,
}

#[derive(Debug, Clone)]
pub struct ClientParam {
    /// Wire name as it appears in params, e.g. "workspaceId".
    pub wire_name: String,
    /// Env var consulted when neither call nor client provides it.
    pub env_var: String,
}

/// An outbound webhook event the API delivers (OpenAPI 3.1 `webhooks:`).
/// Verification follows the Standard Webhooks spec.
#[derive(Debug)]
pub struct Webhook {
    /// Event name as delivered, e.g. "objective_event.user_message".
    pub name: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub payload: Ty,
    /// The payload field carrying the event name, when the envelope has one
    /// (a required string field the SDK can discriminate the union on).
    pub discriminator_field: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Auth {
    /// `Authorization: Bearer <token>`
    Bearer,
    /// API key in a named header.
    ApiKeyHeader(String),
    None,
}

#[derive(Debug)]
pub struct TypeDecl {
    pub name: String,
    pub description: Option<String>,
    pub shape: Shape,
}

#[derive(Debug)]
pub enum Shape {
    Struct(StructShape),
    Enum(EnumShape),
    Union(UnionShape),
    /// A named alias for another type (e.g. an allOf wrapper around one $ref).
    Alias(Ty),
}

#[derive(Debug, Default)]
pub struct StructShape {
    pub fields: Vec<Field>,
    /// A typed map in addition to (or instead of) fixed fields.
    pub additional: Option<Ty>,
}

impl StructShape {
    /// The request-direction view: `readOnly` fields are server-owned and
    /// never accepted as input. Surviving fields keep their declared
    /// requiredness (a required readOnly field must not poison input).
    pub fn input_fields(&self) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(|f| !f.read_only)
    }

    /// The response-direction view: `writeOnly` fields are input secrets and
    /// never promised (or exposed) in output.
    pub fn output_fields(&self) -> impl Iterator<Item = &Field> {
        self.fields.iter().filter(|f| !f.write_only)
    }
}

#[derive(Debug)]
pub struct Field {
    /// Name exactly as it appears on the wire (JSON key).
    pub wire_name: String,
    pub ty: Ty,
    pub required: bool,
    pub nullable: bool,
    pub read_only: bool,
    pub write_only: bool,
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct EnumShape {
    pub values: Vec<String>,
}

#[derive(Debug)]
pub struct UnionShape {
    /// Present for discriminated (`oneOf` + `discriminator`) unions.
    pub discriminator: Option<Discriminator>,
    pub variants: Vec<UnionVariant>,
}

#[derive(Debug)]
pub struct Discriminator {
    /// The wire property holding the tag, e.g. "type".
    pub property: String,
}

#[derive(Debug)]
pub struct UnionVariant {
    /// Discriminator tag value for this variant, when discriminated.
    pub tag: Option<String>,
    pub ty: Ty,
}

/// A reference to a type. Optionality lives on `Field`/`Param`, not here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    String,
    Bool,
    Int32,
    Int64,
    Float,
    Double,
    /// RFC 3339 timestamp (string format: date-time).
    Timestamp,
    /// Base64 bytes (string format: byte).
    Bytes,
    /// Untyped JSON value.
    Json,
    /// A single-value string enum, e.g. a discriminator tag.
    Literal(String),
    Named(String),
    List(Box<Ty>),
    Map(Box<Ty>),
}

impl Api {
    /// Names of every named type reachable from a request position
    /// (parameters, flattened body fields, whole bodies), transitively.
    pub fn request_reachable(&self) -> std::collections::BTreeSet<String> {
        use std::collections::BTreeSet;
        fn walk(api: &Api, ty: &Ty, seen: &mut BTreeSet<String>) {
            match ty {
                Ty::Named(n) => {
                    if !seen.insert(n.clone()) {
                        return;
                    }
                    match api.types.get(n).map(|d| &d.shape) {
                        Some(Shape::Struct(s)) => {
                            for f in &s.fields {
                                walk(api, &f.ty, seen);
                            }
                            if let Some(a) = &s.additional {
                                walk(api, a, seen);
                            }
                        }
                        Some(Shape::Union(u)) => {
                            for v in &u.variants {
                                walk(api, &v.ty, seen);
                            }
                        }
                        Some(Shape::Alias(inner)) => {
                            let inner = inner.clone();
                            walk(api, &inner, seen);
                        }
                        _ => {}
                    }
                }
                Ty::List(inner) | Ty::Map(inner) => walk(api, inner, seen),
                _ => {}
            }
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        for resource in &self.resources {
            for op in &resource.operations {
                for p in op.path_params.iter().chain(op.query_params.iter()) {
                    walk(self, &p.ty, &mut seen);
                }
                for f in &op.body_fields {
                    walk(self, &f.ty, &mut seen);
                }
                if let Some(ty) = &op.whole_body {
                    walk(self, ty, &mut seen);
                }
            }
        }
        seen
    }

    /// Named types whose request-direction and response-direction views
    /// differ, directly (a readOnly/writeOnly field of their own) or
    /// transitively (they reference a divergent type). Backends that share
    /// one nominal type between directions must emit a distinct input type
    /// for exactly these.
    pub fn divergent_types(&self) -> std::collections::BTreeSet<String> {
        use std::collections::BTreeSet;
        fn ty_refs(ty: &Ty, out: &mut Vec<String>) {
            match ty {
                Ty::Named(n) => out.push(n.clone()),
                Ty::List(inner) | Ty::Map(inner) => ty_refs(inner, out),
                _ => {}
            }
        }
        let mut divergent: BTreeSet<String> = BTreeSet::new();
        let mut edges: Vec<(String, String)> = Vec::new(); // (referrer, referenced)
        for decl in self.types.values() {
            let mut refs: Vec<String> = Vec::new();
            match &decl.shape {
                Shape::Struct(st) => {
                    if st.fields.iter().any(|f| f.read_only || f.write_only) {
                        divergent.insert(decl.name.clone());
                    }
                    for f in &st.fields {
                        ty_refs(&f.ty, &mut refs);
                    }
                    if let Some(additional) = &st.additional {
                        ty_refs(additional, &mut refs);
                    }
                }
                Shape::Union(u) => {
                    for v in &u.variants {
                        ty_refs(&v.ty, &mut refs);
                    }
                }
                Shape::Alias(ty) => ty_refs(ty, &mut refs),
                Shape::Enum(_) => {}
            }
            for r in refs {
                edges.push((decl.name.clone(), r));
            }
        }
        // Propagate divergence up through referrers to a fixed point.
        loop {
            let before = divergent.len();
            for (referrer, referenced) in &edges {
                if divergent.contains(referenced) {
                    divergent.insert(referrer.clone());
                }
            }
            if divergent.len() == before {
                return divergent;
            }
        }
    }

    /// A whole-body discriminated union viewed as a CHOICE: one arm per
    /// variant, selecting an arm fixes the discriminator tag. Surfaces that
    /// take flags rather than documents (the CLI) expose one input per arm
    /// instead of an opaque JSON body. None when the body is not such a
    /// union (undiscriminated, non-struct arms, or arms whose inputs would
    /// collide by name) — the whole-body document is the only faithful form.
    pub fn body_choices(&self, op: &Operation) -> Option<Vec<BodyChoice>> {
        let Some(Ty::Named(name)) = &op.whole_body else {
            return None;
        };
        let mut current = name.as_str();
        let union = loop {
            match self.types.get(current).map(|d| &d.shape) {
                Some(Shape::Union(u)) => break u,
                Some(Shape::Alias(Ty::Named(next))) => current = next,
                _ => return None,
            }
        };
        let discriminator = union.discriminator.as_ref()?;
        let mut choices = Vec::with_capacity(union.variants.len());
        for variant in &union.variants {
            let tag = variant.tag.clone()?;
            let Ty::Named(variant_name) = &variant.ty else {
                return None;
            };
            let Some(Shape::Struct(st)) = self.types.get(variant_name).map(|d| &d.shape) else {
                return None;
            };
            let payload: Vec<&Field> = st
                .input_fields()
                .filter(|f| f.wire_name != discriminator.property)
                .collect();
            let choice = match payload.as_slice() {
                [field] => BodyChoice {
                    tag: tag.clone(),
                    variant: variant_name.clone(),
                    wire_name: field.wire_name.clone(),
                    ty: field.ty.clone(),
                    description: field.description.clone(),
                    payload_field: Some(field.wire_name.clone()),
                },
                _ => BodyChoice {
                    tag: tag.clone(),
                    variant: variant_name.clone(),
                    wire_name: tag.clone(),
                    ty: variant.ty.clone(),
                    description: self
                        .types
                        .get(variant_name)
                        .and_then(|d| d.description.clone()),
                    payload_field: None,
                },
            };
            choices.push(choice);
        }
        let mut names: Vec<&str> = choices.iter().map(|c| c.wire_name.as_str()).collect();
        names.extend(
            op.path_params
                .iter()
                .chain(&op.query_params)
                .map(|p| p.wire_name.as_str()),
        );
        let unique: std::collections::BTreeSet<&str> = names.iter().copied().collect();
        if unique.len() != names.len() || choices.is_empty() {
            return None;
        }
        Some(choices)
    }
}

/// One arm of a whole-body discriminated union, see [`Api::body_choices`].
#[derive(Debug, Clone)]
pub struct BodyChoice {
    /// Discriminator tag value that selects this arm.
    pub tag: String,
    /// The arm's struct type name.
    pub variant: String,
    /// Input name for the arm: its single payload field's wire name, or the
    /// tag when the arm carries several (or no) fields.
    pub wire_name: String,
    /// Type of the input: the payload field's type, or the arm's struct.
    pub ty: Ty,
    pub description: Option<String>,
    /// Some(field) when the input is that field's value (the request body is
    /// `{tag, field: value}`); None when the input is the whole arm object.
    pub payload_field: Option<String>,
}

#[derive(Debug)]
pub struct Resource {
    /// snake_case accessor name on its owner, e.g. "workspace_secrets", or
    /// the leaf ("variations") when nested under a parent.
    pub name: String,
    /// Stable identity used for TYPE naming (interfaces, params structs,
    /// classes, file names), e.g. "agent_variations". Never changes when the
    /// resource is nested, so generated type names stay unique.
    pub ident: String,
    /// Accessor name of the parent resource when nested (one level deep,
    /// e.g. "agents" for agents.variations). None for top-level resources.
    pub parent: Option<String>,
    pub description: Option<String>,
    pub operations: Vec<Operation>,
}

impl Resource {
    /// Dotted accessor path, e.g. "agents.variations" or "workspaces".
    pub fn path(&self) -> String {
        match &self.parent {
            Some(parent) => format!("{parent}.{}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl HttpMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
        }
    }
}

#[derive(Debug)]
pub struct Operation {
    /// Original operationId, kept for traceability.
    pub id: String,
    /// snake_case method name on the resource, e.g. "list", "stream_events".
    pub name: String,
    pub http_method: HttpMethod,
    /// Path template with `{param}` placeholders.
    pub path: String,
    pub summary: Option<String>,
    pub description: Option<String>,
    pub deprecated: bool,
    /// Path params passed positionally, in path order. Default inference
    /// yields at most the resource's own ID; the `[positional]` config
    /// section sets any subset explicitly (Stainless positional_params).
    pub positionals: Vec<Param>,
    /// Remaining path params, in path order.
    pub path_params: Vec<Param>,
    pub query_params: Vec<Param>,
    /// Body fields flattened into the params object. Empty when no body or
    /// when the body is `whole_body`.
    pub body_fields: Vec<Field>,
    /// A request body that is NOT a flattenable object (e.g. a oneOf union):
    /// the params object carries it under a required `body` member, sent as
    /// the entire request body. Mutually exclusive with `body_fields`.
    pub whole_body: Option<Ty>,
    pub response: ResponseKind,
    pub pagination: Option<Pagination>,
}

impl Operation {
    pub fn has_params(&self) -> bool {
        !self.path_params.is_empty()
            || !self.query_params.is_empty()
            || !self.body_fields.is_empty()
            || self.whole_body.is_some()
    }

    pub fn required_params(&self) -> bool {
        self.path_params.iter().any(|p| p.required)
            || self.query_params.iter().any(|p| p.required)
            || self.body_fields.iter().any(|f| f.required)
            || self.whole_body.is_some()
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    /// Name exactly as it appears on the wire (path segment / query key).
    pub wire_name: String,
    pub ty: Ty,
    pub required: bool,
    pub description: Option<String>,
}

#[derive(Debug)]
pub enum ResponseKind {
    /// JSON body of the given type.
    Json(Ty),
    /// Server-sent events; each event's `data` decodes to the given type.
    Sse(Ty),
    /// No meaningful body (empty object / 204).
    Empty,
}

#[derive(Debug)]
pub struct Pagination {
    /// Element type of the `items` array.
    pub item_ty: Ty,
    /// Wire name of the array field, e.g. "items".
    pub items_field: String,
    /// Wire name of the cursor query param, e.g. "cursor".
    pub cursor_param: String,
    /// Dotted wire path to the next cursor in the response, e.g. "pagination.nextCursor".
    pub next_cursor_path: String,
}
