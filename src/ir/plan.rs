//! Body plan: how a request body is presented as individual inputs.
//!
//! Surfaces that take flags rather than documents (the CLI) need every
//! scalar leaf of a request body reachable without hand-writing JSON. The
//! plan is a pure function over the IR — no vocabulary from any particular
//! spec — plus a handful of options a project may set:
//!
//! - **N1 path join.** An input's name is the kebab-case of its wire-name
//!   path segments joined with `-` (`spec.adapter.mcp.url` → `spec-adapter-mcp-url`).
//! - **N2 envelope elision.** Segments listed in [`PlanOptions::elide`]
//!   (`metadata`, `spec` by default) are dropped wherever they occur.
//! - **N3 top-level union collapse.** A discriminated union on the body
//!   root or directly under an envelope drops its own segment; the arm
//!   field is the prefix (`--mcp-url`). Deeper unions keep their segment.
//!   The union field itself becomes the tag input (`--adapter mcp`).
//! - **N4 duplicate collapse.** A segment repeating or extending the one
//!   before it collapses onto it (`credentials.apiKey.apiKey` → `--api-key`,
//!   `modelConfig.modelId` renamed `model` → `--model-id`).
//! - **N5 singular repeatables.** Maps and lists take the singular of their
//!   last segment (`labels` → `--label`, `overlays` → `--overlay`).
//! - **N6 enum short forms.** Values sharing a `SCREAMING_` prefix are
//!   offered as the lowercase remainder (`weighted`); `*_UNSPECIFIED` is
//!   never accepted.
//! - **N7 collisions are errors**, resolved by a path-keyed rename.
//!
//! Every struct node is both a document input (a YAML/JSON value for the
//! whole subtree) and a set of leaves, so nothing is lost when a shape is
//! too deep to flatten.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Result};
use heck::ToKebabCase;
use indexmap::IndexMap;

use super::*;

/// Inputs every body-taking command owns regardless of the schema.
pub const RESERVED_FLAGS: &[&str] = &[
    "display", "file", "f", "dry-run", "strict", "help", "version",
];

/// Nesting depth (struct levels) beyond which a subtree is a document only.
const MAX_DEPTH: usize = 8;

/// Accepted enum wire values and their `(short, wire)` forms.
pub type EnumTable = (Vec<String>, Option<Vec<(String, String)>>);
/// [`EnumTable`] for a type that may not be an enum at all.
type MaybeEnumTable = (Option<Vec<String>>, Option<Vec<(String, String)>>);

#[derive(Debug, Clone)]
pub struct PlanOptions {
    /// Envelope segments dropped from input names (N2).
    pub elide: Vec<String>,
    /// Singular overrides keyed by the field's WIRE name (N5), e.g.
    /// `"memoryCascade" = "memory-cascade"`.
    pub singular: IndexMap<String, String>,
    /// Segment renames keyed by dotted wire path, e.g.
    /// `"spec.modelConfig" = "model"`. An empty replacement elides the
    /// segment. Every rename must match a path in the body (an unused
    /// rename is an error: it would silently rot with the spec).
    pub rename: IndexMap<String, String>,
    /// Offer enum short forms (N6).
    pub enum_short_forms: bool,
    /// Additional names the surface reserves for this operation (path and
    /// query parameter flags, for instance).
    pub reserved: Vec<String>,
}

