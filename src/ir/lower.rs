//! OpenAPI -> IR lowering. All spec semantics are resolved here so backends
//! never see a $ref, an allOf, or an anonymous schema.

use anyhow::{bail, Context, Result};
use heck::{ToSnakeCase, ToUpperCamelCase};
use indexmap::IndexMap;

use crate::ir::*;
use crate::openapi::{self, AdditionalProperties, ParamLocation, Spec};

pub fn lower(spec: &Spec) -> Result<Api> {
    let mut lowerer = Lowerer::new(spec);
    lowerer.run()?;
    Ok(Api {
        name: client_name(&spec.info.title),
        version: spec.info.version.clone(),
        description: spec.info.description.clone(),
        base_url: spec
            .servers
            .first()
            .map(|s| s.url.clone())
            .unwrap_or_default(),
        auth: detect_auth(spec)?,
        types: lowerer.types,
        resources: lowerer.resources,
        webhooks: lowerer.webhooks,
        client_params: Vec::new(),
        sse_skip_events: Vec::new(),
        api_key_env: format!("{}_API_KEY", client_name(&spec.info.title).to_uppercase()),
        webhook_env: format!(
            "{}_WEBHOOK_SECRET",
            client_name(&spec.info.title).to_uppercase()
        ),
        max_retries: 0,
        auth_optional: false,
    })
}

fn client_name(title: &str) -> String {
    title.trim_end_matches(" API").trim().to_upper_camel_case()
}

/// Resolve authentication from the effective Security REQUIREMENT, never
/// from the inventory of declared schemes: unused component definitions have
/// no effect, and component order never selects behavior. Requirements the
/// api-wide auth model cannot represent (per-operation overrides, mixed
/// public/private, OR alternatives, AND compounds) fail generation with the
/// operation id rather than silently choosing a scheme.
fn detect_auth(spec: &Spec) -> anyhow::Result<Auth> {
    let resolve = |requirement: &indexmap::IndexMap<String, Vec<String>>| -> anyhow::Result<Auth> {
        anyhow::ensure!(
            requirement.len() == 1,
            "compound (AND) security requirements are not supported yet: {:?}",
            requirement.keys().collect::<Vec<_>>()
        );
        let (name, _scopes) = requirement.iter().next().expect("len checked");
        let scheme = spec.components.security_schemes.get(name).ok_or_else(|| {
            anyhow::anyhow!("security requirement references undefined scheme {name}")
        })?;
        match (scheme.scheme_type.as_str(), scheme.scheme.as_deref()) {
            ("http", Some("bearer")) => Ok(Auth::Bearer),
            ("apiKey", _) if scheme.location.as_deref() == Some("header") => scheme
                .name
                .clone()
                .map(Auth::ApiKeyHeader)
                .ok_or_else(|| anyhow::anyhow!("apiKey scheme {name} has no header name")),
            _ => anyhow::bail!(
                "security scheme {name} ({}) is not supported yet",
                scheme.scheme_type
            ),
        }
    };

    let root = match spec.security.as_slice() {
        [] => None,
        [only] => Some(resolve(only)?),
        alternatives => anyhow::bail!(
            "alternative (OR) security requirements are not supported yet: \
             {} root alternatives declared",
            alternatives.len()
        ),
    };

    // Per-operation requirements: allowed only when semantically identical
    // to the root (same single scheme). Everything else needs per-operation
    // auth state the IR does not carry yet — fail, do not collapse.
    for (path, item) in &spec.paths {
        for (_method, op) in item.operations() {
            let Some(op_security) = &op.security else {
                continue;
            };
            let id = op.operation_id.as_deref().unwrap_or(path);
            match (op_security.as_slice(), &root) {
                ([], _) => anyhow::bail!(
                    "{id}: operation-level `security: []` (explicitly public) is not \
                     supported yet in an authenticated API — per-operation auth is \
                     required to represent it"
                ),
                ([only], Some(root_auth)) => {
                    let op_auth = resolve(only)?;
                    anyhow::ensure!(
                        &op_auth == root_auth,
                        "{id}: operation-level security overrides the root requirement, \
                         which is not supported yet"
                    );
                }
                (_, None) => anyhow::bail!(
                    "{id}: operation-level security in an API with no root requirement \
                     is not supported yet"
                ),
                (alternatives, _) => anyhow::bail!(
                    "{id}: {} alternative security requirements are not supported yet",
                    alternatives.len()
                ),
            }
        }
    }

    Ok(root.unwrap_or(Auth::None))
}

struct Lowerer<'a> {
    spec: &'a Spec,
    types: IndexMap<String, TypeDecl>,
    resources: Vec<Resource>,
    webhooks: Vec<Webhook>,
}

impl<'a> Lowerer<'a> {
    fn new(spec: &'a Spec) -> Self {
        Self {
            spec,
            types: IndexMap::new(),
            resources: Vec::new(),
            webhooks: Vec::new(),
        }
    }

    fn run(&mut self) -> Result<()> {
        // Input-surface audit: every operation in the document must either
        // enter the IR or fail generation. Silent endpoint loss is never an
        // acceptable fallback.
        for (path, item) in &self.spec.paths {
            // Reference Objects at unsupported locations would deserialize
            // into EMPTY structures — erasing endpoints, bodies, and
            // responses invisibly. Fail with the pointer instead.
            if let Some(reference) = &item.reference {
                bail!("{path}: reusable Path Item reference {reference} is not supported yet");
            }
            for (method, op) in item.operations() {
                if let Some(body) = &op.request_body {
                    if let Some(reference) = &body.reference {
                        bail!("{path} {method}: requestBody reference {reference} is not supported yet");
                    }
                }
                for (status, response) in &op.responses {
                    if let Some(reference) = &response.reference {
                        bail!("{path} {method} response {status}: reference {reference} is not supported yet");
                    }
                }
                for p in &op.parameters {
                    if let Some(reference) = &p.reference {
                        bail!(
                            "{path} {method}: parameter reference {reference} is not supported yet"
                        );
                    }
                }
            }
            for p in &item.parameters {
                if let Some(reference) = &p.reference {
                    bail!("{path}: shared parameter reference {reference} is not supported yet");
                }
            }
            for (verb, op) in [
                ("HEAD", &item.head),
                ("OPTIONS", &item.options),
                ("TRACE", &item.trace),
            ] {
                if let Some(op) = op {
                    bail!(
                        "{path}: {verb} operation {} is not supported yet — generation \
                         would silently drop this endpoint",
                        op.operation_id.as_deref().unwrap_or("<no operationId>")
                    );
                }
            }
        }
        for (name, schema) in &self.spec.components.schemas {
            let decl = self
                .lower_decl(name, schema)
                .with_context(|| format!("lowering schema {name}"))?;
            self.types.insert(name.clone(), decl);
        }
        self.lower_operations()?;
        self.lower_webhooks()?;
        Ok(())
    }

    /// OpenAPI 3.1 `webhooks:` — each entry's POST body is the delivery
    /// payload the SDK must verify and unwrap.
    fn lower_webhooks(&mut self) -> Result<()> {
        for (name, item) in &self.spec.webhooks {
            let Some(op) = &item.post else { continue };
            let payload = {
                let schema = op
                    .request_body
                    .as_ref()
                    .and_then(|b| b.content.get("application/json"))
                    .and_then(|m| m.schema.clone())
                    .unwrap_or_default();
                let hint = format!("{}Payload", name.to_upper_camel_case());
                self.lower_ty(&schema, &hint)
                    .with_context(|| format!("lowering webhook {name}"))?
            };
            let discriminator_field = self.webhook_discriminator(&payload);
            self.webhooks.push(Webhook {
                name: name.clone(),
                summary: op.summary.clone(),
                description: op.description.clone(),
                payload,
                discriminator_field,
            });
        }
        Ok(())
    }