impl Default for PlanOptions {
    fn default() -> Self {
        Self {
            elide: vec!["metadata".into(), "spec".into()],
            singular: IndexMap::new(),
            rename: IndexMap::new(),
            enum_short_forms: true,
            reserved: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputKind {
    /// One scalar value (string, number, bool, timestamp, enum, literal).
    Leaf(Ty),
    /// `map<string, scalar>`: repeatable `KEY=VALUE` (value type given).
    KvMap(Ty),
    /// `list<scalar>`: repeatable single values (item type given).
    ScalarList(Ty),
    /// A struct subtree: one document (YAML/JSON) for the whole node.
    Doc(Ty),
    /// An untyped value or map of non-scalars: a document, or repeatable
    /// `KEY=VALUE` (string) / `KEY:=JSON` (typed) entries.
    EntryDoc(Ty),
    /// `list<struct>` with nested structure: repeatable documents.
    DocList(Ty),
    /// `list<struct>` whose items are all scalars: repeatable
    /// `key=value,key=value` shorthand (or a document).
    ShorthandList {
        item: Ty,
        fields: Vec<ShorthandField>,
    },
    /// The tag of a discriminated union (`--adapter mcp`); also accepts a
    /// document for the whole union value.
    UnionTag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShorthandField {
    pub wire_name: String,
    /// Kebab-case key used in the shorthand.
    pub key: String,
    pub ty: Ty,
    pub required: bool,
    pub enum_values: Option<Vec<String>>,
    pub enum_short: Option<Vec<(String, String)>>,
}

/// Which arm of which union an input belongs to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArmRef {
    pub union: usize,
    pub tag: String,
}

#[derive(Debug, Clone)]
pub struct Input {
    /// Kebab-case input name (the flag, without dashes).
    pub flag: String,
    /// Wire path from the body root.
    pub path: Vec<String>,
    pub kind: InputKind,
    /// Required given the body is sent: the field and every ancestor are
    /// required (arms count as present once their union is selected).
    pub required: bool,
    pub description: Option<String>,
    /// Help grouping: empty for common inputs, `<union> = <tag>` inside an
    /// arm.
    pub category: String,
    /// Innermost union arm containing this input.
    pub arm: Option<ArmRef>,
    /// For [`InputKind::UnionTag`]: index into [`BodyPlan::unions`].
    pub union: Option<usize>,
    /// Accepted wire values for enum leaves / scalar lists / union tags.
    pub enum_values: Option<Vec<String>>,
    /// `(short, wire)` pairs accepted in place of the wire value.
    pub enum_short: Option<Vec<(String, String)>>,
    /// For shorthand lists whose item is `{name|key, value}`: the two
    /// field wire names, so `NAME=VALUE` is accepted as one item.
    pub pair_form: Option<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ArmPlan {
    pub tag: String,
    /// The arm's struct type name.
    pub variant: String,
    /// Field wire names found in this arm and no other: presence of any
    /// selects the arm (fields shared between arms identify nothing).
    pub keys: Vec<String>,
    /// Required struct-typed fields initialised to `{}` when the arm is
    /// selected explicitly with none of its inputs set.
    pub init_objects: Vec<String>,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct UnionPlan {
    /// The tag input's name.
    pub flag: String,
    /// Wire path of the union value (empty for a whole-body union).
    pub path: Vec<String>,
    /// Wire property holding the tag.
    pub discriminator: String,
    pub arms: Vec<ArmPlan>,
    pub required: bool,
    /// Arm key sets are pairwise disjoint and non-empty, so the tag can be
    /// inferred from which arm's inputs were supplied.
    pub inferable: bool,
    /// The enclosing arm when this union is nested inside another.
    pub parent_arm: Option<ArmRef>,
}

#[derive(Debug, Clone, Default)]
pub struct BodyPlan {
    /// Every input in walk order: a document precedes the leaves it
    /// contains, unions precede their arms.
    pub inputs: Vec<Input>,
    /// Outermost first.
    pub unions: Vec<UnionPlan>,
    /// The assembled value IS the request body (an `Operation::whole_body`),
    /// rather than the fields of a params object.
    pub whole_body: bool,
}

impl BodyPlan {
    /// Inputs that read a single string value that may be `-` (stdin).
    pub fn stdin_capable(&self) -> impl Iterator<Item = &Input> {
        self.inputs.iter().filter(|i| {
            matches!(
                i.kind,
                InputKind::Leaf(Ty::String)
                    | InputKind::Leaf(Ty::Bytes)
                    | InputKind::Doc(_)
                    | InputKind::EntryDoc(_)
                    | InputKind::DocList(_)
                    | InputKind::ShorthandList { .. }
                    | InputKind::UnionTag
                    | InputKind::KvMap(_)
            )
        })
    }

    /// Whether an input takes several occurrences.
    pub fn repeatable(kind: &InputKind) -> bool {
        matches!(
            kind,
            InputKind::KvMap(_)
                | InputKind::ScalarList(_)
                | InputKind::EntryDoc(_)
                | InputKind::DocList(_)
                | InputKind::ShorthandList { .. }
        )
    }
}

/// Plan the inputs for an operation's request body; `None` when it has
/// none. Errors name colliding inputs and unused renames.
pub fn body_plan(api: &Api, op: &Operation, opts: &PlanOptions) -> Result<Option<BodyPlan>> {
    if op.body_fields.is_empty() && op.whole_body.is_none() {
        return Ok(None);
    }
    let mut walker = Walker {
        api,
        opts,
        plan: BodyPlan::default(),
        by_flag: BTreeMap::new(),
        used_renames: BTreeSet::new(),
    };
    let reserved: BTreeSet<String> = RESERVED_FLAGS
        .iter()
        .map(|s| s.to_string())
        .chain(opts.reserved.iter().cloned())
        .collect();
    walker.by_flag = reserved
        .iter()
        .map(|r| (r.clone(), FlagOwner::Reserved))
        .collect();
    let mut stack: Vec<String> = Vec::new();
    match &op.whole_body {
        Some(ty) => {
            walker.plan.whole_body = true;
            let resolved = walker.resolve(ty);
            let planned_union = match &resolved {
                Ty::Named(name) => match walker.union_shape(name) {
                    Some(union) => {
                        walker.plan_union(
                            name,
                            union,
                            &[],
                            &[],
                            &[],
                            true,
                            None,
                            None,
                            "",
                            &mut stack,
                            0,
                        )?;
                        true
                    }
                    None => false,
                },
                _ => false,
            };
            if !planned_union {
                walker.push(Input {
                    flag: "body".into(),
                    path: Vec::new(),
                    kind: InputKind::Doc(ty.clone()),
                    required: true,
                    description: Some("The entire request body.".into()),
                    category: String::new(),
                    arm: None,
                    union: None,
                    enum_values: None,
                    enum_short: None,
                    pair_form: None,
                })?;
            }
        }
        None => {
            for field in &op.body_fields {
                walker.walk_field(field, &[], &[], true, None, "", &mut stack, 0)?;
            }
        }
    }
    let unused: Vec<&String> = opts
        .rename
        .keys()
        .filter(|k| !walker.used_renames.contains(*k))
        .collect();
    if !unused.is_empty() {
        bail!(
            "[lang.cli.body.rename] {} match{} no path in the request body of {}",
            unused
                .iter()
                .map(|k| format!("{k:?}"))
                .collect::<Vec<_>>()
                .join(", "),
            if unused.len() == 1 { "es" } else { "" },
            op.id
        );
    }
    Ok(Some(walker.plan))
}

enum FlagOwner {
    Reserved,
    Path(Vec<String>),
}

struct Walker<'a> {
    api: &'a Api,
    opts: &'a PlanOptions,
    plan: BodyPlan,
    by_flag: BTreeMap<String, FlagOwner>,
    used_renames: BTreeSet<String>,
}

impl<'a> Walker<'a> {
    /// Resolve aliases to the underlying type (named structs/unions/enums
    /// stay named).
    fn resolve(&self, ty: &Ty) -> Ty {
        let mut current = ty.clone();
        for _ in 0..8 {
            match &current {
                Ty::Named(n) => match self.api.types.get(n).map(|d| &d.shape) {
                    Some(Shape::Alias(inner)) => current = inner.clone(),
                    _ => return current,
                },
                _ => return current,
            }
        }
        current
    }

    fn shape(&self, name: &str) -> Option<&'a Shape> {
        self.api.types.get(name).map(|d| &d.shape)
    }

    /// A discriminated union whose arms are all tagged named structs.
    fn union_shape(&self, name: &str) -> Option<&'a UnionShape> {
        match self.shape(name)? {
            Shape::Union(u) => {
                u.discriminator.as_ref()?;
                let all_struct_arms = u.variants.iter().all(|v| {
                    v.tag.is_some()
                        && matches!(
                            &self.resolve(&v.ty),
                            Ty::Named(n) if matches!(self.shape(n), Some(Shape::Struct(_)))
                        )
                });
                if all_struct_arms && !u.variants.is_empty() {
                    Some(u)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn is_scalar(&self, ty: &Ty) -> bool {
        match self.resolve(ty) {
            Ty::String
            | Ty::Bool
            | Ty::Int32
            | Ty::Int64
            | Ty::Float
            | Ty::Double
            | Ty::Timestamp
            | Ty::Bytes
            | Ty::Literal(_) => true,
            Ty::Named(n) => matches!(self.shape(&n), Some(Shape::Enum(_))),
            _ => false,
        }
    }

    fn is_struct(&self, ty: &Ty) -> Option<(String, &'a StructShape)> {
        match self.resolve(ty) {
            Ty::Named(n) => match self.shape(&n) {
                Some(Shape::Struct(st)) => Some((n, st)),
                _ => None,
            },
            _ => None,
        }
    }

    /// Accepted enum values and short forms for a scalar type, if it is an
    /// enum or literal.
    fn enum_table(&self, ty: &Ty) -> MaybeEnumTable {
        let values = match self.resolve(ty) {
            Ty::Literal(v) => vec![v],
            Ty::Named(n) => match self.shape(&n) {
                Some(Shape::Enum(e)) => e.values.clone(),
                _ => return (None, None),
            },
            _ => return (None, None),
        };
        let (values, short) = enum_forms(&values, self.opts.enum_short_forms);
        (Some(values), short)
    }

    /// The name segment a field contributes, after renames and elision.
    fn segment(&mut self, path: &[String], wire: &str, repeatable: bool) -> Option<String> {
        let dotted = path.join(".");
        if let Some(replacement) = self.opts.rename.get(&dotted) {
            self.used_renames.insert(dotted);
            return if replacement.is_empty() {
                None
            } else {
                Some(replacement.clone())
            };
        }
        if self.opts.elide.iter().any(|e| e == wire) {
            return None;
        }
        if repeatable {
            if let Some(s) = self.opts.singular.get(wire) {
                return Some(s.clone());
            }
            return Some(singularize(&wire.to_kebab_case()));
        }
        Some(wire.to_kebab_case())
    }

    fn push(&mut self, input: Input) -> Result<()> {
        match self.by_flag.get(&input.flag) {
            Some(FlagOwner::Reserved) => bail!(
                "input --{} (from {}) collides with a reserved flag name; rename it in [lang.cli.body.rename]",
                input.flag,
                dotted(&input.path)
            ),
            Some(FlagOwner::Path(existing)) if *existing == input.path => {
                // The same wire path reached through two union arms can use
                // one flag only when both arms agree on how that flag is
                // parsed. Silently keeping the first arm's type would make
                // the other arm schema-dependent on declaration order.
                let prior = self
                    .plan
                    .inputs
                    .iter()
                    .find(|prior| prior.flag == input.flag && prior.path == input.path)
                    .expect("registered input has a plan entry");
                if prior.kind != input.kind
                    || prior.enum_values != input.enum_values
                    || prior.enum_short != input.enum_short
                    || prior.pair_form != input.pair_form
                {
                    bail!(
                        "input --{} reaches {} through multiple union arms with incompatible types ({:?} and {:?}); use the union document input instead",
                        input.flag,
                        dotted(&input.path),
                        prior.kind,
                        input.kind
                    );
                }
                return Ok(());
            }
            Some(FlagOwner::Path(existing)) => bail!(
                "input --{} would be produced by both {} and {}; rename one of them in [lang.cli.body.rename]",
                input.flag,
                dotted(existing),
                dotted(&input.path)
            ),
            None => {}
        }
        self.by_flag
            .insert(input.flag.clone(), FlagOwner::Path(input.path.clone()));
        self.plan.inputs.push(input);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_field(
        &mut self,
        field: &Field,
        parent_path: &[String],
        parent_segs: &[String],
        chain: bool,
        arm: Option<&ArmRef>,
        category: &str,
        stack: &mut Vec<String>,
        depth: usize,
    ) -> Result<()> {
        let mut path = parent_path.to_vec();
        path.push(field.wire_name.clone());
        let kind = self.classify(&field.ty, stack);
        let repeatable = BodyPlan::repeatable(&kind);
        let seg = self.segment(&path, &field.wire_name, repeatable);
        let elided = seg.is_none();
        // N4 also collapses a singularised child onto its parent's name
        // (`headers.headers` -> `--header`, not `--headers-header`).
        let segs = if repeatable && parent_segs.last() == Some(&field.wire_name.to_kebab_case()) {
            let mut segs = parent_segs.to_vec();
            if let Some(seg) = seg {
                *segs.last_mut().expect("non-empty") = seg;
            }
            segs
        } else {
            push_seg(parent_segs, seg)
        };
        let required = chain && field.required;
        self.walk_ty(
            &field.ty,
            kind,
            path,
            parent_segs,
            segs,
            elided,
            required,
            field.description.clone(),
            arm,
            category,
            stack,
            depth,
        )
    }

    /// Classify a type into the input kind its node takes (independent of
    /// naming). Structs and unions are classified by their walk.
    fn classify(&self, ty: &Ty, stack: &[String]) -> InputKind {
        match self.resolve(ty) {
            Ty::String
            | Ty::Bool
            | Ty::Int32
            | Ty::Int64
            | Ty::Float
            | Ty::Double
            | Ty::Timestamp
            | Ty::Bytes
            | Ty::Literal(_) => InputKind::Leaf(self.resolve(ty)),
            Ty::Json => InputKind::EntryDoc(Ty::Json),
            Ty::Map(inner) => {
                if self.is_scalar(&inner) {
                    InputKind::KvMap(self.resolve(&inner))
                } else {
                    InputKind::EntryDoc(ty.clone())
                }
            }
            Ty::List(inner) => {
                if self.is_scalar(&inner) {
                    InputKind::ScalarList(self.resolve(&inner))
                } else if let Some((name, st)) = self.is_struct(&inner) {
                    if !stack.contains(&name) && self.is_flat(st) {
                        InputKind::ShorthandList {
                            item: inner.as_ref().clone(),
                            fields: self.shorthand_fields(st),
                        }
                    } else {
                        InputKind::DocList(inner.as_ref().clone())
                    }
                } else {
                    InputKind::DocList(inner.as_ref().clone())
                }
            }
            Ty::Named(n) => match self.shape(&n) {
                Some(Shape::Enum(_)) => InputKind::Leaf(Ty::Named(n)),
                Some(Shape::Struct(st)) => {
                    if st.fields.is_empty() {
                        match &st.additional {
                            Some(a) if self.is_scalar(a) => InputKind::KvMap(self.resolve(a)),
                            _ => InputKind::EntryDoc(ty.clone()),
                        }
                    } else {
                        InputKind::Doc(ty.clone())
                    }
                }
                Some(Shape::Union(_)) if self.union_shape(&n).is_some() => InputKind::UnionTag,
                _ => InputKind::Doc(ty.clone()),
            },
        }
    }

    fn is_flat(&self, st: &StructShape) -> bool {
        let mut any = false;
        for f in st.input_fields() {
            any = true;
            if !self.is_scalar(&f.ty) {
                return false;
            }
        }
        any && st.additional.is_none()
    }

    fn shorthand_fields(&self, st: &StructShape) -> Vec<ShorthandField> {
        st.input_fields()
            .map(|f| {
                let (enum_values, enum_short) = self.enum_table(&f.ty);
                ShorthandField {
                    wire_name: f.wire_name.clone(),
                    key: f.wire_name.to_kebab_case(),
                    ty: self.resolve(&f.ty),
                    required: f.required,
                    enum_values,
                    enum_short,
                }
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn walk_ty(
        &mut self,
        ty: &Ty,
        kind: InputKind,
        path: Vec<String>,
        parent_segs: &[String],
        segs: Vec<String>,
        elided: bool,
        required: bool,
        description: Option<String>,
        arm: Option<&ArmRef>,
        category: &str,
        stack: &mut Vec<String>,
        depth: usize,
    ) -> Result<()> {
        let flag = if segs.is_empty() {
            // Only an elided field at the body root can land here; keep the
            // wire name so the input stays addressable.
            path.last().map(|s| s.to_kebab_case()).unwrap_or_default()
        } else {
            segs.join("-")
        };
        let mut input = Input {
            flag,
            path: path.clone(),
            kind: kind.clone(),
            required,
            description,
            category: category.to_string(),
            arm: arm.cloned(),
            union: None,
            enum_values: None,
            enum_short: None,
            pair_form: None,
        };
        match &kind {
            InputKind::Leaf(leaf) => {
                let (values, short) = self.enum_table(leaf);
                input.enum_values = values;
                input.enum_short = short;
                self.push(input)
            }
            InputKind::ScalarList(item) | InputKind::KvMap(item) => {
                let (values, short) = self.enum_table(item);
                input.enum_values = values;
                input.enum_short = short;
                self.push(input)
            }
            InputKind::ShorthandList { fields, .. } => {
                input.pair_form = pair_form(fields);
                self.push(input)
            }
            InputKind::EntryDoc(_) | InputKind::DocList(_) => self.push(input),
            InputKind::Doc(_) => {
                let expanded = self
                    .is_struct(ty)
                    .filter(|(name, _)| !stack.contains(name) && depth < MAX_DEPTH);
                // A child named like its parent (`apiKey.apiKey`) collapses
                // onto the parent's name (N4); the leaf is what that name
                // means, so the parent forgoes its document input.
                let shadowed = expanded.as_ref().is_some_and(|(_, st)| {
                    st.input_fields().any(|f| {
                        segs.last()
                            .is_some_and(|last| *last == f.wire_name.to_kebab_case())
                    })
                });
                // An elided struct below the root (`defaultVariation.spec`)
                // has no name of its own: its parent's document covers it.
                let nameless = elided && !parent_segs.is_empty();
                if !shadowed && !nameless {
                    self.push(input)?;
                }
                if let Some((name, st)) = expanded {
                    {
                        stack.push(name);
                        for f in st.input_fields() {
                            self.walk_field(
                                f,
                                &path,
                                &segs,
                                required,
                                arm,
                                category,
                                stack,
                                depth + 1,
                            )?;
                        }
                        stack.pop();
                    }
                }
                Ok(())
            }
            InputKind::UnionTag => {
                let Ty::Named(name) = self.resolve(ty) else {
                    unreachable!("union tags are named");
                };
                let union = self.union_shape(&name).expect("classified as a union");
                self.plan_union(
                    &name,
                    union,
                    &path,
                    parent_segs,
                    &segs,
                    required,
                    input.description.clone(),
                    arm,
                    category,
                    stack,
                    depth,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_union(
        &mut self,
        name: &str,
        union: &'a UnionShape,
        path: &[String],
        parent_segs: &[String],
        segs: &[String],
        required: bool,
        description: Option<String>,
        parent_arm: Option<&ArmRef>,
        category: &str,
        stack: &mut Vec<String>,
        depth: usize,
    ) -> Result<()> {
        let discriminator = union
            .discriminator
            .as_ref()
            .map(|d| d.property.clone())
            .expect("discriminated");
        let flag = if segs.is_empty() {
            discriminator.to_kebab_case()
        } else {
            segs.join("-")
        };
        // N3: a union on the body root or directly under an envelope drops
        // its own segment; deeper unions keep it.
        let arm_segs: Vec<String> = if path.len() <= 2 {
            parent_segs.to_vec()
        } else {
            segs.to_vec()
        };
        let index = self.plan.unions.len();
        self.plan.unions.push(UnionPlan {
            flag: flag.clone(),
            path: path.to_vec(),
            discriminator: discriminator.clone(),
            arms: Vec::new(),
            required,
            inferable: false,
            parent_arm: parent_arm.cloned(),
        });
        let tags: Vec<String> = union
            .variants
            .iter()
            .filter_map(|v| v.tag.clone())
            .collect();
        let (_, short) = enum_forms(&tags, true);
        self.push(Input {
            flag: flag.clone(),
            path: path.to_vec(),
            kind: InputKind::UnionTag,
            required,
            description,
            category: category.to_string(),
            arm: parent_arm.cloned(),
            union: Some(index),
            enum_values: Some(tags),
            enum_short: short,
            pair_form: None,
        })?;
        let mut arms = Vec::new();
        let mut key_sets: Vec<BTreeSet<String>> = Vec::new();
        for variant in &union.variants {
            let tag = variant.tag.clone().expect("tagged");
            let Ty::Named(variant_name) = self.resolve(&variant.ty) else {
                unreachable!("struct arms");
            };
            let Some(Shape::Struct(st)) = self.shape(&variant_name) else {
                unreachable!("struct arms");
            };
            let keys: Vec<String> = st
                .input_fields()
                .filter(|f| f.wire_name != discriminator)
                .map(|f| f.wire_name.clone())
                .collect();
            let init_objects: Vec<String> = st
                .input_fields()
                .filter(|f| {
                    f.wire_name != discriminator && f.required && self.is_struct(&f.ty).is_some()
                })
                .map(|f| f.wire_name.clone())
                .collect();
            key_sets.push(keys.iter().cloned().collect());
            arms.push(ArmPlan {
                tag: tag.clone(),
                variant: variant_name.clone(),
                keys,
                init_objects,
                description: self
                    .api
                    .types
                    .get(&variant_name)
                    .and_then(|d| d.description.clone()),
            });
            let arm_ref = ArmRef {
                union: index,
                tag: tag.clone(),
            };
            let arm_category = format!("{flag} = {}", tag.to_kebab_case());
            stack.push(variant_name.clone());
            for f in st.input_fields() {
                if f.wire_name == discriminator {
                    continue;
                }
                self.walk_field(
                    f,
                    path,
                    &arm_segs,
                    required,
                    Some(&arm_ref),
                    &arm_category,
                    stack,
                    depth + 1,
                )?;
            }
            stack.pop();
        }
        // Only keys no other arm carries can identify an arm.
        for (i, arm) in arms.iter_mut().enumerate() {
            arm.keys.retain(|k| {
                key_sets
                    .iter()
                    .enumerate()
                    .all(|(j, other)| i == j || !other.contains(k))
            });
        }
        let inferable = arms.iter().all(|a| !a.keys.is_empty());
        let plan = &mut self.plan.unions[index];
        plan.arms = arms;
        plan.inferable = inferable;
        let _ = name;
        Ok(())
    }
}

fn dotted(path: &[String]) -> String {
    if path.is_empty() {
        "the body".to_string()
    } else {
        path.join(".")
    }
}

fn push_seg(parent: &[String], seg: Option<String>) -> Vec<String> {
    let mut segs = parent.to_vec();
    if let Some(seg) = seg {
        // N4: a segment repeating the previous one collapses onto it, as
        // does one that merely extends it (`model` + `model-id`).
        match segs.last() {
            Some(last) if *last == seg => {}
            Some(last) if seg.starts_with(&format!("{last}-")) => {
                *segs.last_mut().expect("non-empty") = seg;
            }
            _ => segs.push(seg),
        }
    }
    segs
}

/// `{name|key, value}` string items take a `NAME=VALUE` pair form.
fn pair_form(fields: &[ShorthandField]) -> Option<(String, String)> {
    if fields.len() != 2 {
        return None;
    }
    let key = fields
        .iter()
        .find(|f| matches!(f.wire_name.as_str(), "name" | "key") && f.ty == Ty::String)?;
    let value = fields
        .iter()
        .find(|f| f.wire_name == "value" && f.ty == Ty::String)?;
    Some((key.wire_name.clone(), value.wire_name.clone()))
}

/// Singular of a kebab-case name: only the last word changes.
pub fn singularize(kebab: &str) -> String {
    let (head, last) = match kebab.rfind('-') {
        Some(i) => (&kebab[..=i], &kebab[i + 1..]),
        None => ("", kebab),
    };
    let singular = if last.len() > 3 && last.ends_with("ies") {
        format!("{}y", &last[..last.len() - 3])
    } else if last.ends_with("ss") || last.len() < 2 {
        last.to_string()
    } else if ["ses", "xes", "shes", "ches", "zes"]
        .iter()
        .any(|suffix| last.ends_with(suffix))
    {
        last[..last.len() - 2].to_string()
    } else if let Some(stem) = last.strip_suffix('s') {
        stem.to_string()
    } else {
        last.to_string()
    };
    format!("{head}{singular}")
}

/// Deduplicated enum values with `*_UNSPECIFIED` dropped (when anything
/// else remains), and their short forms: the lowercase kebab remainder
/// after the longest shared `_`-token prefix. None when short forms are
/// disabled or would not be unique.
pub fn enum_forms(
    values: &[String],
    short_forms: bool,
) -> (Vec<String>, Option<Vec<(String, String)>>) {
    let mut seen = BTreeSet::new();
    let mut deduped: Vec<String> = values
        .iter()
        .filter(|v| seen.insert((*v).clone()))
        .cloned()
        .collect();
    let specified: Vec<String> = deduped
        .iter()
        .filter(|v| !v.ends_with("_UNSPECIFIED"))
        .cloned()
        .collect();
    if !specified.is_empty() {
        deduped = specified;
    }
    if !short_forms || deduped.is_empty() {
        return (deduped, None);
    }
    let tokens: Vec<Vec<&str>> = deduped.iter().map(|v| v.split('_').collect()).collect();
    let min_len = tokens.iter().map(|t| t.len()).min().unwrap_or(0);
    let mut prefix = 0;
    while prefix + 1 < min_len && tokens.iter().all(|t| t[prefix] == tokens[0][prefix]) {
        prefix += 1;
    }
    let shorts: Vec<(String, String)> = tokens
        .iter()
        .zip(&deduped)
        .map(|(t, full)| (t[prefix..].join("_").to_kebab_case(), full.clone()))
        .collect();
    let unique: BTreeSet<&str> = shorts.iter().map(|(s, _)| s.as_str()).collect();
    if unique.len() != shorts.len() || shorts.iter().any(|(s, _)| s.is_empty()) {
        return (deduped, None);
    }
    (deduped, Some(shorts))
}