    /// A required string field named `type` on the payload envelope carries
    /// the event name, letting SDKs discriminate the event union on it.
    fn webhook_discriminator(&self, payload: &Ty) -> Option<String> {
        let Ty::Named(name) = payload else {
            return None;
        };
        let Shape::Struct(s) = &self.types.get(name)?.shape else {
            return None;
        };
        s.fields
            .iter()
            .find(|f| f.wire_name == "type" && f.required && f.ty == Ty::String)
            .map(|f| f.wire_name.clone())
    }

    // ---- schemas -------------------------------------------------------

    fn resolve_ref(&self, reference: &str) -> Result<(&'a str, &'a openapi::Schema)> {
        let name = reference.rsplit('/').next().context("empty $ref")?;
        let schema = self
            .spec
            .components
            .schemas
            .get(name)
            .with_context(|| format!("unresolved $ref {reference}"))?;
        // IndexMap keys outlive the borrow; fetch the owned key's str.
        let (key, _) = self
            .spec
            .components
            .schemas
            .get_key_value(name)
            .expect("just found");
        Ok((key.as_str(), schema))
    }

    fn lower_decl(&mut self, name: &str, schema: &openapi::Schema) -> Result<TypeDecl> {
        let shape = if !schema.one_of.is_empty() || !schema.any_of.is_empty() {
            Shape::Union(self.lower_union(name, schema)?)
        } else if !schema.enum_values.is_empty() {
            Shape::Enum(lower_enum(schema))
        } else if !schema.all_of.is_empty() {
            self.lower_all_of(name, schema)?
        } else if schema.primary_type() == Some("object") || !schema.properties.is_empty() {
            self.lower_object_shape(name, schema)?
        } else {
            Shape::Alias(self.lower_ty(schema, name)?)
        };
        Ok(TypeDecl {
            name: name.to_string(),
            description: schema.description.clone(),
            shape,
        })
    }

    fn lower_union(&mut self, name: &str, schema: &openapi::Schema) -> Result<UnionShape> {
        let members = if schema.one_of.is_empty() {
            &schema.any_of
        } else {
            &schema.one_of
        };
        // Invert the discriminator mapping: ref -> tag.
        let tag_by_ref: IndexMap<&str, &str> = schema
            .discriminator
            .iter()
            .flat_map(|d| d.mapping.iter())
            .map(|(tag, reference)| (reference.as_str(), tag.as_str()))
            .collect();
        let mut variants = Vec::new();
        for (i, member) in members.iter().enumerate() {
            let tag = member
                .reference
                .as_deref()
                .and_then(|r| tag_by_ref.get(r).map(|t| t.to_string()));
            let hint = format!("{name}Variant{i}");
            variants.push(UnionVariant {
                tag,
                ty: self.lower_ty(member, &hint)?,
            });
        }
        Ok(UnionShape {
            discriminator: schema.discriminator.as_ref().map(|d| Discriminator {
                property: d.property_name.clone(),
            }),
            variants,
        })
    }

    fn lower_all_of(&mut self, name: &str, schema: &openapi::Schema) -> Result<Shape> {
        // Attribute-only wrapper around a single $ref -> alias.
        if schema.all_of.len() == 1
            && schema.all_of[0].reference.is_some()
            && schema.properties.is_empty()
        {
            return Ok(Shape::Alias(self.lower_ty(&schema.all_of[0], name)?));
        }
        // Real composition: merge every member's fields, then own properties.
        let mut merged = StructShape::default();
        for member in &schema.all_of {
            let resolved = match &member.reference {
                Some(r) => self.resolve_ref(r)?.1,
                None => member,
            };
            let part = self.lower_object_fields(name, resolved)?;
            merged.fields.extend(part.fields);
            merged.additional = merged.additional.or(part.additional);
        }
        let own = self.lower_object_fields(name, schema)?;
        merged.fields.extend(own.fields);
        merged.additional = merged.additional.or(own.additional);
        Ok(Shape::Struct(merged))
    }

    fn lower_object_shape(&mut self, name: &str, schema: &openapi::Schema) -> Result<Shape> {
        Ok(Shape::Struct(self.lower_object_fields(name, schema)?))
    }

    fn lower_object_fields(
        &mut self,
        owner: &str,
        schema: &openapi::Schema,
    ) -> Result<StructShape> {
        let mut fields = Vec::new();
        for (prop_name, prop) in &schema.properties {
            let hint = format!("{owner}{}", prop_name.to_upper_camel_case());
            let read_only = field_read_only(prop);
            let write_only = field_write_only(prop);
            if read_only && write_only {
                anyhow::bail!(
                    "{owner}.{prop_name}: a property cannot be both readOnly and writeOnly"
                );
            }
            fields.push(Field {
                wire_name: prop_name.clone(),
                ty: self.lower_ty(prop, &hint)?,
                required: schema.required.iter().any(|r| r == prop_name),
                nullable: prop.is_nullable(),
                read_only,
                write_only,
                description: field_description(prop),
            });
        }
        let additional = match &schema.additional_properties {
            Some(AdditionalProperties::Schema(s)) => {
                Some(self.lower_ty(s, &format!("{owner}Value"))?)
            }
            Some(AdditionalProperties::Bool(true)) => Some(Ty::Json),
            _ => None,
        };
        Ok(StructShape { fields, additional })
    }

    /// Lower a schema in type position. Anonymous compound schemas are lifted
    /// into `types` under a deterministic name derived from `hint`.
    fn lower_ty(&mut self, schema: &openapi::Schema, hint: &str) -> Result<Ty> {
        if let Some(reference) = &schema.reference {
            let (name, _) = self.resolve_ref(reference)?;
            return Ok(Ty::Named(name.to_string()));
        }
        // Attribute-only allOf wrapper in type position.
        if schema.all_of.len() == 1 && schema.all_of[0].reference.is_some() {
            return self.lower_ty(&schema.all_of[0], hint);
        }
        if !schema.all_of.is_empty() {
            let shape = self.lower_all_of(hint, schema)?;
            return Ok(self.lift(hint, schema.description.clone(), shape));
        }
        if !schema.one_of.is_empty() || !schema.any_of.is_empty() {
            let union = self.lower_union(hint, schema)?;
            return Ok(self.lift(hint, schema.description.clone(), Shape::Union(union)));
        }
        if !schema.enum_values.is_empty() {
            if let [value] = schema.enum_values.as_slice() {
                if let Some(s) = value.as_str() {
                    return Ok(Ty::Literal(s.to_string()));
                }
            }
            let shape = Shape::Enum(lower_enum(schema));
            return Ok(self.lift(hint, schema.description.clone(), shape));
        }
        match schema.primary_type() {
            Some("string") => Ok(match schema.format.as_deref() {
                Some("date-time") => Ty::Timestamp,
                Some("byte") => Ty::Bytes,
                _ => Ty::String,
            }),
            Some("integer") => Ok(match schema.format.as_deref() {
                Some("int64") | Some("uint64") => Ty::Int64,
                _ => Ty::Int32,
            }),
            Some("number") => Ok(match schema.format.as_deref() {
                Some("float") => Ty::Float,
                _ => Ty::Double,
            }),
            Some("boolean") => Ok(Ty::Bool),
            Some("array") => {
                let items = schema.items.as_deref().cloned().unwrap_or_default();
                Ok(Ty::List(Box::new(self.lower_ty(&items, hint)?)))
            }
            Some("object") | None => {
                if !schema.properties.is_empty() {
                    let shape = self.lower_object_shape(hint, schema)?;
                    return Ok(self.lift(hint, schema.description.clone(), shape));
                }
                match &schema.additional_properties {
                    Some(AdditionalProperties::Schema(s)) => {
                        let value = self.lower_ty(s, &format!("{hint}Value"))?;
                        Ok(Ty::Map(Box::new(value)))
                    }
                    _ if schema.primary_type() == Some("object") => Ok(Ty::Map(Box::new(Ty::Json))),
                    _ => Ok(Ty::Json),
                }
            }
            Some(other) => bail!("unsupported schema type `{other}`"),
        }
    }

    /// Insert a lifted anonymous type, disambiguating on collision.
    fn lift(&mut self, hint: &str, description: Option<String>, shape: Shape) -> Ty {
        let mut name = hint.to_upper_camel_case();
        let mut n = 2;
        while self.types.contains_key(&name) || self.spec.components.schemas.contains_key(&name) {
            name = format!("{}{}", hint.to_upper_camel_case(), n);
            n += 1;
        }
        self.types.insert(
            name.clone(),
            TypeDecl {
                name: name.clone(),
                description,
                shape,
            },
        );
        Ty::Named(name)
    }

    // ---- operations ----------------------------------------------------

    fn lower_operations(&mut self) -> Result<()> {
        for (path, item) in &self.spec.paths {
            for (http_method, op) in item.operations() {
                let operation = self
                    .lower_operation(path, http_method, op, &item.parameters)
                    .with_context(|| format!("lowering {} {}", http_method, path))?;
                let resource_name = resource_name(op);
                let resource = match self.resources.iter_mut().find(|r| r.name == resource_name) {
                    Some(r) => r,
                    None => {
                        let description = self
                            .spec
                            .tags
                            .iter()
                            .find(|t| t.name.to_snake_case() == resource_name)
                            .and_then(|t| t.description.clone());
                        self.resources.push(Resource {
                            name: resource_name.clone(),
                            ident: resource_name.clone(),
                            parent: None,
                            description,
                            operations: Vec::new(),
                        });
                        self.resources.last_mut().expect("just pushed")
                    }
                };
                resource.operations.push(operation);
            }
        }
        Ok(())
    }

    fn lower_operation(
        &mut self,
        path: &str,
        http_method: &str,
        op: &openapi::Operation,
        shared_params: &[openapi::Parameter],
    ) -> Result<Operation> {
        let resource = resource_name(op);
        let operation_id = op
            .operation_id
            .clone()
            .with_context(|| "path operation missing operationId")?;
        let name = method_name(&operation_id, &resource);

        // Merge per OpenAPI: an operation-level parameter with the same
        // (location, name) identity REPLACES the shared Path Item parameter
        // in place (both never coexist); operation-only parameters append.
        // Duplicates WITHIN either list are invalid input, not a tie to
        // break silently. Header names compare case-insensitively.
        let identity = |p: &crate::openapi::Parameter| -> (ParamLocation, String) {
            let name = if p.location == ParamLocation::Header {
                p.name.to_ascii_lowercase()
            } else {
                p.name.clone()
            };
            (p.location, name)
        };
        let mut merged: Vec<&crate::openapi::Parameter> = Vec::new();
        let mut seen_shared = std::collections::BTreeMap::new();
        for p in shared_params.iter() {
            anyhow::ensure!(
                seen_shared.insert(identity(p), merged.len()).is_none(),
                "{operation_id}: duplicate shared parameter {} in {:?}",
                p.name,
                p.location
            );
            merged.push(p);
        }
        let mut seen_op = std::collections::BTreeSet::new();
        for p in op.parameters.iter() {
            anyhow::ensure!(
                seen_op.insert(identity(p)),
                "{operation_id}: duplicate operation parameter {} in {:?}",
                p.name,
                p.location
            );
            match seen_shared.get(&identity(p)) {
                Some(&slot) => merged[slot] = p,
                None => merged.push(p),
            }
        }

        let mut path_params = Vec::new();
        let mut query_params = Vec::new();
        for p in merged {
            // Faithfulness gate: reject parameters the runtimes cannot
            // serialize per contract rather than silently emitting a
            // plausible-but-wrong request.
            anyhow::ensure!(
                p.content.is_none(),
                "{operation_id}: parameter {} uses content-based encoding, which is not supported yet",
                p.name
            );
            anyhow::ensure!(
                !p.allow_reserved,
                "{operation_id}: parameter {} sets allowReserved, which is not supported yet",
                p.name
            );
            match p.location {
                ParamLocation::Path => {
                    // Implemented policy: simple style, no explode.
                    anyhow::ensure!(
                        p.style.as_deref().is_none_or(|s| s == "simple") && p.explode != Some(true),
                        "{operation_id}: path parameter {} declares style/explode the \
                         generated SDKs do not implement (only simple, explode=false)",
                        p.name
                    );
                }
                ParamLocation::Query => {
                    // Implemented policy: form style with explode=true
                    // (repeated keys for arrays) — the OpenAPI default.
                    anyhow::ensure!(
                        p.style.as_deref().is_none_or(|s| s == "form") && p.explode != Some(false),
                        "{operation_id}: query parameter {} declares style/explode the \
                         generated SDKs do not implement (only form, explode=true)",
                        p.name
                    );
                }
                ParamLocation::Header | ParamLocation::Cookie => {
                    // No typed header/cookie parameter generation yet: a
                    // required one would make every generated call unable to
                    // satisfy the operation, so generation must fail loudly
                    // instead of erasing the requirement.
                    anyhow::bail!(
                        "{operation_id}: {} parameter {} is not supported yet — \
                         generated SDKs would silently drop a contract requirement",
                        if p.location == ParamLocation::Header {
                            "header"
                        } else {
                            "cookie"
                        },
                        p.name
                    );
                }
            }
            let schema = p.schema.clone().unwrap_or_default();
            let hint = format!(
                "{}{}",
                operation_id.to_upper_camel_case(),
                p.name.to_upper_camel_case()
            );
            let param = Param {
                wire_name: p.name.clone(),
                ty: self.lower_ty(&schema, &hint)?,
                required: p.required,
                description: p.description.clone(),
            };
            match p.location {
                ParamLocation::Path => path_params.push(param),
                ParamLocation::Query => query_params.push(param),
                _ => unreachable!("header/cookie rejected above"),
            }
        }

        let positionals = extract_positional(&mut path_params, &resource);
        let (body_fields, whole_body, update_mask) = self.lower_body(op)?;
        let response = self.lower_response(op)?;
        let pagination = self.detect_pagination(&query_params, &response);

        Ok(Operation {
            id: operation_id,
            name,
            http_method: match http_method {
                "get" => HttpMethod::Get,
                "post" => HttpMethod::Post,
                "put" => HttpMethod::Put,
                "patch" => HttpMethod::Patch,
                "delete" => HttpMethod::Delete,
                other => bail!("unsupported http method {other}"),
            },
            path: path.to_string(),
            summary: op.summary.clone(),
            description: op.description.clone(),
            deprecated: op.deprecated,
            positionals,
            path_params,
            query_params,
            body_fields,
            whole_body,
            update_mask,
            response,
            pagination,
        })
    }

    /// Lower the JSON request body. Object bodies flatten into fields
    /// (excluding readOnly fields — the server populates those from the
    /// path); union/array/scalar bodies can't flatten and become a
    /// `whole_body` type instead.
    fn lower_body(
        &mut self,
        op: &openapi::Operation,
    ) -> Result<(Vec<Field>, Option<Ty>, Option<String>)> {
        let Some(body) = &op.request_body else {
            return Ok((Vec::new(), None, None));
        };
        let Some(media) = body.content.get("application/json") else {
            return Ok((Vec::new(), None, None));
        };
        let Some(schema) = &media.schema else {
            return Ok((Vec::new(), None, None));
        };
        let (owner, resolved): (String, &openapi::Schema) = match &schema.reference {
            Some(r) => {
                let (name, s) = self.resolve_ref(r)?;
                (name.to_string(), s)
            }
            None => (
                format!(
                    "{}Body",
                    op.operation_id
                        .clone()
                        .unwrap_or_default()
                        .to_upper_camel_case()
                ),
                schema,
            ),
        };
        let resolved = resolved.clone();
        let flattenable = resolved.one_of.is_empty()
            && resolved.any_of.is_empty()
            && !matches!(
                resolved.primary_type(),
                Some("array") | Some("string") | Some("integer") | Some("number") | Some("boolean")
            );
        if !flattenable {
            let ty = self.lower_ty(schema, &owner)?;
            return Ok((Vec::new(), Some(ty), None));
        }
        // A `format: field-mask` string is the partial-update mask: recorded
        // by wire name so body-assembling surfaces can derive it from the
        // paths they set.
        let update_mask = resolved
            .properties
            .iter()
            .find(|(_, p)| {
                p.format.as_deref() == Some("field-mask")
                    && p.primary_type() == Some("string")
                    && !field_read_only(p)
            })
            .map(|(name, _)| name.clone());
        let all = self.lower_object_fields(&owner, &resolved)?;
        Ok((
            all.fields.into_iter().filter(|f| !f.read_only).collect(),
            None,
            update_mask,
        ))
    }

    fn lower_response(&mut self, op: &openapi::Operation) -> Result<ResponseKind> {
        let success = op
            .responses
            .iter()
            .find(|(status, _)| status.starts_with('2'))
            .map(|(_, r)| r);
        let Some(response) = success else {
            return Ok(ResponseKind::Empty);
        };
        if let Some(media) = response.content.get("text/event-stream") {
            let schema = media.schema.clone().unwrap_or_default();
            let hint = format!(
                "{}Event",
                op.operation_id
                    .clone()
                    .unwrap_or_default()
                    .to_upper_camel_case()
            );
            return Ok(ResponseKind::Sse(self.lower_ty(&schema, &hint)?));
        }
        if let Some(media) = response.content.get("application/json") {
            if let Some(schema) = &media.schema {
                let hint = format!(
                    "{}Response",
                    op.operation_id
                        .clone()
                        .unwrap_or_default()
                        .to_upper_camel_case()
                );
                let ty = self.lower_ty(&schema.clone(), &hint)?;
                if self.is_empty_type(&ty) {
                    return Ok(ResponseKind::Empty);
                }
                return Ok(ResponseKind::Json(ty));
            }
        }
        Ok(ResponseKind::Empty)
    }

    /// google.protobuf.Empty and friends: a named struct with no fields.
    fn is_empty_type(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Named(name) => match self.types.get(name).map(|d| &d.shape) {
                Some(Shape::Struct(s)) => s.fields.is_empty() && s.additional.is_none(),
                _ => false,
            },
            _ => false,
        }
    }

    /// Cursor pagination: a `cursor` query param plus a response struct with
    /// an items array and a reachable next-cursor string.
    fn detect_pagination(
        &self,
        query_params: &[Param],
        response: &ResponseKind,
    ) -> Option<Pagination> {
        let cursor = query_params
            .iter()
            .find(|p| p.wire_name == "cursor" || p.wire_name == "page_token")?;
        let ResponseKind::Json(Ty::Named(response_name)) = response else {
            return None;
        };
        let Shape::Struct(s) = &self.types.get(response_name)?.shape else {
            return None;
        };
        let items = s.fields.iter().find(|f| matches!(f.ty, Ty::List(_)))?;
        let Ty::List(item_ty) = &items.ty else {
            unreachable!()
        };
        let next_cursor_path = self.find_next_cursor(s)?;
        Some(Pagination {
            item_ty: (**item_ty).clone(),
            items_field: items.wire_name.clone(),
            cursor_param: cursor.wire_name.clone(),
            next_cursor_path,
        })
    }

    fn find_next_cursor(&self, response: &StructShape) -> Option<String> {
        const CURSOR_NAMES: &[&str] = &[
            "nextCursor",
            "next_cursor",
            "nextPageToken",
            "next_page_token",
        ];
        // Directly on the response…
        for f in &response.fields {
            if CURSOR_NAMES.contains(&f.wire_name.as_str()) && f.ty == Ty::String {
                return Some(f.wire_name.clone());
            }
        }
        // …or one level down inside a pagination envelope.
        for f in &response.fields {
            if let Ty::Named(inner) = &f.ty {
                if let Some(Shape::Struct(s)) = self.types.get(inner).map(|d| &d.shape) {
                    for g in &s.fields {
                        if CURSOR_NAMES.contains(&g.wire_name.as_str()) && g.ty == Ty::String {
                            return Some(format!("{}.{}", f.wire_name, g.wire_name));
                        }
                    }
                }
            }
        }
        None
    }
}

fn lower_enum(schema: &openapi::Schema) -> EnumShape {
    // Real-world specs contain duplicate enum members; dedupe preserving
    // order so backends never emit colliding constants.
    let mut seen = std::collections::HashSet::new();
    EnumShape {
        values: schema
            .enum_values
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .filter(|v| seen.insert(v.clone()))
            .collect(),
    }
}

fn field_read_only(schema: &openapi::Schema) -> bool {
    schema.is_read_only() || (schema.all_of.len() == 1 && schema.all_of[0].is_read_only())
}

fn field_write_only(schema: &openapi::Schema) -> bool {
    schema.is_write_only() || (schema.all_of.len() == 1 && schema.all_of[0].is_write_only())
}

fn field_description(schema: &openapi::Schema) -> Option<String> {
    schema
        .description
        .clone()
        .or_else(|| schema.all_of.first().and_then(|s| s.description.clone()))
}

/// Resource name: the last tag that is not a `FooService` tag, else the first
/// tag with its Service suffix stripped, else "api". snake_cased.
fn resource_name(op: &openapi::Operation) -> String {
    op.tags
        .iter()
        .rev()
        .find(|t| !t.ends_with("Service"))
        .map(String::as_str)
        .or_else(|| op.tags.first().map(|t| t.trim_end_matches("Service")))
        .unwrap_or("api")
        .to_snake_case()
}

/// Split a PascalCase identifier into lowercase words.
fn pascal_words(s: &str) -> Vec<String> {
    s.to_snake_case().split('_').map(str::to_string).collect()
}

fn singular(word: &str) -> String {
    if word.ends_with("ics") {
        // Uncountable: analytics, metrics, statistics.
        word.to_string()
    } else if let Some(stem) = word.strip_suffix("ies") {
        format!("{stem}y")
    } else if word.ends_with("ses") || word.ends_with("xes") {
        word[..word.len() - 2].to_string()
    } else if word.len() > 1 && word.ends_with('s') {
        word[..word.len() - 1].to_string()
    } else {
        word.to_string()
    }
}

/// Derive the SDK method name from an operationId like
/// `ObjectiveEventStreamsService_StreamObjectiveEvents`, resource "objectives"
/// -> "stream_events". `Get` maps to `retrieve` (house style).
fn method_name(operation_id: &str, resource: &str) -> String {
    let verb_phrase = operation_id
        .split_once('_')
        .map(|(_, rest)| rest)
        .unwrap_or(operation_id);
    let words = pascal_words(verb_phrase);
    let resource_words: Vec<String> = resource.split('_').map(singular).collect();
    let mut out: Vec<String> = Vec::new();
    for (i, word) in words.iter().enumerate() {
        if i == 0 {
            out.push(match word.as_str() {
                "get" => "retrieve".to_string(),
                other => other.to_string(),
            });
            continue;
        }
        if resource_words.contains(&singular(word)) {
            continue;
        }
        out.push(word.clone());
    }
    out.join("_")
}

/// Pull the resource's own ID out of the path params (house style: it is the
/// positional argument). Matches `id` or `<singularResource>Id`.
fn extract_positional(path_params: &mut Vec<Param>, resource: &str) -> Vec<Param> {
    let own_id: String = resource
        .split('_')
        .map(singular)
        .collect::<Vec<_>>()
        .join("")
        + "id";
    let Some(idx) = path_params.iter().rposition(|p| {
        let lower = p.wire_name.to_lowercase().replace('_', "");
        lower == "id" || lower == own_id
    }) else {
        return Vec::new();
    };
    vec![path_params.remove(idx)]
}
