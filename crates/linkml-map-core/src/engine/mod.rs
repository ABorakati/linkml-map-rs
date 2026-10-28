//! Object transformer engine — single-object, single-threaded port of
//! `linkml_map.transformer.object_transformer.ObjectTransformer`.
//!
//! # Entry point
//! ```ignore
//! let engine = ObjectTransformer::new(spec, source_schema, target_schema);
//! let result: Value = engine.map_object(source_value, Some("Person"))?;
//! ```
//!
//! # Design notes
//! - Both a source and target `&dyn SchemaProvider` are accepted (may be the
//!   same pointer when schemas are identical, or `None` for the target when not
//!   needed).
//! - The expr evaluator (`eval_expr_with_mapping`) is called for every `expr:`
//!   slot.  Bindings are built from the source object's map keys.
//! - Recursion happens via `map_object` — nested class-ranged slots recurse
//!   into the same engine instance.
//! - FK join, stringification, `unit_conversion:`, `class_derivations:`,
//!   `offset:`, `aggregation_operation:` and `pivot_operation:` (melt/unmelt)
//!   ARE ported (`unit_conversion` via the dependency-free [`units`] table).
//!   Aggregation has no Python `ObjectTransformer` reference — its semantics are
//!   defined in [`apply_aggregation`]. Spec→Python/SQL compilation and
//!   JSON-schema target validation are intentionally out of scope.

pub mod coerce;
pub mod content_id;
pub mod lookup_index;
mod merged_row;
#[cfg(test)]
mod merged_row_tests;
pub mod object_index;
mod pivot;
mod populated_from;
mod source_slots;
pub mod units;
pub use coerce::TargetCoercions;
pub use content_id::ContentIds;
pub use lookup_index::{LookupIndex, LookupIndexRef};
pub use object_index::ObjectIndex;
// gate:internal — engine plan table, not part of the binding-consumed surface.
pub use source_slots::SourceSlots;

use indexmap::IndexMap;

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// A `HashMap` for the engine's read-only lookup tables: names looked up per row,
/// from spec and schema text, so a fast non-cryptographic hash.
pub(crate) type FastMap<K, V> = HashMap<K, V, foldhash::fast::RandomState>;

use crate::{
    datamodel::{
        AggregationOperation, AggregationType, AliasedClass, ClassDerivation, CollectionType,
        EnumDerivation, InvalidValueHandlingStrategy, Offset, SerializationSyntaxType,
        SlotDerivation, TransformationSpecification,
    },
    error::{Error, Result},
    expr::{
        Bindings, DeclaredSlots, EvalMode, ExprResult, ParsedExpr, eval_enum_parsed,
        eval_parsed_gated, parse_expr, with_ambiguous_names, with_declared_slots,
    },
    schema::{RangeKind, SchemaProvider, SlotDef},
    value::Value,
};
use coerce::{TargetSlot, coerce_datatype};
use merged_row::MergedRow;

// ── Compiled expr cache ───────────────────────────────────────────────

/// Pre-parsed `expr:` ASTs for an entire [`TransformationSpecification`],
/// built once and reused across every row.
///
/// Each `expr:` string that appears on a class slot derivation or an enum
/// derivation is lexed + parsed a single time into a [`ParsedExpr`]. The engine
/// then evaluates the cached AST per row instead of re-parsing the string,
/// which is the dominant per-row cost on expr-heavy specs.
///
/// `CompiledExprs` is plain data (`Send + Sync`), so a single instance can be
/// shared across worker threads via `Arc`.
#[derive(Debug, Clone, Default)]
pub struct CompiledExprs {
    /// `class_derivation.name` -> `slot_derivation.name` -> (the `expr:` text,
    /// its parse). The text is kept so a lookup can confirm the cached parse is
    /// of the very expression about to be evaluated.
    slot_exprs: FastMap<String, FastMap<String, (String, ParsedExpr)>>,
    /// `enum_derivation map key` → parsed enum-level `expr:`.
    enum_exprs: HashMap<String, ParsedExpr>,
}

impl CompiledExprs {
    /// Parse every `expr:` in `spec` once.
    ///
    /// PARITY: upstream parses an expression when a row first evaluates it, so a spec
    /// with a malformed `expr:` loads, and runs without error when no row reaches it
    /// (zero rows, or no row reaches the slot). A malformed
    /// expression is therefore left out of the cache -- the row path then parses the
    /// text again and reports the parse error on first use, with the text upstream
    /// gives -- instead of failing the whole plan here. Valid expressions stay cached.
    ///
    /// The result is `Ok` for every spec; the `Result` is kept so callers need not change.
    pub fn build(spec: &TransformationSpecification) -> ExprResult<Self> {
        let mut slot_exprs: FastMap<String, FastMap<String, (String, ParsedExpr)>> =
            FastMap::default();
        if let Some(cds) = &spec.class_derivations {
            for cd in cds {
                if let Some(sds) = &cd.slot_derivations {
                    for (_, sd) in sds {
                        if let Some(expr) = &sd.expr
                            && let Ok(parsed) = parse_expr(expr)
                        {
                            slot_exprs
                                .entry(cd.name.clone())
                                .or_default()
                                .insert(sd.name.clone(), (expr.clone(), parsed));
                        }
                    }
                }
            }
        }
        // The derivations the engine runs are the `is_a` / `mixins`-merged ones,
        // and nested ones: an `expr:` inherited from an ancestor, or on a nested
        // class derivation, is cached too. A malformed one is left out, so the
        // row path reports it exactly as it did before it was cached.
        for cd in effective_class_derivations(spec) {
            cache_slot_exprs(&cd, &mut slot_exprs);
        }

        let mut enum_exprs = HashMap::new();
        if let Some(eds) = &spec.enum_derivations {
            for (key, ed) in eds {
                if let Some(expr) = &ed.expr
                    && let Ok(parsed) = parse_expr(expr)
                {
                    enum_exprs.insert(key.clone(), parsed);
                }
            }
        }

        Ok(Self {
            slot_exprs,
            enum_exprs,
        })
    }

    /// The parse of `expr`, the `expr:` of `slot` in class derivation `class`,
    /// if one was cached from that same text.
    fn slot(&self, class: &str, slot: &str, expr: &str) -> Option<&ParsedExpr> {
        let (text, parsed) = self.slot_exprs.get(class)?.get(slot)?;
        (text == expr).then_some(parsed)
    }
}

/// Cache the parse of every slot `expr:` under `cd`, nested derivations
/// included, that is not cached yet and parses.
fn cache_slot_exprs(
    cd: &ClassDerivation,
    out: &mut FastMap<String, FastMap<String, (String, ParsedExpr)>>,
) {
    for (_, sd) in cd.slot_derivations.iter().flatten() {
        if let Some(expr) = &sd.expr
            && !out.get(&cd.name).is_some_and(|m| m.contains_key(&sd.name))
            && let Ok(parsed) = parse_expr(expr)
        {
            out.entry(cd.name.clone())
                .or_default()
                .insert(sd.name.clone(), (expr.clone(), parsed));
        }
        for nested in sd.class_derivations.iter().flatten() {
            cache_slot_exprs(nested, out);
        }
    }
}

/// The top-level class derivations as the engine runs them: each merged with
/// its `is_a` / `mixins` ancestors (upstream `_with_inherited_fields`), or as
/// written when the merge fails (the row path reports that error).
pub(crate) fn effective_class_derivations(
    spec: &TransformationSpecification,
) -> Vec<ClassDerivation> {
    let all: &[ClassDerivation] = spec.class_derivations.as_deref().unwrap_or(&[]);
    all.iter()
        .map(|cd| {
            crate::normalize::inheritance::with_inherited_fields(cd, all)
                .unwrap_or_else(|_| cd.clone())
        })
        .collect()
}

/// Class derivations resolved by source type, built once.
///
/// [`ObjectTransformer::map_object`] finds the derivation for a source type and
/// merges it with its `is_a` / `mixins` ancestors, which clones the whole
/// derivation. This table does that once per source type; a pipeline that
/// builds an engine per row attaches it with
/// [`ObjectTransformer::with_resolved_derivations`]. A source type that does not
/// resolve (no derivation, several, or a broken `is_a` chain) is absent, so the
/// engine resolves it itself and reports the error as before.
#[derive(Debug, Clone, Default)]
pub struct ResolvedDerivations {
    by_source: FastMap<String, ClassDerivation>,
}

impl ResolvedDerivations {
    /// Resolve the derivation of every source type `spec` names.
    pub fn build(spec: &TransformationSpecification) -> Self {
        let mut by_source = FastMap::default();
        for cd in spec.class_derivations.iter().flatten() {
            let source_type = cd.populated_from.as_deref().unwrap_or(&cd.name);
            if !by_source.contains_key(source_type)
                && let Ok(resolved) = resolve_class_derivation(spec, source_type)
            {
                by_source.insert(source_type.to_owned(), resolved);
            }
        }
        Self { by_source }
    }
}

/// Find the unique `ClassDerivation` for `source_type`: see
/// [`ObjectTransformer::get_class_derivation`].
fn resolve_class_derivation(
    spec: &TransformationSpecification,
    source_type: &str,
) -> Result<ClassDerivation> {
    let derivations = match &spec.class_derivations {
        Some(v) => v,
        None => {
            return Err(Error::NoClassDerivation {
                source_type: source_type.to_string(),
                count: 0,
            });
        }
    };

    let matching: Vec<&ClassDerivation> = derivations
        .iter()
        .filter(|cd| {
            cd.populated_from.as_deref() == Some(source_type)
                || (cd.populated_from.is_none() && cd.name == source_type)
        })
        .collect();

    if matching.len() != 1 {
        return Err(Error::NoClassDerivation {
            source_type: source_type.to_string(),
            count: matching.len(),
        });
    }

    // Python: `return self._with_inherited_fields(matching_tgt_class_derivs[0])`.
    crate::normalize::inheritance::with_inherited_fields(matching[0], derivations)
}

/// The restricted-eval [`Bindings`] of one source row, built on first use and
/// shared by every `expr:` slot of that row (see
/// [`ObjectTransformer::eval_expr_for_slot`]). Local to one pass over a row's
/// slot derivations, so the engine itself stays `Send + Sync`.
#[derive(Default)]
struct RowBindings<'m>(
    std::cell::RefCell<Option<Bindings>>,
    /// The row's keys that are not slots of the source type (see
    /// [`ObjectTransformer::check_row_names`]), found once per row.
    std::cell::OnceCell<Vec<String>>,
    /// The slots the source type declares (see [`SourceSlots`]), looked up once per row.
    std::cell::OnceCell<Option<Arc<DeclaredSlots>>>,
    /// The merge of the parent and joined rows, when the row is a nested
    /// derivation's over a join (see [`merged_row`]).
    Option<&'m MergedRow<'m>>,
);

/// Why [`ObjectTransformer::unresolved_join`] cannot resolve a join alias.
enum JoinFailure {
    /// No `LookupIndex`: upstream's `ValueError`.
    NoIndex(Error),
    /// An index without the table: DuckDB's `CatalogException`.
    NoTable(Error),
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Single-object transformer.
///
/// Owns the spec; borrows source and target schema providers.
pub struct ObjectTransformer<'s> {
    spec: Cow<'s, TransformationSpecification>,
    source_schema: Option<&'s dyn SchemaProvider>,
    target_schema: Option<&'s dyn SchemaProvider>,
    /// Optional pre-parsed `expr:` ASTs. When present, the per-slot and
    /// per-enum expr paths evaluate the cached AST instead of re-parsing the
    /// `expr:` string on every call. When `None`, the engine falls back to the
    /// string path (`eval_expr_with_mapping`), preserving existing behaviour.
    compiled: Option<&'s CompiledExprs>,
    /// Optional cross-table lookup index for join resolution.
    ///
    /// When set, `populated_from: "table.field"` and `{table.field}` expr
    /// bindings resolve through this index.  Multi-row aggregation
    /// (`aggregation_operation` + join) uses `lookup_rows` to collect all
    /// matching rows before reducing.  Attach via [`with_lookup_index`].
    lookup_index: Option<Arc<LookupIndex>>,
    /// When true, `expr:` evaluation errors on unbound names / unresolved
    /// `slot()` references instead of silently yielding null (#232). Default
    /// false (lax). Set via [`with_strict_exprs`].
    strict: bool,
    /// When true, the rows of a block were joined up front, as upstream's set-based
    /// join engine does: every join of the block (and of its nested derivations) is
    /// resolved from the primary row into a `MergedRow` before the row is mapped.
    /// Set via [`with_prejoined_joins`].
    prejoined: bool,
    /// When true, `expr:` may use the unrestricted (asteval-style) evaluator:
    /// statement blocks with `src`, and the fallback for an expression the
    /// restricted evaluator rejects. Default false (restricted), as upstream
    /// `unrestricted_eval`. Set via [`with_unrestricted_eval`].
    unrestricted_eval: bool,
    /// Optional shared target-schema lookup table (target slot datatypes).
    /// Built once per plan and borrowed by every per-row engine; see
    /// [`ObjectTransformer::with_target_coercions`].
    target_coercions: Option<&'s TargetCoercions>,
    /// Optional shared table of resolved class derivations; see
    /// [`ObjectTransformer::with_resolved_derivations`].
    resolved_derivations: Option<&'s ResolvedDerivations>,
    /// Lazily built fallback used when no shared table is attached (tests,
    /// `ObjectTransformer::new` callers). Unused without a target schema.
    local_target_coercions: OnceLock<TargetCoercions>,
    /// Optional shared table of the slots each source class declares; see
    /// [`ObjectTransformer::with_source_slots`].
    source_slots: Option<&'s SourceSlots>,
    /// Fallback used when no shared table is attached: the classes resolved so
    /// far. Unused without a source schema; built on first use, so an engine
    /// made per row (as the pipeline does, with a shared table) never pays for it.
    local_source_slots: OnceLock<Mutex<FastMap<String, Option<Arc<DeclaredSlots>>>>>,
    /// Shared content-id synthesiser (built once per plan); when `None` one is
    /// built lazily into `owned_content_ids`. See [`content_id`].
    content_ids: Option<&'s ContentIds>,
    owned_content_ids: content_id::OwnedContentIds,
    /// Emit records whose identifier is neither derived nor minted instead of
    /// raising (`--continue-on-error`). See [`content_id`].
    emit_without_identifiers: bool,
}

impl<'s> ObjectTransformer<'s> {
    /// Create a new transformer.
    ///
    /// `source_schema` is required for range-based coercion and cardinality
    /// decisions.  `target_schema` is optional; when present it is used for
    /// target-slot cardinality decisions.
    pub fn new(
        spec: TransformationSpecification,
        source_schema: Option<&'s dyn SchemaProvider>,
        target_schema: Option<&'s dyn SchemaProvider>,
    ) -> Self {
        Self {
            spec: Cow::Owned(spec),
            source_schema,
            target_schema,
            compiled: None,
            lookup_index: None,
            strict: false,
            prejoined: false,
            unrestricted_eval: false,
            target_coercions: None,
            resolved_derivations: None,
            local_target_coercions: OnceLock::new(),
            source_slots: None,
            local_source_slots: OnceLock::new(),
            content_ids: None,
            owned_content_ids: Default::default(),
            emit_without_identifiers: false,
        }
    }

    /// Create a transformer that *borrows* the spec instead of owning it.
    ///
    /// This is the hot-path constructor for the row pipeline: it avoids deep-
    /// cloning the whole [`TransformationSpecification`] for every row. The
    /// resulting transformer is otherwise identical to one built with
    /// [`ObjectTransformer::new`].
    pub fn new_borrowed(
        spec: &'s TransformationSpecification,
        source_schema: Option<&'s dyn SchemaProvider>,
        target_schema: Option<&'s dyn SchemaProvider>,
    ) -> Self {
        Self {
            spec: Cow::Borrowed(spec),
            source_schema,
            target_schema,
            compiled: None,
            lookup_index: None,
            strict: false,
            prejoined: false,
            unrestricted_eval: false,
            target_coercions: None,
            resolved_derivations: None,
            local_target_coercions: OnceLock::new(),
            source_slots: None,
            local_source_slots: OnceLock::new(),
            content_ids: None,
            owned_content_ids: Default::default(),
            emit_without_identifiers: false,
        }
    }

    /// Attach a pre-built [`LookupIndex`] for cross-table join resolution.
    ///
    /// Register secondary tables on the index before attaching it; the engine
    /// then resolves `populated_from: "alias.field"` and `expr: "{alias.field}"`
    /// references and supports multi-row aggregation over joined rows.
    pub fn with_lookup_index(mut self, index: Arc<LookupIndex>) -> Self {
        self.lookup_index = Some(index);
        self
    }

    /// Attach a pre-built [`CompiledExprs`] so the engine evaluates cached
    /// `expr:` ASTs instead of re-parsing strings per row.
    ///
    /// The cache must be built from the same spec this transformer holds
    /// (keyed by class/slot derivation names). When no cache is attached, the
    /// engine transparently falls back to the per-call string parse path, so
    /// callers and tests that use [`ObjectTransformer::new`] are unaffected.
    pub fn with_compiled_exprs(mut self, compiled: &'s CompiledExprs) -> Self {
        self.compiled = Some(compiled);
        self
    }

    /// Attach a pre-built [`TargetCoercions`] table so the engine does not
    /// re-resolve target slot datatypes per row. Must be built from the same
    /// spec and target schema this transformer holds. Without one the engine
    /// lazily builds its own on first use.
    pub fn with_target_coercions(mut self, coercions: &'s TargetCoercions) -> Self {
        self.target_coercions = Some(coercions);
        self
    }

    /// Attach a pre-built [`ResolvedDerivations`] table so the engine does not
    /// re-resolve (and re-merge `is_a` / `mixins` for) the class derivation of
    /// every row. Must be built from the same spec this transformer holds.
    pub fn with_resolved_derivations(mut self, resolved: &'s ResolvedDerivations) -> Self {
        self.resolved_derivations = Some(resolved);
        self
    }

    /// Attach a pre-built [`SourceSlots`] table so the engine does not walk the
    /// source schema for the slots a class declares on every row. Must be built
    /// from the same source schema this transformer holds. Without one the engine
    /// lazily builds its own on first use.
    // gate:internal — engine plan hook, not part of the binding-consumed surface.
    pub fn with_source_slots(mut self, slots: &'s SourceSlots) -> Self {
        self.source_slots = Some(slots);
        self
    }

    /// Enable strict mode: unbound names and unresolved `slot()` references in
    /// `expr:` error instead of yielding null (#232), and a value for an
    /// enum-ranged target slot that matches no permissible value of the target
    /// enum, or more than one, errors instead of passing through with a
    /// (once-per-value) warning. Default lax. Mirrors upstream `strict`.
    pub fn with_strict_exprs(mut self, strict: bool) -> Self {
        self.strict = strict;
        self
    }

    /// Declare that the rows of a block with `joins:` are read by upstream's set-based
    /// join engine (`join_engine.transform_block_via_join`): the row is mapped as a
    /// `MergedRow` that already holds the row of every join of the block, found by the
    /// primary row's key, so a nested `class_derivations` resolves its joins from the
    /// primary row rather than from the merged parent. Applies to a call that names its
    /// class derivation ([`ObjectTransformer::map_object_with_derivation`]).
    pub fn with_prejoined_joins(mut self, prejoined: bool) -> Self {
        self.prejoined = prejoined;
        self
    }

    /// Enable `unrestricted_eval`: `expr:` blocks of Python statements run in
    /// the asteval-style interpreter with the source object bound as `src`, and
    /// an expression the restricted evaluator rejects (an upstream
    /// `InvalidExpression`/`TypeError`/`ValueError`) yields null instead of
    /// failing the row. Default off, as upstream: the restricted evaluator
    /// evaluates one expression over the source slots, with no `src`, and
    /// refuses statements and attribute access on comprehension variables.
    /// Mirrors upstream `Transformer.unrestricted_eval`; only enable it for
    /// trusted specs.
    pub fn with_unrestricted_eval(mut self, unrestricted: bool) -> Self {
        self.unrestricted_eval = unrestricted;
        self
    }

    /// The evaluation mode set by [`with_strict_exprs`] and
    /// [`with_unrestricted_eval`].
    fn eval_mode(&self) -> EvalMode {
        EvalMode {
            strict: self.strict,
            unrestricted: self.unrestricted_eval,
        }
    }

    /// Transform a single source object.
    ///
    /// `source_type` names the LinkML class of `source_obj`.  When `None` the
    /// engine tries the schema's tree-root class, then falls back to the first
    /// class derivation's name.
    pub fn map_object(&self, source_obj: &Value, source_type: Option<&str>) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        self.map_object_with_type(source_obj, &source_type, None)
    }

    /// Transform a single source object through an explicit class derivation.
    ///
    /// Mirrors Python `ObjectTransformer.map_object(..., class_derivation=cd)`:
    /// the derivation is used as given, skipping the by-source-type lookup and
    /// therefore the `is_a`/`mixins` merge, so callers merge first
    /// ([`crate::normalize::inheritance::with_inherited_fields`]). The streaming
    /// driver uses it to emit a constants-only block once from an empty row
    /// (upstream `transform_spec`, `1323cc8`).
    pub fn map_object_with_derivation(
        &self,
        source_obj: &Value,
        source_type: Option<&str>,
        class_derivation: &ClassDerivation,
    ) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        self.map_object_with_type_and_derivation(
            source_obj,
            &source_type,
            None,
            Some(class_derivation),
        )
    }

    /// [`ObjectTransformer::map_object_with_derivation`] with a pre-built
    /// [`ObjectIndex`]: the FK-pipeline entry point for a streaming block that
    /// names its own class derivation (see [`ObjectTransformer::map_object_with_index`]).
    pub fn map_object_with_derivation_and_index(
        &self,
        source_obj: &Value,
        source_type: Option<&str>,
        class_derivation: &ClassDerivation,
        index: &ObjectIndex,
    ) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        let idx_ref = if index.is_empty() { None } else { Some(index) };
        self.map_object_with_type_and_derivation(
            source_obj,
            &source_type,
            idx_ref,
            Some(class_derivation),
        )
    }

    /// Transform a single source object with a pre-built [`ObjectIndex`].
    ///
    /// This is the FK-pipeline entry point: the caller builds the index ONCE
    /// (from the full buffered dataset), wraps it in `Arc`, and hands a
    /// reference into every parallel worker. Each worker calls this method
    /// instead of [`map_object`], paying zero per-row index-rebuild cost.
    ///
    /// When `index.is_empty()`, the call degrades to [`map_object`].
    pub fn map_object_with_index(
        &self,
        source_obj: &Value,
        source_type: Option<&str>,
        index: &ObjectIndex,
    ) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        let idx_ref = if index.is_empty() { None } else { Some(index) };
        self.map_object_with_type(source_obj, &source_type, idx_ref)
    }

    /// Transform a whole source **container**, resolving foreign-key references.
    ///
    /// This is the FK-aware entry point. Unlike [`ObjectTransformer::map_object`]
    /// (single object / row-streamable), `map_container` first scans the entire
    /// `container` to build an [`ObjectIndex`] keyed by identifier, then maps the
    /// container with that index available to every nested `expr:` so an FK
    /// scalar (e.g. `subject: "X:1"`) dereferences to its referenced object
    /// (`{id: "X:1", name: "x1"}`) before attribute access (`subject.id`).
    ///
    /// When the spec does **not** dereference any FK-ranged slot, this is exactly
    /// equivalent to [`ObjectTransformer::map_object`]: the index is built but
    /// never consulted, so the result is identical and the (cheap) scan is the
    /// only overhead.
    ///
    /// # Note on streaming
    /// FK resolution needs the *whole* dataset (the referenced collection may
    /// appear after the referencing rows), so FK specs are **not** row-
    /// streamable. The concurrent pipeline keeps the streaming `map_object` path
    /// for non-FK specs; wiring FK into the pipeline (build the index once, share
    /// it across workers) is a follow-up.
    pub fn map_container(&self, container: &Value, source_type: Option<&str>) -> Result<Value> {
        let source_type = self.resolve_source_type(source_type)?;
        let index = ObjectIndex::build(container, Some(&source_type), self.source_schema);
        let idx_ref = if index.is_empty() { None } else { Some(&index) };
        self.map_object_with_type(container, &source_type, idx_ref)
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    /// Core recursive mapper — source type already resolved.
    ///
    /// `index` carries the optional FK [`ObjectIndex`] down through nested
    /// class-ranged recursion so deep `expr:` accesses can still dereference
    /// foreign keys.
    fn map_object_with_type(
        &self,
        source_obj: &Value,
        source_type: &str,
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
        self.map_object_with_type_and_derivation(source_obj, source_type, index, None)
    }

    /// [`Self::map_object_with_type`] with an optional explicit derivation
    /// (Python `class_deriv = class_derivation or self._get_class_derivation(...)`).
    fn map_object_with_type_and_derivation(
        &self,
        source_obj: &Value,
        source_type: &str,
        index: Option<&ObjectIndex>,
        class_derivation: Option<&ClassDerivation>,
    ) -> Result<Value> {
        // Enum pass-through: if source_type is an enum, transform via enum derivation.
        if let Some(ss) = self.source_schema
            && ss.has_enum(source_type)
        {
            return self.transform_enum(source_obj, &[source_type.to_string()], source_obj);
        }

        // Scalar pass-through: if it's a known type, return as-is.
        if let Some(ss) = self.source_schema
            && ss.has_type(source_type)
        {
            return Ok(source_obj.clone());
        }

        // Must be a map.
        let source_map = match source_obj {
            Value::Map(m) => m,
            other => {
                // Graceful degradation — return as-is with a warning path.
                return Ok(other.clone());
            }
        };

        // Find the matching class derivation(s).
        let looked_up;
        let class_deriv = match class_derivation {
            Some(cd) => cd,
            None => match self
                .resolved_derivations
                .and_then(|r| r.by_source.get(source_type))
            {
                Some(resolved) => resolved,
                None => {
                    looked_up = self.get_class_derivation(source_type)?;
                    &looked_up
                }
            },
        };

        // Class-level pivot (melt/unmelt) replaces per-slot derivation entirely.
        // Mirrors Python `ObjectTransformer._perform_pivot_operation`.
        if let Some(pivot) = &class_deriv.pivot_operation {
            return pivot::perform_pivot_operation(pivot, source_map, self.target_schema);
        }

        // Per-slot iteration.
        let empty_map = IndexMap::new();
        let slot_derivations = class_deriv.slot_derivations.as_ref().unwrap_or(&empty_map);

        let mut tgt_attrs: IndexMap<String, Value> =
            IndexMap::with_capacity(slot_derivations.len());
        // PARITY: `transform_block_via_join` hands `map_object` a `MergedRow` of the
        // primary row with the rows of all the block's joins (`primary = class_deriv
        // .populated_from or class_deriv.name`).
        let prejoin = (self.prejoined && class_derivation.is_some()).then(|| {
            let primary = class_deriv
                .populated_from
                .as_deref()
                .filter(|s| !s.is_empty())
                .unwrap_or(class_deriv.name.as_str());
            MergedRow::prejoin(self, primary, source_map, class_deriv)
        });
        let row = RowBindings(
            Default::default(),
            Default::default(),
            Default::default(),
            prejoin.as_ref(),
        );

        for (_, slot_deriv) in slot_derivations {
            let slot_name = slot_deriv.name.as_str();
            let v = self
                .derive_slot(
                    slot_deriv,
                    source_map,
                    source_type,
                    class_deriv,
                    index,
                    &row,
                    Some(&tgt_attrs),
                )
                .map_err(|e| Error::SlotTransform {
                    class: class_deriv.name.clone(),
                    slot: slot_name.to_string(),
                    cause: e.to_string(),
                })?;
            tgt_attrs.insert(slot_name.to_string(), v);
        }
        // Remove hidden slots from output (they exist only for slot() references).
        // PARITY: object_transformer.py::map_object (a19eb095) — only `hide`
        // pops a key; a nested join miss is handled upstream in
        // `_derive_nested_objects` (slot retained as null/[]), not here.
        for (_, slot_deriv) in slot_derivations {
            if slot_deriv.hide.unwrap_or(false) {
                tgt_attrs.shift_remove(slot_deriv.name.as_str());
            }
        }

        self.with_synthesized_id(tgt_attrs, &class_deriv)
    }

    /// Like [`map_object_with_type`] but takes an explicit [`ClassDerivation`]
    /// instead of looking it up from the spec.  Used by `derive_nested_objects`
    /// where the derivation comes directly from the `object_derivations` field.
    fn map_object_internal(
        &self,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        merged: Option<&MergedRow<'_>>,
    ) -> Result<Value> {
        let empty_map = IndexMap::new();
        let slot_derivations = class_deriv.slot_derivations.as_ref().unwrap_or(&empty_map);

        let mut tgt_attrs: IndexMap<String, Value> =
            IndexMap::with_capacity(slot_derivations.len());
        let row = RowBindings(
            Default::default(),
            Default::default(),
            Default::default(),
            merged,
        );
        for (_, slot_deriv) in slot_derivations {
            let slot_name = slot_deriv.name.as_str();
            let v = self
                .derive_slot(
                    slot_deriv,
                    source_map,
                    source_type,
                    class_deriv,
                    index,
                    &row,
                    Some(&tgt_attrs),
                )
                .map_err(|e| Error::SlotTransform {
                    class: class_deriv.name.clone(),
                    slot: slot_name.to_string(),
                    cause: e.to_string(),
                })?;
            tgt_attrs.insert(slot_name.to_string(), v);
        }
        // PARITY: object_transformer.py::map_object (a19eb095) — only `hide`
        // removes a key. Nested join misses are collapsed to null/[] in
        // `derive_nested_objects`, so the key stays present here.
        for (_, slot_deriv) in slot_derivations {
            if slot_deriv.hide.unwrap_or(false) {
                tgt_attrs.shift_remove(slot_deriv.name.as_str());
            }
        }
        self.with_synthesized_id(tgt_attrs, class_deriv)
    }

    /// Derive the value for a single slot derivation.
    #[allow(clippy::too_many_arguments)]
    fn derive_slot(
        &self,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        row: &RowBindings,
        target_attrs: Option<&IndexMap<String, Value>>,
    ) -> Result<Value> {
        let slot_name = slot_deriv.name.as_str();

        // ── Precedence order (mirrors Python map_object) ──────────────────────
        //
        // 1. constant `value:`
        // 2. `expr:`
        // 3. `populated_from:` (direct field copy + value_mappings)
        // 4. `class_derivations:` (nested object derivation — v0.6.0)
        // 5. implicit same-name copy
        //
        // After obtaining v, apply range coercion + cardinality reshaping.

        let (mut v, source_slot_def) = if let Some(const_val) = &slot_deriv.value {
            // 1. Constant value.
            let v = Value::from(const_val);
            (v, None)
        } else if slot_deriv.unit_conversion.is_some() {
            // 1b. unit_conversion — dimensional magnitude conversion.
            //     Mirrors Python `ObjectTransformer._perform_unit_conversion`.
            //     Returns the converted scalar (or a {magnitude, unit} map when
            //     target_magnitude_slot is set); leaves the value unchanged when
            //     the conversion is impossible (unknown / cross-dimension units).
            let v = self.perform_unit_conversion(slot_deriv, source_map, source_type)?;
            (v, None)
        } else if let Some(pivot) = &slot_deriv.pivot_operation {
            // 1b'. slot-level pivot_operation — upstream applies MELT (wide to
            //      EAV/long) whatever the declared direction.
            // PARITY: object_transformer.py::_derive_slot calls `perform_melt`
            // unconditionally for a slot-level `pivot_operation`.
            let v = pivot::perform_melt(pivot, source_map, self.target_schema)?;
            (v, None)
        } else if let Some(agg) = &slot_deriv.aggregation_operation {
            // 1c. aggregation_operation — reduce a multivalued source to a scalar
            //     (or a collection, for List/Set/Array).
            //
            // Join path (#188): when `populated_from` is "alias.field" and a
            // LookupIndex is attached, gather ALL matching rows from the join
            // table, pluck `field` from each, and aggregate over the resulting
            // list.  Mirrors Python `_apply_aggregation_over_join`.
            //
            // Otherwise read the source list from `populated_from` (else same-
            // name slot) and aggregate as before.
            let src_key = slot_deriv.populated_from.as_deref().unwrap_or(slot_name);
            let raw = resolve_aggregation_source(
                src_key,
                source_map,
                self.lookup_index.as_deref(),
                class_deriv,
            );
            let v = apply_aggregation(agg, raw, slot_name)?;
            (v, None)
        } else if let Some(expr) = &slot_deriv.expr {
            // 2. Expression.
            let v = self.eval_expr_for_slot(
                expr,
                source_map,
                slot_name,
                source_type,
                class_deriv,
                index,
                row,
                target_attrs,
            )?;
            (v, None)
        } else if let Some(populated_from) = &slot_deriv.populated_from {
            // 3. populated_from — direct field copy.
            // `_source_class` is a virtual slot that resolves to the source class
            // name itself, enabling value_mappings keyed by source class (#193).
            // `"alias.field"` resolves via the LookupIndex join (#188).
            // The value and its source slot come together, as in upstream
            // (`_resolve_fk_or_literal` and the join / `Class.field` branches).
            let (raw, ssd) = self.resolve_populated_from(
                populated_from,
                slot_deriv,
                source_map,
                source_type,
                class_deriv,
                index,
                row.3,
            )?;
            // PARITY: `_nullify_missing_values` runs on the raw value, before
            // the mappings, offset and range coercion.
            let raw = apply_missing_values(raw, slot_deriv);
            // Apply mappings if present and value is non-null.
            let mut mapped = self.apply_value_mappings(
                raw,
                slot_deriv,
                source_map,
                slot_name,
                source_type,
                class_deriv,
                index,
                row,
                target_attrs,
            )?;
            // `_apply_offset` follows the mappings and precedes the range
            // coercion, which is what turns `30 + 1.0 * 5` (35.0) into 35 for
            // an integer target.
            if let Some(off) = &slot_deriv.offset
                && !mapped.is_null()
            {
                // PARITY: the offset field is read from the row unchecked, so an
                // ambiguous column is the `_AMBIGUOUS` sentinel in `offset_value * ...`.
                if row.3.is_some_and(|m| m.is_ambiguous(&off.offset_field)) {
                    return Err(Error::Python(
                        "TypeError: unsupported operand type(s) for *: 'float' and '_AmbiguousType'"
                            .to_owned(),
                    ));
                }
                mapped = apply_offset(off, mapped, source_map)?;
            }
            (mapped, ssd)
        } else if slot_deriv.class_derivations.is_some() {
            // 4. class_derivations — nested target objects (v0.6.0; replaces the
            //    removed `object_derivations`). Recurse into each nested
            //    ClassDerivation, then decide multivalued/scalar from the TARGET
            //    slot (mirrors Python `_derive_nested_objects` which calls
            //    `target_schemaview.induced_slot`).
            let v =
                self.derive_nested_objects(slot_deriv, source_map, class_deriv, index, row.3)?;
            (v, None)
        } else {
            // 5. Implicit same-name copy.
            // PARITY: `induced_slot` first, then the read: an ambiguous column raises
            // only once the source type is known to declare it.
            let ssd = self.require_source_slot(slot_name, source_type)?;
            if row.3.is_some_and(|m| m.is_ambiguous(slot_name)) {
                return Err(merged_row::ambiguous_column_error(slot_name));
            }
            let raw = source_map.get(slot_name).cloned().unwrap_or(Value::Null);
            let raw = apply_missing_values(raw, slot_deriv);
            (raw, ssd)
        };

        // ── Post-processing: range coercion + cardinality ─────────────────────

        v = self.apply_range_coercion(
            v,
            slot_deriv,
            class_deriv,
            source_slot_def.as_deref(),
            source_map,
            index,
        )?;

        // ── URI / CURIE coercion (applied after other coercions) ──────────────
        //
        // When the slot derivation declares `range: uri` or `range: uriorcurie`,
        // expand any CURIE value to a full URI using the source schema's prefix
        // map.  When `range: curie`, compress an absolute URI to a CURIE.
        // This mirrors Python `ObjectTransformer._coerce_uri` behaviour.
        if !v.is_null()
            && let Some(target_range) = slot_deriv.range.as_deref()
        {
            v = self.coerce_uri_curie(v, target_range);
        }

        Ok(v)
    }

    /// Apply `value_mappings` / `expression_mappings` to `raw` (arm 3 second half).
    ///
    /// Returns `raw` unchanged when neither mapping table is present.  When both
    /// are absent the mapping block is a no-op, matching Python behaviour.
    fn apply_value_mappings(
        &self,
        raw: Value,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        slot_name: &str,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        row: &RowBindings,
        target_attrs: Option<&IndexMap<String, Value>>,
    ) -> Result<Value> {
        if slot_deriv.value_mappings.is_none() && slot_deriv.expression_mappings.is_none() {
            return Ok(raw);
        }
        if raw.is_null() {
            return Ok(raw);
        }
        let key = value_to_string_key(&raw);
        if let Some(kv) = slot_deriv
            .value_mappings
            .as_ref()
            .and_then(|vm| vm.get(&key))
        {
            Ok(Value::from(
                kv.value.as_ref().unwrap_or(&serde_json::Value::Null),
            ))
        } else if let Some(kv) = slot_deriv
            .expression_mappings
            .as_ref()
            .and_then(|em| em.get(&key))
        {
            let expr = kv.value.as_ref().and_then(|v| v.as_str()).unwrap_or("None");
            self.eval_expr_for_slot(
                expr,
                source_map,
                slot_name,
                source_type,
                class_deriv,
                index,
                row,
                target_attrs,
            )
        } else {
            Ok(Value::Null)
        }
    }

    /// The slots `source_type` declares (upstream `Bindings._schema_slots`); `None`
    /// without a source schema or when the source type is not one of its classes.
    fn declared_slots(&self, source_type: &str) -> Option<Arc<DeclaredSlots>> {
        let schema = self.source_schema?;
        if let Some(shared) = self.source_slots {
            return shared.slots_of(source_type).cloned();
        }
        self.local_source_slots
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(source_type.to_owned())
            .or_insert_with(|| SourceSlots::class_slots(schema, source_type))
            .clone()
    }

    /// The shared (or lazily built) target-schema lookup table; `None` without
    /// a target schema, which switches all target-driven coercion off.
    fn target_coercions(&self) -> Option<&TargetCoercions> {
        let target = self.target_schema?;
        Some(match self.target_coercions {
            Some(shared) => shared,
            None => self
                .local_target_coercions
                .get_or_init(|| TargetCoercions::build(&self.spec, target)),
        })
    }

    /// Whether the target schema's induced slot is multivalued; `None` without a
    /// target schema or when the target class has no such slot. `ts` is the
    /// slot's [`TargetSlot`] when the caller already holds it.
    fn target_slot_multivalued(
        &self,
        class_name: &str,
        slot_name: &str,
        ts: Option<&TargetSlot>,
    ) -> Option<bool> {
        let target = self.target_schema?;
        match ts {
            Some(t) => t.multivalued,
            None => self
                .target_coercions()?
                .multivalued(target, class_name, slot_name),
        }
    }

    /// The target schema's facts about one slot of a target class, from the
    /// shared table; `None` without a target schema or when the table does not
    /// hold the slot (callers then resolve on demand).
    fn target_slot(&self, class_name: &str, slot_name: &str) -> Option<&TargetSlot> {
        self.target_coercions()?.slot(class_name, slot_name)
    }

    /// Mirror of `Transformer._target_slot_datatype`: the built-in datatype the
    /// target schema declares for a slot of the target class, if any.
    fn target_slot_datatype(&self, class_name: &str, slot_name: &str) -> Option<&'static str> {
        let target = self.target_schema?;
        self.target_coercions()?
            .datatype(target, class_name, slot_name)
    }

    /// Mirror of `_resolve_enum_value(v, _target_enum_index(...))`: resolve an
    /// enum-ranged target slot's value to the target enum's permissible value
    /// key. A no-op without a target schema or an enum range.
    fn resolve_enum_value(
        &self,
        v: Value,
        slot_deriv: &SlotDerivation,
        class_deriv: &ClassDerivation,
        ts: Option<&TargetSlot>,
    ) -> Result<Value> {
        let (Some(target), Some(coercions)) = (self.target_schema, self.target_coercions()) else {
            return Ok(v);
        };
        // The slot's own table entry, when it holds this spec range; otherwise
        // resolve afresh.
        let cached = ts.and_then(|t| t.enums.iter().find(|(range, _)| *range == slot_deriv.range));
        let index = match cached {
            Some((_, index)) => index.as_ref().map(Cow::Borrowed),
            None => coercions.enum_index(target, &class_deriv.name, slot_deriv),
        };
        match index {
            Some(index) => coercions.resolve_enum_value(v, &index, target, self.strict),
            None => Ok(v),
        }
    }

    /// The range values of `slot_deriv` are coerced to: the spec `range`, else
    /// (when the spec declares none) the target schema's datatype.
    fn coercion_datatype<'a>(
        &self,
        slot_deriv: &'a SlotDerivation,
        class_deriv: &ClassDerivation,
        ts: Option<&TargetSlot>,
    ) -> Option<Cow<'a, str>> {
        match slot_deriv.range.as_deref().filter(|r| !r.is_empty()) {
            Some(range) => Some(Cow::Borrowed(range)),
            None => match ts {
                Some(t) => t.datatype,
                None => self.target_slot_datatype(&class_deriv.name, &slot_deriv.name),
            }
            .map(Cow::Borrowed),
        }
    }

    /// Apply range coercion post-processing.
    ///
    /// With a source slot def: `map_value_by_range` → `coerce_cardinality` →
    /// `coerce_datatype` → `reshape_collection`, in that order (mirrors Python
    /// post-derivation coercion). Without one, a `value:` / `expr:` slot gets
    /// `coerce_cardinality` → `coerce_datatype` (upstream 11b425b). Returns `v`
    /// unchanged when it is null, the slot is hidden, or neither applies.
    ///
    /// `map_value_by_range` sees the spec range only; the datatype coercion
    /// falls back to the target schema's range when the spec declares none
    /// (upstream 0469da7).
    fn apply_range_coercion(
        &self,
        mut v: Value,
        slot_deriv: &SlotDerivation,
        class_deriv: &ClassDerivation,
        source_slot_def: Option<&SlotDef>,
        source_map: &IndexMap<String, Value>,
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
        // Everything the target schema says of this slot, looked up once.
        let ts = self.target_slot(&class_deriv.name, &slot_deriv.name);
        if let Some(ssd) = source_slot_def
            && !v.is_null()
            && !slot_deriv.hide.unwrap_or(false)
        {
            v = self.map_value_by_range(&v, ssd, slot_deriv.range.as_deref(), source_map, index)?;
            self.require_target_slot(slot_deriv, class_deriv, &v, ts)?;
            v = self.coerce_cardinality(v, slot_deriv, class_deriv, ts)?;
            if let Some(datatype) = self.coercion_datatype(slot_deriv, class_deriv, ts) {
                v = coerce_datatype(v, &datatype)?;
            }
            v = self.resolve_enum_value(v, slot_deriv, class_deriv, ts)?;
            v = self.reshape_collection(v, slot_deriv, ssd)?;
        } else if (slot_deriv.value.is_some()
            || slot_deriv.expr.as_deref().is_some_and(|e| !e.is_empty()))
            && !v.is_null()
            && !slot_deriv.hide.unwrap_or(false)
        {
            // PARITY: object_transformer.py::_derive_slot (11b425b) — an `elif`
            // of the source-slot branch, keyed on the spec fields (`value is not
            // None` / truthy `expr`) rather than on which branch produced `v`,
            // so a slot with both `unit_conversion` and `expr` also lands here.
            // No `_map_value_by_range` / `_reshape_collection` (they need a
            // source slot). A `value:`/`expr:` result is coerced to the spec
            // range even without a target schema.
            self.require_target_slot(slot_deriv, class_deriv, &v, ts)?;
            v = self.coerce_cardinality(v, slot_deriv, class_deriv, ts)?;
            if let Some(datatype) = self.coercion_datatype(slot_deriv, class_deriv, ts) {
                v = coerce_datatype(v, &datatype)?;
            }
            v = self.resolve_enum_value(v, slot_deriv, class_deriv, ts)?;
        }
        Ok(v)
    }

    /// The source schema's induced slot for a plain (undotted) `populated_from` or an
    /// implicit same-name copy; `None` without a source schema.
    ///
    /// PARITY: `ObjectTransformer._resolve_fk_or_literal` and the implicit-copy `else` arm
    /// of `_derive_slot` call `context.sv.induced_slot(name, context.source_type)`
    /// unguarded, so a slot the source type does not have (or a source type that is not a
    /// class: `--source-type` is forced onto every block of a directory) raises
    /// `ValueError` instead of yielding null. `TransformationError` wraps it with the
    /// derivation context.
    fn require_source_slot(
        &self,
        slot_name: &str,
        source_type: &str,
    ) -> Result<Option<Cow<'s, SlotDef>>> {
        let Some(schema) = self.source_schema else {
            return Ok(None);
        };
        induced_slot_cow(Some(schema), slot_name, source_type)
            .map(Some)
            .ok_or_else(|| {
                Error::Python(match schema.get_class(source_type) {
                    Err(_) => format!("ValueError: Class: \"{source_type}\" not found"),
                    Ok(_) => format!(
                        "ValueError: No such slot {slot_name} as an attribute of {source_type} ancestors or as a slot definition in the schema"
                    ),
                })
            })
    }

    /// Upstream raises `ValueError` ("No such slot ...") from `sv.induced_slot` when a
    /// target schema is loaded but does not resolve the derived slot on the target class
    /// (or the class itself).
    ///
    /// PARITY: `Transformer._is_coerce_to_multivalued` / `_is_coerce_to_singlevalued`
    /// (reached from `ObjectTransformer._coerce_cardinality`) and
    /// `_resolve_target_slot_datatype` (reached when the spec has no `range`) look the
    /// slot up on `target_schemaview`, so a derived slot absent from the target class
    /// fails the row. The only lookups upstream skips: a `MultiValued`/`MultiValuedDict`
    /// cast or a reversed stringification short-circuits the cardinality probes for a
    /// non-list value (the datatype probe still runs when the spec has no `range`).
    fn require_target_slot(
        &self,
        slot_deriv: &SlotDerivation,
        class_deriv: &ClassDerivation,
        v: &Value,
        ts: Option<&TargetSlot>,
    ) -> Result<()> {
        let Some(target) = self.target_schema else {
            return Ok(());
        };
        if self
            .target_slot_multivalued(&class_deriv.name, &slot_deriv.name, ts)
            .is_some()
        {
            return Ok(());
        }
        let cardinality_shortcut = matches!(
            slot_deriv.cast_collection_as,
            Some(CollectionType::MultiValued | CollectionType::MultiValuedDict)
        ) || slot_deriv
            .stringification
            .as_ref()
            .is_some_and(|s| s.reversed.unwrap_or(false));
        let probes_datatype = slot_deriv.range.as_deref().is_none_or(str::is_empty);
        if cardinality_shortcut && !matches!(v, Value::List(_)) && !probes_datatype {
            return Ok(());
        }
        Err(Error::Python(match target.get_class(&class_deriv.name) {
            Err(_) => format!("ValueError: Class {:?} not found", class_deriv.name),
            Ok(_) => format!(
                "ValueError: No such slot {} as an attribute of {} ancestors or as a slot definition in the schema",
                slot_deriv.name, class_deriv.name
            ),
        }))
    }

    /// Perform a `unit_conversion:` slot derivation.
    ///
    /// Mirrors Python `ObjectTransformer._perform_unit_conversion`:
    ///
    /// - The magnitude is read from `populated_from` (scalar input) or, when
    ///   `source_unit_slot` is set, from a structured `{magnitude, unit}` map
    ///   via `source_magnitude_slot` / `source_unit_slot`.
    /// - The source unit is taken from (in priority order) the structured
    ///   value's unit slot, then the spec's `source_unit`, then the schema
    ///   slot's `unit` annotation. A mismatch between an explicit spec unit
    ///   and the schema unit is an error, matching Python.
    /// - The magnitude is converted to `target_unit` via
    ///   [`units::convert_checked`].
    /// - When `target_magnitude_slot` is set the result is a
    ///   `{target_magnitude_slot: value, target_unit_slot: unit}` map;
    ///   otherwise the bare converted scalar is returned.
    ///
    /// When a unit is unknown to the table or the conversion is dimensionally
    /// incompatible (incl. molar↔mass, which needs a molecular weight not present
    /// in the token), an [`Error::UnitConversion`] is raised — matching Python's
    /// `UndefinedUnitError` / `DimensionalityError`. A non-numeric magnitude
    /// returns `Null` when `none_if_non_numeric` is set, otherwise it is an error;
    /// no source unit anywhere (schema, spec or structured value) is an error too.
    fn perform_unit_conversion(
        &self,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
    ) -> Result<Value> {
        let uc = slot_deriv
            .unit_conversion
            .as_ref()
            .expect("caller guarantees unit_conversion is Some");

        // Locate the raw source value.
        // PARITY: functions/unit_conversion.py::perform_unit_conversion reads
        // `source_obj.get(slot_derivation.populated_from)` — with no
        // `populated_from` that is `get(None)`, so the slot is null; it never
        // falls back to the slot's own name.
        let Some(src_key) = slot_deriv.populated_from.as_deref() else {
            return Ok(Value::Null);
        };
        let curr_v = match source_map.get(src_key) {
            Some(v) if !v.is_null() => v,
            _ => return Ok(Value::Null),
        };

        // Schema-declared source unit (the SlotDef carries the resolved unit
        // code plus the metaslot scheme — ucum_code / symbol / etc. — if any).
        // PARITY: `sv.induced_slot(populated_from, source_type)` raises for a
        // slot the source class does not have.
        let schema_unit_ref: Option<crate::schema::UnitRef> = match self.source_schema {
            Some(ss) => {
                ss.induced_slot(src_key, source_type)
                    .map_err(|_| Error::SlotTransform {
                        class: source_type.to_string(),
                        slot: slot_deriv.name.clone(),
                        cause: format!("ValueError: No such slot as \"{src_key}\""),
                    })?
                    .unit
            }
            None => None,
        };
        // The unit system used for conversion follows the source slot's
        // metaslot (Python derives it the same way); units supplied only via the
        // spec or a structured value carry no metaslot, so default to `Other`
        // (a plain pint registry).
        let unit_system = schema_unit_ref
            .as_ref()
            .map(|u| u.system)
            .unwrap_or(crate::schema::UnitSystem::Other);
        let schema_unit: Option<String> = schema_unit_ref.as_ref().map(|u| u.code.clone());
        let spec_unit = uc.source_unit.clone();

        if let (Some(su), Some(pu)) = (&schema_unit, &spec_unit)
            && su != pu
        {
            return Err(Error::SlotTransform {
                class: source_type.to_string(),
                slot: slot_deriv.name.clone(),
                cause: format!(
                    "mismatch in source units for slot '{src_key}': schema unit \
                         '{su}' vs. transformation spec '{pu}'"
                ),
            });
        }
        // Resolved 'from' unit from schema or spec (may still be overridden by
        // a structured value's unit slot below).
        let mut from_unit: Option<String> = schema_unit.clone().or_else(|| spec_unit.clone());
        // PARITY: with neither a schema unit nor `source_unit`, only a structured
        // value (`source_unit_slot`) can say what the unit is; otherwise upstream
        // raises `ValueError` rather than passing the magnitude through.
        if from_unit.is_none() && uc.source_unit_slot.is_none() {
            return Err(Error::SlotTransform {
                class: source_type.to_string(),
                slot: slot_deriv.name.clone(),
                cause: format!(
                    "ValueError: No source unit provided in schema or transformation spec for slot '{src_key}'"
                ),
            });
        }

        // Extract magnitude (scalar vs structured {value, unit}).
        let magnitude_val: &Value = if let Some(unit_slot) = &uc.source_unit_slot {
            // Structured input.
            let map = match curr_v {
                Value::Map(m) => m,
                _ => {
                    return Err(Error::SlotTransform {
                        class: source_type.to_string(),
                        slot: slot_deriv.name.clone(),
                        cause: format!(
                            "source_unit_slot set but value for '{src_key}' is not a map"
                        ),
                    });
                }
            };
            match map.get(unit_slot) {
                Some(Value::Str(u)) if !u.is_empty() => {
                    if let Some(fu) = &from_unit
                        && u != fu
                    {
                        return Err(Error::SlotTransform {
                            class: source_type.to_string(),
                            slot: slot_deriv.name.clone(),
                            cause: format!(
                                "value unit '{u}' does not match expected '{fu}' \
                                     for slot '{src_key}'"
                            ),
                        });
                    }
                    from_unit = Some(u.clone());
                }
                _ => {
                    return Err(Error::SlotTransform {
                        class: source_type.to_string(),
                        slot: slot_deriv.name.clone(),
                        cause: format!("missing unit in structured value for slot '{src_key}'"),
                    });
                }
            }
            // PARITY: `curr_v.get(uc.source_magnitude_slot)` has no default, so
            // an unset `source_magnitude_slot` is `get(None)`: a missing magnitude.
            match uc
                .source_magnitude_slot
                .as_deref()
                .and_then(|mag_slot| map.get(mag_slot))
                .filter(|m| !m.is_null())
            {
                Some(m) => m,
                None => {
                    return Err(Error::SlotTransform {
                        class: source_type.to_string(),
                        slot: slot_deriv.name.clone(),
                        cause: format!(
                            "missing magnitude in structured value for slot '{src_key}'"
                        ),
                    });
                }
            }
        } else {
            curr_v
        };

        // Coerce magnitude to f64.
        let magnitude = match magnitude_val.try_numeric() {
            Some(m) => m,
            None => {
                if uc.none_if_non_numeric.unwrap_or(false) {
                    return Ok(Value::Null);
                }
                // PARITY: object_transformer.py:1060-1065 — `float(magnitude)`
                // then a bare `raise` on TypeError/ValueError. With
                // none_if_non_numeric unset/false a non-numeric magnitude is an
                // error, not a pass-through.
                return Err(Error::SlotTransform {
                    class: source_type.to_string(),
                    slot: slot_deriv.name.clone(),
                    cause: format!(
                        "non-numeric magnitude value '{magnitude_val:?}' for slot '{src_key}'"
                    ),
                });
            }
        };

        // Determine target unit (defaults to the source unit → identity).
        let to_unit = uc.target_unit.clone().or_else(|| from_unit.clone());

        let (result, out_unit) = match (&from_unit, &to_unit) {
            // PARITY: `if from_unit == to_unit: result = magnitude` — equal units
            // are never looked up, so even an unknown one passes through.
            (Some(fu), Some(tu)) if fu == tu => (magnitude, tu.clone()),
            (Some(fu), Some(tu)) => match units::convert_checked_ex(
                magnitude,
                fu,
                tu,
                unit_system,
                uc.molecular_weight,
                uc.valence,
            ) {
                Ok(r) => (r, tu.clone()),
                // Unknown unit / incompatible dimensions (incl. molar↔mass):
                // raise, mirroring Python's UndefinedUnitError / DimensionalityError.
                Err(units::ConvError::Undefined(u)) => {
                    return Err(Error::UnitConversion {
                        slot: slot_deriv.name.clone(),
                        msg: format!("undefined unit '{u}'"),
                    });
                }
                Err(units::ConvError::Dimensionality(f, t)) => {
                    return Err(Error::UnitConversion {
                        slot: slot_deriv.name.clone(),
                        msg: format!("cannot convert '{f}' to '{t}': incompatible dimensions"),
                    });
                }
            },
            // No units resolvable at all: pass the magnitude through.
            _ => (magnitude, from_unit.clone().unwrap_or_default()),
        };

        let result_val = float_to_value(result);

        if let Some(tgt_mag_slot) = &uc.target_magnitude_slot {
            let mut out = IndexMap::new();
            out.insert(tgt_mag_slot.clone(), result_val);
            if let Some(tgt_unit_slot) = &uc.target_unit_slot {
                out.insert(tgt_unit_slot.clone(), Value::Str(out_unit));
            }
            Ok(Value::Map(out))
        } else {
            Ok(result_val)
        }
    }

    /// Apply URI expansion or CURIE compression to a value based on the
    /// target range name.
    ///
    /// - `"uri"` / `"uriorcurie"` → expand CURIEs to full URIs (already-
    ///   absolute URIs pass through unchanged).
    /// - `"curie"` → compress full URIs to CURIEs (already-CURIE strings
    ///   pass through unchanged).
    ///
    /// Lists are mapped element-wise.  Non-string scalars and maps are
    /// returned unchanged.  When the schema provider cannot perform the
    /// conversion (unknown prefix / no prefix map), the value is returned
    /// unchanged (safe no-op).
    fn coerce_uri_curie(&self, v: Value, target_range: &str) -> Value {
        match target_range {
            "uri" | "uriorcurie" => self.apply_to_strings(v, |s| {
                if let Some(ss) = self.source_schema {
                    ss.expand_curie(s).unwrap_or_else(|| s.to_owned())
                } else {
                    s.to_owned()
                }
            }),
            "curie" => self.apply_to_strings(v, |s| {
                if let Some(ss) = self.source_schema {
                    ss.compress_uri(s).unwrap_or_else(|| s.to_owned())
                } else {
                    s.to_owned()
                }
            }),
            _ => v,
        }
    }

    /// Map a string-transform function over a [`Value`], applying it to every
    /// string leaf while leaving non-string scalars and maps unchanged.
    fn apply_to_strings<F>(&self, v: Value, f: F) -> Value
    where
        F: Fn(&str) -> String + Copy,
    {
        match v {
            Value::Str(s) => Value::Str(f(&s)),
            Value::List(items) => Value::List(
                items
                    .into_iter()
                    .map(|i| self.apply_to_strings(i, f))
                    .collect(),
            ),
            other => other,
        }
    }

    // ── Expression evaluation ─────────────────────────────────────────────────

    /// Mirror of `ObjectTransformer._eval_expr`: evaluate `expr` (pre-parsed as
    /// `cached`, when a compiled cache holds it) under this engine's strict and
    /// unrestricted settings. See [`eval_parsed_gated`].
    fn eval_gated(
        &self,
        cached: Option<&ParsedExpr>,
        expr: &str,
        bindings: &Bindings,
    ) -> ExprResult<Value> {
        match cached {
            Some(parsed) => eval_parsed_gated(parsed, bindings, self.eval_mode()),
            None => parse_expr(expr)
                .and_then(|parsed| eval_parsed_gated(&parsed, bindings, self.eval_mode())),
        }
    }

    /// The restricted-eval bindings of one source row: every source slot,
    /// `NULL`, `_source_class` and the join aliases (everything but
    /// `__slot_values`, which differs per slot).
    fn row_bindings(
        &self,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        merged: Option<&MergedRow<'_>>,
    ) -> Bindings {
        // Sized for the slots and the three injected names.
        let mut bindings: Bindings = IndexMap::with_capacity(source_map.len() + 3);
        bindings.insert("NULL".to_string(), Value::Null);
        // _source_class is the source class name, available in expr: bindings (#193).
        bindings.insert(
            "_source_class".to_string(),
            Value::Str(source_type.to_string()),
        );
        for (k, v) in source_map {
            // FK dereference: when this source slot holds a foreign key (a
            // scalar identifier whose range is a class), replace the binding
            // with the *referenced object* from the index so attribute access
            // (`subject.id` / `subject.name`) resolves against the dereferenced
            // entity rather than failing on a bare string. Distributes over a
            // multivalued FK (list of ids → list of resolved objects).
            let bound = match index {
                Some(idx) if self.is_fk_slot(k, source_type, v, idx) => {
                    self.deref_fk_value(k, source_type, v, idx)
                }
                _ => v.clone(),
            };
            bindings.insert(k.clone(), bound);
        }

        // Join bindings (#188): for each declared join alias, look up the
        // matching row from the LookupIndex and bind the whole row map as
        // `alias → Value::Map(row)`.  This lets expr: "{alias.field}" resolve
        // via normal dot-access on the bound map without any parser changes.
        if let Some(joins) = class_deriv.joins.as_ref() {
            for (alias, ac) in joins {
                // Bind every declared join alias so a qualified `{alias.field}`
                // never trips the unknown-root guard in `eval_ast`: the matched
                // row on a hit, otherwise Null. A sparse-join miss stays a
                // legitimate SQL null, mirroring upstream binding the joined
                // row or None into the eval namespace (source_key or join_on).
                // Without a `LookupIndex` (single-object input) a row with no
                // key value is still `None`; a row with one is
                // [`Self::unresolved_join`].
                // A merged row that holds the join's row hands it over (the pre-joined
                // fast path of `_resolve_joined_row`).
                if let Some(joined) = merged.and_then(|m| m.table(alias)) {
                    bindings.insert(
                        alias.clone(),
                        joined.map_or(Value::Null, |row| Value::Map(row.clone())),
                    );
                    continue;
                }
                let row = self.lookup_index.as_deref().and_then(|li| {
                    let key_str = match source_map.get(join_source_key(ac)?)? {
                        Value::Str(s) => s.clone(),
                        Value::Int(i) => i.to_string(),
                        Value::BigInt(n) => n.to_string(),
                        _ => return None,
                    };
                    let table = ac.class_named.as_deref().unwrap_or(alias.as_str());
                    li.lookup_row(table, &key_str)
                        .map(|r| Value::Map(r.clone()))
                });
                bindings.insert(alias.clone(), row.unwrap_or(Value::Null));
            }
        }
        bindings
    }

    /// The failure upstream's `Bindings._resolve_join` raises when `expr` reads a
    /// join alias whose key the row has but whose table cannot be looked up.
    ///
    /// PARITY: `_resolve_joined_row` returns `None` for a row with no key value, but
    /// with one and no `lookup_index` at all (single-object input) raises
    /// `ValueError`, and with an index that does not hold the table DuckDB raises a
    /// `CatalogException`. The name is resolved when the expression reads it, which
    /// the syntactic names of `expr` stand in for. The `ValueError` is one of the
    /// errors `_eval_expr` hands to the unrestricted fallback; the
    /// `CatalogException` is not.
    fn unresolved_join(
        &self,
        expr: &str,
        source_map: &IndexMap<String, Value>,
        class_deriv: &ClassDerivation,
        merged: Option<&MergedRow<'_>>,
    ) -> Option<JoinFailure> {
        let joins = class_deriv.joins.as_ref()?;
        let mut names = None;
        for (alias, ac) in joins {
            // The merged row holds the join's row: nothing to look up.
            if merged.is_some_and(|m| m.table(alias).is_some()) {
                continue;
            }
            let Some(key) = join_source_key(ac).and_then(|k| source_map.get(k)) else {
                continue;
            };
            if key.is_null() {
                continue;
            }
            let table = ac.class_named.as_deref().unwrap_or(alias.as_str());
            let failure = match self.lookup_index.as_deref() {
                None => JoinFailure::NoIndex(Error::Python(format!(
                    "ValueError: Join configured for {alias:?} but lookup_index has not been initialized"
                ))),
                Some(li) if !li.is_registered(table) => {
                    JoinFailure::NoTable(Error::Python(format!(
                        "duckdb.CatalogException: Catalog Error: Table with name {table} does not exist!"
                    )))
                }
                Some(_) => continue,
            };
            let names = names.get_or_insert_with(|| {
                crate::validate::expr_refs::extract_expr_slot_references(expr)
            });
            if names.contains(alias) {
                return Some(failure);
            }
        }
        None
    }

    /// An `expr:` that names a row key which is not a slot of the source type fails.
    ///
    /// PARITY: `Bindings.__getitem__` resolves a name found in the row through
    /// `get_ctxt_obj_and_dict` -> `dynamic_object(row, sv, source_type)`, whose
    /// `sv.induced_slot(key, source_type)` raises `ValueError` for a key the source class
    /// lacks. This is what makes `--source-type X` over a directory fail for another
    /// block's expression (corpus case d398794d299d). Join aliases are resolved first and
    /// never reach it. Syntactic names stand in for the names evaluation would touch.
    fn check_row_names(
        &self,
        expr: &str,
        source_map: &IndexMap<String, Value>,
        source_type: &str,
        class_deriv: &ClassDerivation,
        row: &RowBindings,
        whole_row: bool,
    ) -> Result<()> {
        let Some(schema) = self.source_schema else {
            return Ok(());
        };
        let unknown = row.1.get_or_init(|| {
            source_map
                .keys()
                .filter(|k| {
                    schema.induced_slot_ref(k, source_type).is_none()
                        && schema.induced_slot(k, source_type).is_err()
                })
                .cloned()
                .collect()
        });
        if unknown.is_empty() {
            return Ok(());
        }
        let names = if whole_row {
            Default::default()
        } else {
            crate::validate::expr_refs::extract_expr_slot_references(expr)
        };
        for key in unknown {
            let is_join = class_deriv
                .joins
                .as_ref()
                .is_some_and(|j| j.contains_key(key));
            if !is_join && (whole_row || names.contains(key)) {
                return Err(Error::Python(match schema.get_class(source_type) {
                    Err(_) => format!("ValueError: Class: \"{source_type}\" not found"),
                    Ok(_) => format!(
                        "ValueError: No such slot {key} as an attribute of {source_type} ancestors or as a slot definition in the schema"
                    ),
                }));
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn eval_expr_for_slot(
        &self,
        expr: &str,
        source_map: &IndexMap<String, Value>,
        slot_name: &str,
        source_type: &str,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        row: &RowBindings,
        target_attrs: Option<&IndexMap<String, Value>>,
    ) -> Result<Value> {
        let class_name = class_deriv.name.as_str();
        // Cached-AST fast path: evaluate the pre-parsed expr when a compiled
        // cache is attached and holds this (class, slot). Falls back to the
        // string parse path otherwise (identical result).
        let cached = self
            .compiled
            .and_then(|c| c.slot(class_name, slot_name, expr));
        // PARITY: `_eval_expr` hands an unrestricted statement block the whole row
        // (`bindings.get_ctxt_obj_and_dict()`), so every key of it must be a slot of the
        // source type, not just the names the block reads.
        let whole_row = self.unrestricted_eval
            && match cached {
                Some(parsed) => parsed.is_program(),
                None => parse_expr(expr).is_ok_and(|parsed| parsed.is_program()),
            };
        self.check_row_names(expr, source_map, source_type, class_deriv, row, whole_row)?;

        // The row's bindings are built by the row's first `expr:` slot and kept
        // for the rest: only `__slot_values` changes from slot to slot.
        let mut cell = row.0.borrow_mut();
        let bindings = cell.get_or_insert_with(|| {
            self.row_bindings(source_map, source_type, class_deriv, index, row.3)
        });

        // PARITY: `Bindings.__getitem__` binds a table of the merged row to its
        // original row (`DynObj(**rows_by_table[name])`), after join aliases and
        // ambiguous columns, ahead of the row's own columns and the self-reference
        // below. It is bound when the expression names it, as a row's worth of clone
        // is not worth paying for every expression.
        if let Some(m) = row.3 {
            for (table, original) in m.tables() {
                let Some(original) = original else { continue };
                if !bindings.contains_key(*table)
                    && !m.is_ambiguous(table)
                    && expr.contains(table)
                    && !class_deriv
                        .joins
                        .as_ref()
                        .is_some_and(|j| j.contains_key(*table))
                {
                    bindings.insert((*table).to_string(), Value::Map((*original).clone()));
                }
            }
        }

        // PARITY: `Bindings.__getitem__` binds the source type's own name to the
        // row (`DynObj(**source_obj)`) when the row has no such key, so `Person.name`
        // reads a slot of the row of a `Person`. It is bound when the expression
        // names it in a qualified reference (`Person.name`: a dot, and the type's name,
        // in the text stand in for that), as a row's worth of clone is not worth
        // paying for every expression; join aliases (already bound) win, and a
        // statement block never sees bare names.
        if expr.as_bytes().contains(&b'.')
            && expr.contains(source_type)
            && !source_map.contains_key(source_type)
            && !bindings.contains_key(source_type)
            && !(self.unrestricted_eval && cached.is_some_and(ParsedExpr::is_program))
        {
            bindings.insert(source_type.to_string(), Value::Map(source_map.clone()));
        }

        // PARITY: no `src` binding. Upstream's `Bindings` hold the source slots,
        // `NULL` and join aliases; `src` exists only inside an unrestricted
        // statement block (`_unrestricted_eval`), where `eval_parsed_gated`
        // builds it from these bindings. A single expression reading `src.x`
        // sees an unbound name: null, or an error in strict mode.
        match (target_attrs, bindings.get_mut("__slot_values")) {
            (Some(attrs), Some(slot_values)) => *slot_values = Value::Map(attrs.clone()),
            (Some(attrs), None) => {
                bindings.insert("__slot_values".to_string(), Value::Map(attrs.clone()));
            }
            (None, _) => {
                bindings.shift_remove("__slot_values");
            }
        }

        if let Some(failure) = self.unresolved_join(expr, source_map, class_deriv, row.3) {
            // A statement block never resolves a join alias (it reads `src`), and
            // one that does not parse fails before any name is read.
            let parsed = cached.cloned().or_else(|| parse_expr(expr).ok());
            if parsed.is_some_and(|p| !p.is_program()) {
                return match failure {
                    JoinFailure::NoIndex(_) if self.unrestricted_eval => Ok(Value::Null),
                    JoinFailure::NoIndex(e) | JoinFailure::NoTable(e) => Err(e),
                };
            }
        }
        let declared = row.2.get_or_init(|| self.declared_slots(source_type));
        let eval = || {
            with_declared_slots(declared.as_ref(), || {
                self.eval_gated(cached, expr, bindings)
            })
        };
        // PARITY: reading an ambiguous column of a merged row raises (see `merged_row`).
        let result = match row.3.and_then(|m| m.ambiguous.as_ref()) {
            Some(names) => with_ambiguous_names(names, eval),
            None => eval(),
        };
        result.map_err(|e| Error::ExprEval {
            class: source_type.to_string(),
            slot: slot_name.to_string(),
            cause: e.to_string(),
        })
    }

    /// Decide whether `(slot_name, value)` on `source_type` is a foreign-key
    /// reference that should be dereferenced through the [`ObjectIndex`].
    ///
    /// Schema-first: when the source schema knows this slot and its range is a
    /// **class**, a scalar value is treated as an FK (an inlined object — a
    /// `Map` — is left as-is). When the schema is unavailable or does not know
    /// the slot, fall back to a structural test: a scalar value (or list of
    /// scalars) whose id(s) are present in the index.
    fn is_fk_slot(
        &self,
        slot_name: &str,
        source_type: &str,
        value: &Value,
        index: &ObjectIndex,
    ) -> bool {
        // An inlined object is never an FK scalar.
        if matches!(value, Value::Map(_)) {
            return false;
        }
        // Schema says range is a class → FK by definition.
        if let Some(ss) = self.source_schema
            && let Ok(slot) = ss.induced_slot(slot_name, source_type)
        {
            return slot.range_class().is_some();
        }
        // Fallback: scalar (or list of scalars) that the index can resolve.
        match value {
            Value::Str(s) => index.contains_id(s),
            Value::Int(i) => index.contains_id(&i.to_string()),
            Value::BigInt(n) => index.contains_id(&n.to_string()),
            Value::List(items) => items.iter().any(|it| match it {
                Value::Str(s) => index.contains_id(s),
                Value::Int(i) => index.contains_id(&i.to_string()),
                Value::BigInt(n) => index.contains_id(&n.to_string()),
                _ => false,
            }),
            _ => false,
        }
    }

    /// Resolve an FK value to its referenced object(s) via the index, keeping
    /// the original value when no entry is found (so a dangling FK degrades to
    /// the bare scalar rather than vanishing). Distributes over lists.
    fn deref_fk_value(
        &self,
        slot_name: &str,
        source_type: &str,
        value: &Value,
        index: &ObjectIndex,
    ) -> Value {
        let range_class: Option<String> = self.source_schema.and_then(|ss| {
            ss.induced_slot(slot_name, source_type)
                .ok()
                .and_then(|s| s.range_class().map(|c| c.to_string()))
        });

        let resolve_one = |v: &Value| -> Value {
            let id = match v {
                Value::Str(s) => Some(s.clone()),
                Value::Int(i) => Some(i.to_string()),
                Value::BigInt(n) => Some(n.to_string()),
                _ => None,
            };
            match id {
                Some(id) => index
                    .get(range_class.as_deref(), &id)
                    .cloned()
                    .unwrap_or_else(|| v.clone()),
                None => v.clone(),
            }
        };

        match value {
            Value::List(items) => Value::List(items.iter().map(&resolve_one).collect()),
            other => resolve_one(other),
        }
    }

    // ── Range-based value coercion ────────────────────────────────────────────

    /// Mirror of Python `_map_value_by_range`.
    ///
    /// Recursively maps nested values based on the source slot's range type.
    fn map_value_by_range(
        &self,
        v: &Value,
        source_slot: &SlotDef,
        target_range: Option<&str>,
        source_map: &IndexMap<String, Value>,
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
        let source_range = &source_slot.range;

        // any_of enum shortcut (range is None/Any but enum alternatives exist).
        if matches!(source_range, RangeKind::None) && !source_slot.any_of_enums.is_empty() {
            let enum_names = source_slot.any_of_enums.clone();
            // PARITY: `transform_enum(v, any_of_enums, source_obj)` — an enum
            // derivation's `expr:` sees the whole source row here (the value
            // itself, as `src`, for the plain enum-range case below). The row
            // is only materialised when some derivation has an `expr:`.
            let row;
            let source_obj = if self.enums_have_expr(&enum_names) {
                row = Value::Map(source_map.clone());
                Some(&row)
            } else {
                None
            };
            if source_slot.multivalued
                && let Value::List(items) = v
            {
                let mapped: Result<Vec<Value>> = items
                    .iter()
                    .map(|item| self.transform_enum(item, &enum_names, source_obj.unwrap_or(item)))
                    .collect();
                return Ok(Value::List(mapped?));
            }
            return self.transform_enum(v, &enum_names, source_obj.unwrap_or(v));
        }

        // No range — return as-is unless it's a complex nested value.
        if matches!(source_range, RangeKind::None) {
            return Ok(v.clone());
        }

        match source_range {
            RangeKind::Enum(enum_name) => {
                if source_slot.multivalued {
                    if let Value::List(items) = v {
                        let mapped: Result<Vec<Value>> = items
                            .iter()
                            .map(|item| {
                                self.transform_enum(item, std::slice::from_ref(enum_name), item)
                            })
                            .collect();
                        return Ok(Value::List(mapped?));
                    }
                    // Scalar value on a multivalued enum slot: map then wrap in a
                    // one-element list (mirror of Python single→multivalued).
                    let mapped = self.transform_enum(v, std::slice::from_ref(enum_name), v)?;
                    return Ok(Value::List(vec![mapped]));
                }
                self.transform_enum(v, std::slice::from_ref(enum_name), v)
            }
            RangeKind::Class(class_name) => {
                let target_range_str = target_range.map(|s| s.to_string());
                if source_slot.multivalued {
                    match v {
                        Value::List(items) => {
                            let mapped: Result<Vec<Value>> = items
                                .iter()
                                .map(|item| {
                                    self.map_object_with_type_and_target(
                                        item,
                                        class_name,
                                        target_range_str.as_deref(),
                                        index,
                                    )
                                })
                                .collect();
                            Ok(Value::List(mapped?))
                        }
                        Value::Map(_) => {
                            // Dict of class instances — recurse each value.
                            let mapped: Result<IndexMap<String, Value>> = if let Value::Map(m) = v {
                                m.iter()
                                    .map(|(k, item)| {
                                        let r = self.map_object_with_type_and_target(
                                            item,
                                            class_name,
                                            target_range_str.as_deref(),
                                            index,
                                        )?;
                                        Ok((k.clone(), r))
                                    })
                                    .collect()
                            } else {
                                unreachable!()
                            };
                            Ok(Value::Map(mapped?))
                        }
                        _ => {
                            // Scalar wrapped in list.
                            let inner = self.map_object_with_type_and_target(
                                v,
                                class_name,
                                target_range_str.as_deref(),
                                index,
                            )?;
                            Ok(Value::List(vec![inner]))
                        }
                    }
                } else {
                    self.map_object_with_type_and_target(
                        v,
                        class_name,
                        target_range_str.as_deref(),
                        index,
                    )
                }
            }
            // PARITY: `_map_value_by_range` runs every value of a multivalued
            // source slot through `map_object`, so a scalar there comes back
            // wrapped in a one-element list (`[map_object(v, ...)]`) — before,
            // and independently of, any cardinality coercion. For a type range
            // `map_object` hands the scalar back as it is.
            //
            // PARITY: that `map_object(v, <type>, target_range)` call is also
            // where a type-ranged value is cast to a `string`/`integer`/`float`
            // target (`str(v)`, `int(v)`, `float(v)`), so `int(True)` is `1`
            // here although `_coerce_datatype` would keep a bool as it is.
            RangeKind::Type(_) if source_slot.multivalued => match v {
                Value::List(items) => items
                    .iter()
                    .map(|item| coerce::map_object_type_value(item, target_range))
                    .collect::<Result<Vec<_>>>()
                    .map(Value::List),
                Value::Map(m) => m
                    .iter()
                    .map(|(k, item)| {
                        coerce::map_object_type_value(item, target_range).map(|c| (k.clone(), c))
                    })
                    .collect::<Result<_>>()
                    .map(Value::Map),
                scalar => coerce::map_object_type_value(scalar, target_range)
                    .map(|c| Value::List(vec![c])),
            },
            RangeKind::Type(_) => coerce::map_object_type_value(v, target_range),
            RangeKind::None => Ok(v.clone()),
        }
    }

    /// map_object_with_type but also respects an explicit target_range override.
    ///
    /// In Python this is `self.map_object(v, source_class_slot_range, target_range)`.
    fn map_object_with_type_and_target(
        &self,
        v: &Value,
        source_type: &str,
        _target_range: Option<&str>,
        index: Option<&ObjectIndex>,
    ) -> Result<Value> {
        // For now target_range is only meaningful for scalar type coercions,
        // which are handled by coerce_datatype. The recursive call just needs
        // the source class to find its derivation. The FK index is threaded
        // through so nested class-ranged objects can still dereference FKs.
        self.map_object_with_type(v, source_type, index)
    }

    // ── Collection reshape (dictionary_key / cast_collection_as) ─────────────

    /// Mirror of Python `ObjectTransformer._reshape_collection`.
    ///
    /// Two directions:
    ///
    /// **List → compact dict** (keyed dict):
    ///   Triggered by `dictionary_key: <slot>` on the SlotDerivation.
    ///   Each element of the list becomes a value in the dict, keyed by the
    ///   element's `dictionary_key` field.  The key field is dropped from
    ///   the stored value (Python: `del v1[slot_derivation.dictionary_key]`).
    ///   If `dictionary_key` is absent but `cast_collection_as:
    ///   MultiValuedDict` is set, the source range class's identifier/key
    ///   slot is used as the key (via `SchemaProvider::identifier_slot`).
    ///
    /// **Compact dict → list** (unkey):
    ///   Triggered by `cast_collection_as: MultiValuedList` on a dict value.
    ///   Each `(k, v)` pair is re-emitted as `{…v, <id_slot>: k}` when the
    ///   range class has an identifier slot, otherwise as bare `v` values.
    ///   Mirrors Python: `[{**v1, src_rng_id_slot.name: k} for k, v1 in v.items()]`.
    fn reshape_collection(
        &self,
        v: Value,
        sd: &SlotDerivation,
        source_slot: &SlotDef,
    ) -> Result<Value> {
        // ── List → keyed dict ────────────────────────────────────────────────
        //
        // If `dictionary_key` is explicitly set, use it directly.
        // If only `cast_collection_as: MultiValuedDict` is set, fall back to
        // the identifier slot of the source range class.
        let dict_key: Option<String> = if let Some(dk) = &sd.dictionary_key {
            Some(dk.clone())
        } else if sd
            .cast_collection_as
            .as_ref()
            .map(|c| matches!(c, CollectionType::MultiValuedDict))
            .unwrap_or(false)
        {
            // Try to find the identifier slot from the source range class.
            if let Some(range_class) = source_slot.range_class() {
                if let Some(src_schema) = self.source_schema {
                    src_schema
                        .identifier_slot(range_class)
                        .ok()
                        .flatten()
                        .map(|s| s.name)
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        if let Some(key_slot) = dict_key
            && let Value::List(items) = v
        {
            // The key keeps its type (`{v1[dictionary_key]: v1 for v1 in v}`): an
            // integer identifier is an integer key, which YAML output shows
            // as such; a dict with only string keys is a record map.
            let mut result = crate::value::PyDict::new();
            for item in items {
                if let Value::Map(mut m) = item {
                    let key_val = m
                        .shift_remove(&key_slot)
                        .unwrap_or_else(|| Value::Str(String::new()));
                    result
                        .insert(key_val, Value::Map(m))
                        .map_err(Error::Python)?;
                }
                // Non-map items in a list being keyed: skip (no key to extract).
            }
            return Ok(Value::Dict(Box::new(result)).into_record_form());
        }

        // ── Keyed dict → list ────────────────────────────────────────────────
        if let Some(CollectionType::MultiValuedList) = &sd.cast_collection_as
            && let Value::Dict(d) = &v
        {
            // A dict with non-string keys: the key keeps its type when re-injected.
            let id_slot_name: Option<String> = source_slot
                .range_class()
                .and_then(|range_class| {
                    self.source_schema
                        .and_then(|ss| ss.identifier_slot(range_class).ok())
                })
                .flatten()
                .map(|s| s.name);
            let list: Vec<Value> = d
                .iter()
                .map(|(k, val)| match (&id_slot_name, val) {
                    (Some(id_slot), Value::Map(inner)) => {
                        let mut inner = inner.clone();
                        inner.insert(id_slot.clone(), k.clone());
                        Value::Map(inner)
                    }
                    _ => val.clone(),
                })
                .collect();
            return Ok(Value::List(list));
        }
        if let Some(CollectionType::MultiValuedList) = &sd.cast_collection_as
            && let Value::Map(m) = v
        {
            // Look up the identifier slot of the source range class so we
            // can re-inject the key into each value object.
            let id_slot_name: Option<String> = if let Some(range_class) = source_slot.range_class()
            {
                self.source_schema
                    .and_then(|ss| ss.identifier_slot(range_class).ok())
                    .flatten()
                    .map(|s| s.name)
            } else {
                None
            };

            let list: Vec<Value> = m
                .into_iter()
                .map(|(k, val)| {
                    if let Some(ref id_slot) = id_slot_name {
                        // Re-inject key into the value map.
                        if let Value::Map(mut inner) = val {
                            inner.insert(id_slot.clone(), Value::Str(k));
                            Value::Map(inner)
                        } else {
                            val
                        }
                    } else {
                        // No identifier slot — return bare values.
                        val
                    }
                })
                .collect();
            return Ok(Value::List(list));
        }

        Ok(v)
    }

    // ── Nested object derivations ─────────────────────────────────────────────

    /// Mirror of Python `ObjectTransformer._derive_nested_objects` (v0.6.0).
    ///
    /// Iterates `slot_deriv.class_derivations` (the v0.6.0 slot-level nested
    /// object map, one level flatter than the removed `object_derivations`) and
    /// recursively transforms `source_map` using each `ClassDerivation`.  The
    /// resulting objects are collected into a `Vec`.
    ///
    /// Cardinality is decided from the **target slot** (not the source), as
    /// Python does via `target_schemaview.induced_slot(slot, target_class)`:
    /// - If `target_schema` is available and the induced target slot is
    ///   `multivalued`, the whole `Vec` is returned as `Value::List`.
    /// - If exactly one object was produced and the target slot is
    ///   single-valued (or no target schema is loaded), the single object is
    ///   returned directly.
    /// - An empty `Vec` always yields `Value::Null`.
    ///
    /// After collecting, `reshape_collection` is **not** called here — the
    /// `dictionary_key` / `cast_collection_as` path in `derive_slot` only
    /// fires when a `source_slot_def` is present (populated_from path).
    /// Object-derivation slots that additionally need dict reshape should be
    /// expressed via `dictionary_key` on the outer slot; callers may chain
    /// that separately.
    fn derive_nested_objects(
        &self,
        slot_deriv: &SlotDerivation,
        source_map: &IndexMap<String, Value>,
        class_deriv: &ClassDerivation,
        index: Option<&ObjectIndex>,
        merged: Option<&MergedRow<'_>>,
    ) -> Result<Value> {
        let cls_derivations = match &slot_deriv.class_derivations {
            Some(v) => v,
            None => return Ok(Value::Null),
        };

        let mut derived: Vec<Value> = Vec::new();
        // `parent_source = parent_class_deriv.populated_from or parent_class_deriv.name`.
        let parent_source = class_deriv
            .populated_from
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(class_deriv.name.as_str());

        for cls_deriv in cls_derivations {
            // `nested_source = cls_derivation.populated_from`; `source_type=cls_derivation
            // .populated_from or parent_source` below.
            let nested_source = cls_deriv
                .populated_from
                .as_deref()
                .filter(|s| !s.is_empty());

            let nested = match nested_source {
                Some(nested_source) if nested_source != parent_source => {
                    let spec = class_deriv
                        .joins
                        .as_ref()
                        .and_then(|joins| joins.get(nested_source));
                    let Some(spec) = spec else {
                        return Err(self.no_implicit_join(cls_deriv, parent_source, nested_source));
                    };
                    // PARITY: object_transformer.py::_derive_nested_objects (a19eb095, #217).
                    // When a nested class_derivation joins to a *different* table and no
                    // row matches the join key (sparse miss), upstream does `continue`
                    // (`if joined_row is None: ... continue`): it appends no object for
                    // that derivation. The enclosing slot is then assigned `None`
                    // (singular) or `[]` (multivalued) below — the target key is
                    // RETAINED with a null/empty value. This keeps "no joined row exists"
                    // distinguishable from "a joined row exists whose field is null",
                    // and only `hide` ever removes the key. This supersedes the earlier
                    // local #266 behaviour, which over-broadly dropped ANY hollow nested
                    // object (including genuine same-table `{slot: null}` objects that
                    // upstream retains).
                    let Some(joined_row) =
                        self.resolve_joined_row(nested_source, source_map, merged, class_deriv)?
                    else {
                        continue;
                    };
                    // `join_key, _ = join_keys(join_spec)`: the parent-side key, which the
                    // merge keeps unmarked.
                    let join_key = join_source_key(spec).unwrap_or_default();
                    let merged_row = MergedRow::merge(
                        source_map,
                        merged,
                        joined_row,
                        join_key,
                        parent_source,
                        nested_source,
                    );
                    self.map_object_internal(
                        &merged_row.map,
                        nested_source,
                        cls_deriv,
                        index,
                        Some(&merged_row),
                    )?
                }
                // The nested derivation reads the parent row as it is (a merge stays one).
                _ => self.map_object_internal(
                    source_map,
                    nested_source.unwrap_or(parent_source),
                    cls_deriv,
                    index,
                    merged,
                )?,
            };
            derived.push(nested);
        }

        // PARITY: `_derive_nested_objects` ends with
        // `self.target_schemaview.induced_slot(slot_derivation.name, target_type)`, so
        // without a target schema (`None`) it raises `AttributeError`, and with one that
        // does not resolve the slot, `ValueError`. Both fail the row. (`target_type` is
        // `None` for a top-level class derivation, which makes upstream's lookup
        // class-less -- any slot or attribute of that name in the schema -- so a slot
        // missing on the target class only fails when no target class has it either;
        // a nested derivation's lookup is always class-based, which is not modelled.)
        match self.target_schema {
            None => {
                return Err(Error::Python(
                    "AttributeError: 'NoneType' object has no attribute 'induced_slot'".to_owned(),
                ));
            }
            Some(target) => {
                let resolves = self
                    .target_slot_multivalued(&class_deriv.name, &slot_deriv.name, None)
                    .is_some()
                    || target
                        .all_class_names()
                        .iter()
                        .any(|class| target.induced_slot(&slot_deriv.name, class).is_ok());
                if !resolves {
                    return Err(Error::Python(format!(
                        "ValueError: No such slot {} as an attribute of {} ancestors or as a slot definition in the schema",
                        slot_deriv.name, class_deriv.name
                    )));
                }
            }
        }

        // Decide cardinality from the target schema slot when available,
        // mirroring Python: `target_class_slot = self.target_schemaview.induced_slot(slot, target_type)`.
        // PARITY (a19eb095): cardinality is applied even when `derived` is empty
        // (all nested derivations were skipped as join misses) — multivalued
        // yields `[]`, singular yields `None`. Upstream: `v = derived_objs` when
        // multivalued else `derived_objs[0] if derived_objs else None`. Do NOT
        // early-return Null on empty: that would drop the `[]` for a multivalued
        // all-miss.
        let target_multivalued = self
            .target_slot_multivalued(&class_deriv.name, &slot_deriv.name, None)
            .unwrap_or(false); // default: single-valued when no target schema

        if target_multivalued {
            Ok(Value::List(derived))
        } else {
            // Single-valued: return first; if >1 that's a schema mismatch but
            // we follow Python and return the first without erroring.
            Ok(derived.into_iter().next().unwrap_or(Value::Null))
        }
    }

    /// The failure `_derive_nested_objects` raises for a nested class derivation
    /// populated from another table than its parent when the parent declares no join
    /// to it: the reason comes from the shared `resolve_join`.
    fn no_implicit_join(
        &self,
        nested: &ClassDerivation,
        parent_source: &str,
        nested_source: &str,
    ) -> Error {
        let reason = match self.source_schema {
            Some(schema) => {
                crate::normalize::join_utils::resolve_join(schema, parent_source, nested_source)
                    .reason
                    .unwrap_or_default()
            }
            None => String::new(),
        };
        Error::Python(format!(
            "TransformationError: Nested class {:?} has populated_from={nested_source:?} which \
             differs from parent populated_from={parent_source:?}, but no implicit join could be \
             synthesized: {reason}. Add an explicit joins: block.",
            nested.name
        ))
    }

    // ── Cardinality coercion ──────────────────────────────────────────────────

    /// Mirror of Python `_coerce_cardinality`: the to-multivalued test comes
    /// first, and the to-single-valued one only applies to a list.
    fn coerce_cardinality(
        &self,
        v: Value,
        slot_deriv: &SlotDerivation,
        class_deriv: &ClassDerivation,
        ts: Option<&TargetSlot>,
    ) -> Result<Value> {
        // PARITY: object_transformer.py::_coerce_cardinality — a scalar bound
        // for a multivalued target is "single to multivalued" even when the
        // slot also carries a (non-reversed) `stringification`, in which case the
        // scalar is *split* on the delimiter (`_singlevalued_to_multivalued`).
        if !matches!(v, Value::Null | Value::List(_))
            && self.is_coerce_to_multivalued(slot_deriv, class_deriv, ts)
        {
            return self.single_to_multivalued(v, slot_deriv);
        }
        if let Value::List(items) = v {
            if self.is_coerce_to_singlevalued(slot_deriv, class_deriv, ts) {
                return self.multivalued_to_single(items, slot_deriv);
            }
            return Ok(Value::List(items));
        }
        Ok(v)
    }

    /// Mirror of `Transformer._is_coerce_to_multivalued`.
    fn is_coerce_to_multivalued(
        &self,
        sd: &SlotDerivation,
        cd: &ClassDerivation,
        ts: Option<&TargetSlot>,
    ) -> bool {
        // PARITY: transformer.py::_is_coerce_to_multivalued lists
        // `MultiValued, MultiValuedDict, MultiValuedDict` (the second a typo for
        // `MultiValuedList`), so `MultiValuedList` never forces a wrap.
        if matches!(
            sd.cast_collection_as,
            Some(CollectionType::MultiValued | CollectionType::MultiValuedDict)
        ) {
            return true;
        }
        if sd
            .stringification
            .as_ref()
            .is_some_and(|s| s.reversed.unwrap_or(false))
        {
            return true;
        }
        self.target_slot_multivalued(&cd.name, &sd.name, ts) == Some(true)
    }

    /// Mirror of `Transformer._is_coerce_to_singlevalued`.
    fn is_coerce_to_singlevalued(
        &self,
        sd: &SlotDerivation,
        cd: &ClassDerivation,
        ts: Option<&TargetSlot>,
    ) -> bool {
        if matches!(sd.cast_collection_as, Some(CollectionType::SingleValued)) {
            return true;
        }
        if sd
            .stringification
            .as_ref()
            .is_some_and(|s| !s.reversed.unwrap_or(false))
        {
            return true;
        }
        self.target_slot_multivalued(&cd.name, &sd.name, ts) == Some(false)
    }

    /// Mirror of `ObjectTransformer._singlevalued_to_multivalued`.
    fn single_to_multivalued(&self, v: Value, sd: &SlotDerivation) -> Result<Value> {
        let Some(s) = &sd.stringification else {
            return Ok(Value::List(vec![v]));
        };
        let slot_err = |msg: String| Error::Cardinality {
            slot: sd.name.clone(),
            msg,
        };
        if let Some(delim) = s.delimiter.as_deref().filter(|d| !d.is_empty()) {
            // `v.split(delimiter)` — an `AttributeError` for a non-string `v`.
            let Value::Str(sv) = &v else {
                return Err(slot_err(format!(
                    "'{}' object has no attribute 'split'",
                    value_type_name(&v)
                )));
            };
            if sv.is_empty() {
                return Ok(Value::List(vec![]));
            }
            return Ok(Value::List(
                sv.split(delim).map(|p| Value::Str(p.to_owned())).collect(),
            ));
        }
        match &s.syntax {
            Some(SerializationSyntaxType::Json) => {
                let Value::Str(sv) = &v else {
                    return Err(slot_err(format!(
                        "the JSON object must be str, bytes or bytearray, not {}",
                        value_type_name(&v)
                    )));
                };
                serde_json::from_str::<serde_json::Value>(sv)
                    .map(|j| Value::from(&j))
                    .map_err(|e| slot_err(e.to_string()))
            }
            Some(SerializationSyntaxType::Yaml) => {
                // PARITY (approximate): `yaml.safe_load(v)`. serde_yaml_ng reads
                // YAML 1.2 scalars where PyYAML reads 1.1 (`yes`, `017`, ...).
                let Value::Str(sv) = &v else {
                    return Ok(v);
                };
                serde_yaml_ng::from_str::<Value>(sv).map_err(|e| slot_err(e.to_string()))
            }
            Some(other) => Err(slot_err(format!("Unknown syntax: {other:?}"))),
            None => Err(slot_err(format!(
                "Cannot convert single valued to multivalued: {}; no delimiter",
                value_to_string_key(&v)
            ))),
        }
    }

    /// Mirror of `ObjectTransformer._multivalued_to_singlevalued`.
    fn multivalued_to_single(&self, items: Vec<Value>, sd: &SlotDerivation) -> Result<Value> {
        let slot_err = |msg: String| Error::Cardinality {
            slot: sd.name.clone(),
            msg,
        };
        if let Some(s) = &sd.stringification {
            if let Some(delim) = s.delimiter.as_deref().filter(|d| !d.is_empty()) {
                // PARITY: `delimiter.join(vs)` raises `TypeError` for a
                // non-string item; it does not stringify.
                let mut parts = Vec::with_capacity(items.len());
                for (i, it) in items.iter().enumerate() {
                    let Value::Str(p) = it else {
                        return Err(slot_err(format!(
                            "sequence item {i}: expected str instance, {} found",
                            value_type_name(it)
                        )));
                    };
                    parts.push(p.as_str());
                }
                return Ok(Value::Str(parts.join(delim)));
            }
            if let Some(syntax) = &s.syntax {
                let list = Value::List(items);
                let out = match syntax {
                    SerializationSyntaxType::Json => py_json(&list).map_err(slot_err)?,
                    SerializationSyntaxType::Yaml => yaml_flow(&list),
                    SerializationSyntaxType::Turtle => {
                        return Err(slot_err("Unknown syntax: TURTLE".into()));
                    }
                };
                return Ok(Value::Str(out));
            }
            return Err(slot_err(format!(
                "Cannot convert multivalued to single valued: {}; no delimiter",
                crate::expr::eval::py_str(&Value::List(items))
            )));
        }
        if items.len() > 1 {
            return Err(Error::Cardinality {
                slot: sd.name.clone(),
                msg: format!(
                    "Cannot coerce multiple values {}",
                    crate::expr::eval::py_str(&Value::List(items))
                ),
            });
        }
        Ok(items.into_iter().next().unwrap_or(Value::Null))
    }

    // ── Enum transformation ───────────────────────────────────────────────────

    /// Whether any enum derivation for `enum_names` evaluates an `expr:`.
    fn enums_have_expr(&self, enum_names: &[String]) -> bool {
        self.spec.enum_derivations.as_ref().is_some_and(|eds| {
            enum_names
                .iter()
                .any(|n| find_enum_derivation(eds, n).is_some_and(|(_, ed)| ed.expr.is_some()))
        })
    }

    /// Mirror of Python `transform_enum`.
    ///
    /// Iterates `enum_names` in order, tries expr evaluation then PV mappings,
    /// then mirror_source fallback.
    fn transform_enum(
        &self,
        source_value: &Value,
        enum_names: &[String],
        source_obj: &Value,
    ) -> Result<Value> {
        let no_enum_derivations = IndexMap::new();
        let enum_derivations = self
            .spec
            .enum_derivations
            .as_ref()
            .unwrap_or(&no_enum_derivations);

        for enum_name in enum_names {
            // PARITY: `Transformer._get_enum_derivation` raises `ValueError` unless exactly
            // one derivation matches (`populated_from == enum` or, with no
            // `populated_from`, `name == enum`): none and several both fail.
            let mut matching = enum_derivations.iter().filter(|(_, ed)| {
                ed.populated_from.as_deref() == Some(enum_name)
                    || (ed.populated_from.is_none() && ed.name == *enum_name)
            });
            let enum_deriv = matching.next().filter(|_| matching.next().is_none());
            let Some((ed_key, ed)) = enum_deriv.map(|(k, e)| (k.as_str(), e)) else {
                return Err(Error::Python(format!(
                    "ValueError: Could not find what to derive from a source {enum_name}"
                )));
            };

            // Expr evaluation on the enum level (rare).
            if let Some(expr) = &ed.expr {
                let mut bindings: Bindings = IndexMap::new();
                // PARITY: upstream transform_enum (object_transformer.py, post
                // 7cffbf0a59) binds {**source_obj, "NULL": None} when the source
                // is a Mapping, else {"src": source_obj, "NULL": None}. The
                // Mapping branch carries no "src" key — mirror that exactly.
                if let Value::Map(m) = source_obj {
                    for (k, v) in m {
                        bindings.insert(k.clone(), v.clone());
                    }
                } else {
                    bindings.insert("src".to_string(), source_obj.clone());
                }
                bindings.insert("NULL".to_string(), Value::Null);
                // Cached-AST fast path mirrors the slot path; falls back to the
                // string parse when no compiled cache is attached.
                let evaled = match self.compiled.and_then(|c| c.enum_exprs.get(ed_key)) {
                    Some(parsed) => {
                        eval_enum_parsed(parsed, &bindings, source_obj, self.eval_mode())
                    }
                    None => parse_expr(expr).and_then(|p| {
                        eval_enum_parsed(&p, &bindings, source_obj, self.eval_mode())
                    }),
                };
                match evaled {
                    Ok(v) if !v.is_null() => return Ok(v),
                    // A null result still falls through to the PV derivations
                    // below — upstream returns only when `v is not None`.
                    Ok(_) => {}
                    // PARITY: upstream transform_enum re-raises
                    // InvalidExpression/TypeError/ValueError unless
                    // unrestricted_eval is enabled, which `eval_enum_parsed`
                    // models; every other error propagates either way. No slot
                    // name is in scope: use the enum name for `class` and the
                    // derivation key for `slot`.
                    Err(e) => {
                        return Err(Error::ExprEval {
                            class: enum_name.to_string(),
                            slot: ed_key.to_string(),
                            cause: e.to_string(),
                        });
                    }
                }
            }

            // Permissible value derivations.
            let src_str = value_to_string_key(source_value);
            if let Some(pvds) = &ed.permissible_value_derivations {
                for (_, pvd) in pvds {
                    // populated_from match — v0.6.0 list-form (#250): a single
                    // string or a list of source PVs, any of which maps here.
                    if let Some(sources) = &pvd.populated_from
                        && sources.iter().any(|s| s == &src_str)
                    {
                        return Ok(Value::Str(pvd.name.clone()));
                    }
                }
            }

            // mirror_source fallback.
            if ed.mirror_source.unwrap_or(false) {
                return Ok(Value::Str(src_str));
            }
        }

        Ok(Value::Null)
    }

    // ── Class derivation lookup ───────────────────────────────────────────────

    /// Find the unique ClassDerivation for a given source type.
    ///
    /// Matches on `populated_from == source_type` (explicit mapping) OR
    /// `name == source_type` when `populated_from` is absent (identity mapping).
    /// Then resolves is_a / mixin inheritance by merging every ancestor field
    /// ([`crate::normalize::inheritance::with_inherited_fields`]).
    fn get_class_derivation(&self, source_type: &str) -> Result<ClassDerivation> {
        resolve_class_derivation(&self.spec, source_type)
    }

    // ── Source type resolution ────────────────────────────────────────────────

    fn resolve_source_type(&self, source_type: Option<&str>) -> Result<String> {
        if let Some(t) = source_type {
            return Ok(t.to_string());
        }

        // Try tree root from schema.
        if let Some(ss) = self.source_schema {
            if let Some(root) = ss.tree_root_class() {
                return Ok(root);
            }
            // Single class fallback.
            let names = ss.all_class_names();
            if names.len() == 1 {
                return Ok(names.into_iter().next().unwrap());
            }
        }

        // Fall back to first class derivation name.
        if let Some(derivations) = &self.spec.class_derivations
            && let Some(first) = derivations.first()
        {
            return Ok(first.name.clone());
        }

        Err(Error::SourceTypeUnresolvable(
            "no source_type provided, no tree root, and spec has no class_derivations".to_string(),
        ))
    }
}

/// The induced slot `slot_name` of `class_name`, borrowed from the provider when
/// it can lend one (see [`SchemaProvider::induced_slot_ref`]), else cloned;
/// `None` when the class has no such slot.
fn induced_slot_cow<'a>(
    schema: Option<&'a dyn SchemaProvider>,
    slot_name: &str,
    class_name: &str,
) -> Option<Cow<'a, SlotDef>> {
    let schema = schema?;
    match schema.induced_slot_ref(slot_name, class_name) {
        Some(slot) => Some(Cow::Borrowed(slot)),
        None => schema
            .induced_slot(slot_name, class_name)
            .ok()
            .map(Cow::Owned),
    }
}

/// The enum derivation for the source enum `enum_name`: the one whose
/// `populated_from` names it, else (with none set) the one named after it.
fn find_enum_derivation<'a>(
    enum_derivations: &'a IndexMap<String, EnumDerivation>,
    enum_name: &str,
) -> Option<(&'a String, &'a EnumDerivation)> {
    enum_derivations.iter().find(|(_, ed)| {
        ed.populated_from.as_deref() == Some(enum_name)
            || (ed.populated_from.is_none() && ed.name == *enum_name)
    })
}

// ── Utility fns ───────────────────────────────────────────────────────────────

/// Wrap a converted magnitude as a [`Value`].
///
/// Python `convert_units` always returns a float, so unit-conversion results
/// are emitted as [`Value::Float`] to match that observable behaviour (even for
/// whole numbers like `1.0`).
fn float_to_value(f: f64) -> Value {
    Value::Float(f)
}

// ── Join key helpers (mirrors Python `_resolve_joined_row`) ──────────────────

/// Source-side join key: `spec.source_key or spec.join_on`.
///
/// Returns `None` when neither is set — mirrors Python's ValueError
/// "must specify 'join_on' or both 'source_key' and 'lookup_key'".
fn join_source_key(ac: &AliasedClass) -> Option<&str> {
    ac.source_key.as_deref().or(ac.join_on.as_deref())
}

/// Python's `str(v)`, the key `_apply_mappings` / `_nullify_missing_values`
/// look a value up by (`True`, `1.0`, `None`, `['a', 'b']`).
///
/// PARITY: object_transformer.py::_apply_mappings (`str_v = str(v)`) — a YAML
/// or JSON boolean looks up `'True'`/`'False'`, not `'true'`/`'false'`.
fn value_to_string_key(v: &Value) -> String {
    crate::expr::eval::py_str(v)
}

/// Map `v` to `Value::Null` when it matches any sentinel in
/// `slot_deriv.missing_values`.  Returns `v` unchanged when there are no
/// sentinels or `v` is already null.  Mirrors Python
/// `SlotDerivation.missing_values`.
fn apply_missing_values(mut v: Value, slot_deriv: &SlotDerivation) -> Value {
    if v.is_null() {
        return v;
    }
    if let Some(sentinels) = &slot_deriv.missing_values {
        let v_key = value_to_string_key(&v);
        let is_missing = sentinels.iter().any(|s| {
            let sv = Value::from(s);
            sv == v || value_to_string_key(&sv) == v_key
        });
        if is_missing {
            v = Value::Null;
        }
    }
    v
}

/// Resolve the raw [`Value`] for an `aggregation_operation:` arm (arm 1c).
///
/// When `src_key` is an `"alias.field"` dotted path and the engine has both a
/// [`LookupIndex`] and `class_deriv.joins`, gathers ALL matching rows from the
/// join table and returns them as a `Value::List`.  Falls back to a plain
/// `source_map` lookup otherwise.
fn resolve_aggregation_source(
    src_key: &str,
    source_map: &IndexMap<String, Value>,
    lookup_index: Option<&LookupIndex>,
    class_deriv: &ClassDerivation,
) -> Value {
    if let Some(dot) = src_key.find('.') {
        let alias = &src_key[..dot];
        let field = &src_key[dot + 1..];
        if let (Some(li), Some(joins)) = (lookup_index, class_deriv.joins.as_ref())
            && let Some(ac) = joins.get(alias)
        {
            let table = ac.class_named.as_deref().unwrap_or(alias);
            let key_val = join_source_key(ac).and_then(|sk| source_map.get(sk));
            if let Some(key_val) = key_val {
                let key_str = match key_val {
                    Value::Str(s) => s.clone(),
                    Value::Int(i) => i.to_string(),
                    Value::BigInt(n) => n.to_string(),
                    _ => String::new(),
                };
                let items: Vec<Value> = li
                    .lookup_rows(table, &key_str)
                    .iter()
                    .filter_map(|row| row.get(field).cloned())
                    .collect();
                return Value::List(items);
            } else {
                return Value::Null;
            }
        }
    }
    source_map.get(src_key).cloned().unwrap_or(Value::Null)
}

/// Human-readable type name for the [`Error::InlinePath`] diagnostic, matching
/// the shape of Python's `type(current_val).__name__` (dict/list/str/int/...).
fn value_type_name(v: &Value) -> &'static str {
    v.python_type_name()
}

// ── Offset / aggregation / pivot helpers ──────────────────────────────────────

/// Apply an [`Offset`]: `value ± offset_value * source[offset_field]`.
/// Mirrors Python `_apply_offset`: an absent (or null) offset field leaves the
/// value unchanged; the arithmetic is Python's, so a non-numeric operand is a
/// `TypeError` and the result is always a float (the range coercion that
/// follows turns `35.0` into `35` for an integer target).
fn apply_offset(off: &Offset, value: Value, source_map: &IndexMap<String, Value>) -> Result<Value> {
    let Some(off_field_val) = source_map.get(&off.offset_field).filter(|v| !v.is_null()) else {
        return Ok(value);
    };
    let operand = |v: &Value| match v {
        Value::Int(i) => Some(*i as f64),
        // Python raises `OverflowError` past the float range; `inf` stands in.
        Value::BigInt(_) => v.try_numeric(),
        Value::Float(f) => Some(*f),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    };
    let type_error = |op: &str, lhs: &Value, rhs: &str| {
        Error::Python(format!(
            "TypeError: unsupported operand type(s) for {op}: '{}' and '{rhs}'",
            value_type_name(lhs)
        ))
    };
    let Some(field) = operand(off_field_val) else {
        return Err(type_error(
            "*",
            &Value::Float(off.offset_value),
            value_type_name(off_field_val),
        ));
    };
    let delta = off.offset_value * field;
    let reverse = off.offset_reverse.unwrap_or(false);
    let Some(base) = operand(&value) else {
        return Err(type_error(if reverse { "-" } else { "+" }, &value, "float"));
    };
    Ok(float_to_value(if reverse {
        base - delta
    } else {
        base + delta
    }))
}

/// Reduce a (multivalued) source value to an aggregate per [`AggregationType`].
///
/// LinkML's Python `ObjectTransformer` does not implement aggregation (it is a
/// datamodel-only construct there); these semantics are defined here. The source
/// is treated as a list (a scalar becomes a one-element list, null an empty one).
/// `Count`/`List`/`Array`/`Set` work on any element type; the arithmetic
/// operators coerce elements to numbers, honouring `invalid_value_handling`
/// (falling back to `null_handling`): `Ignore` skips, `TreatAsZero` substitutes
/// 0, `ErrorOut` raises.
fn apply_aggregation(agg: &AggregationOperation, value: Value, slot: &str) -> Result<Value> {
    let items: Vec<Value> = match value {
        Value::List(items) => items,
        Value::Null => vec![],
        other => vec![other],
    };

    let strategy = agg
        .invalid_value_handling
        .clone()
        .or_else(|| agg.null_handling.clone())
        .unwrap_or(InvalidValueHandlingStrategy::Ignore);

    let numbers = || -> Result<Vec<f64>> {
        let mut out = Vec::new();
        for it in &items {
            match it.try_numeric() {
                Some(n) => out.push(n),
                None => match strategy {
                    InvalidValueHandlingStrategy::Ignore => {}
                    InvalidValueHandlingStrategy::TreatAsZero => out.push(0.0),
                    InvalidValueHandlingStrategy::ErrorOut => {
                        return Err(Error::SlotTransform {
                            class: String::new(),
                            slot: slot.to_string(),
                            cause: "non-numeric value in aggregation".into(),
                        });
                    }
                },
            }
        }
        Ok(out)
    };

    use AggregationType as A;
    let v = match agg.operator {
        A::Count => Value::Int(items.len() as i64),
        A::List | A::Array => Value::List(items),
        A::Set => {
            let mut seen = std::collections::HashSet::new();
            let mut out = Vec::new();
            for it in items {
                if seen.insert(value_to_string_key(&it)) {
                    out.push(it);
                }
            }
            Value::List(out)
        }
        A::Sum => {
            let n = numbers()?;
            let all_int = items.iter().all(|v| matches!(v, Value::Int(_)));
            let sum: f64 = n.iter().sum();
            if all_int {
                Value::Int(sum as i64)
            } else {
                float_to_value(sum)
            }
        }
        A::Average => {
            let n = numbers()?;
            if n.is_empty() {
                Value::Null
            } else {
                Value::Float(n.iter().sum::<f64>() / n.len() as f64)
            }
        }
        A::Min | A::Max => {
            // Return the original element with the min/max numeric value.
            let mut best: Option<(f64, &Value)> = None;
            for it in &items {
                if let Some(n) = it.try_numeric() {
                    let take = match best {
                        None => true,
                        Some((b, _)) => {
                            if matches!(agg.operator, A::Min) {
                                n < b
                            } else {
                                n > b
                            }
                        }
                    };
                    if take {
                        best = Some((n, it));
                    }
                }
            }
            best.map(|(_, v)| v.clone()).unwrap_or(Value::Null)
        }
        A::Median => {
            let mut n = numbers()?;
            if n.is_empty() {
                Value::Null
            } else {
                n.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let mid = n.len() / 2;
                if n.len() % 2 == 1 {
                    float_to_value(n[mid])
                } else {
                    Value::Float((n[mid - 1] + n[mid]) / 2.0)
                }
            }
        }
        A::Variance | A::StdDev => {
            let n = numbers()?;
            if n.is_empty() {
                Value::Null
            } else {
                let mean = n.iter().sum::<f64>() / n.len() as f64;
                let var = n.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n.len() as f64;
                Value::Float(if matches!(agg.operator, A::StdDev) {
                    var.sqrt()
                } else {
                    var
                })
            }
        }
        A::Mode => {
            // Most frequent element (first to reach the max count wins).
            let mut counts: IndexMap<String, (usize, Value)> = IndexMap::new();
            for it in items {
                let key = value_to_string_key(&it);
                counts.entry(key).or_insert((0, it)).0 += 1;
            }
            counts
                .into_iter()
                .max_by_key(|(_, (c, _))| *c)
                .map(|(_, (_, v))| v)
                .unwrap_or(Value::Null)
        }
        A::Custom => {
            return Err(Error::SlotTransform {
                class: String::new(),
                slot: slot.to_string(),
                cause: "CUSTOM aggregation is not supported".into(),
            });
        }
    };
    Ok(v)
}

/// Serialise a [`Value`] to a JSON string matching upstream's
/// `dumps_json(vs, ensure_ascii=True)` (`utils/serialization.py`, which calls
/// `simplejson.dumps(obj, use_decimal=True, allow_nan=True, ensure_ascii=True)`):
/// `", "` between items, `": "` in objects, floats as Python's `repr`,
/// `NaN`/`Infinity` for non-finite floats, decimals as their exact `str()`
/// literal, and every non-ASCII character escaped as `\uXXXX` (a surrogate
/// pair outside the BMP).
///
/// This is the form `stringification: {syntax: JSON}` produces
/// (`object_transformer.py::_multivalued_to_singlevalued`).
///
/// # Errors
/// simplejson's `TypeError` for a set, or for a dict key it cannot write.
fn py_json(v: &Value) -> std::result::Result<String, String> {
    let mut out = String::new();
    write_py_json(&mut out, v)?;
    Ok(out)
}

fn write_py_json(out: &mut String, v: &Value) -> std::result::Result<(), String> {
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Int(i) => out.push_str(&i.to_string()),
        Value::BigInt(n) => out.push_str(&n.to_string()),
        Value::Float(f) => out.push_str(&py_json_float(*f)),
        // PARITY: simplejson raises `TypeError: Object of type complex is not
        // JSON serializable`; the `str()` text is written (a run that fails
        // part-way is worse), as for a date below.
        Value::Complex(z) => write_py_json_str(out, &crate::pynum::complex::repr(**z)),
        // PARITY: utils/serialization.py::dumps_json (simplejson, use_decimal)
        // writes a Decimal as its exact `str()` literal.
        Value::Decimal(d) => out.push_str(&d.to_sci_string()),
        // PARITY (lenient): simplejson raises `TypeError: Object of type date is
        // not JSON serializable` for the `datetime.date` PyYAML builds from an
        // unquoted timestamp. A run that fails part-way is worse than the
        // string, so the `str()` text is written instead.
        Value::Temporal(t) => write_py_json_str(out, &t.to_string()),
        Value::Str(s) => write_py_json_str(out, s),
        // simplejson writes a tuple as an array (`tuple_as_array`).
        Value::List(items) | Value::Tuple(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py_json(out, item)?;
            }
            out.push(']');
        }
        Value::Map(m) => {
            out.push('{');
            for (i, (k, val)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py_json_str(out, k);
                out.push_str(": ");
                write_py_json(out, val)?;
            }
            out.push('}');
        }
        Value::Dict(d) => {
            out.push('{');
            for (i, (k, val)) in d.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                write_py_json_str(out, &crate::value::simplejson_key(k)?);
                out.push_str(": ");
                write_py_json(out, val)?;
            }
            out.push('}');
        }
        Value::Set(_) => return Err("Object of type set is not JSON serializable".to_owned()),
    }
    Ok(())
}

/// simplejson's `FLOAT_REPR` (`float.__repr__`) with `allow_nan=True`.
fn py_json_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        crate::pyfmt::py_float_repr(f)
    }
}

/// simplejson's `encode_basestring_ascii`: `"` and `\` are backslash-escaped,
/// `\n \r \t \b \f` use their short forms, and everything outside printable
/// ASCII (`' '..='~'`, so control characters, DEL and all non-ASCII) becomes
/// `\uXXXX` with lowercase hex digits, as UTF-16 code units.
fn write_py_json_str(out: &mut String, s: &str) {
    use std::fmt::Write as _;
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            ' '..='~' => out.push(ch),
            _ => {
                let mut units = [0u16; 2];
                for unit in ch.encode_utf16(&mut units) {
                    let _ = write!(out, "\\u{unit:04x}");
                }
            }
        }
    }
    out.push('"');
}

/// Serialise a [`Value`] to a YAML *flow* string (`[a, b]`), matching the
/// upstream `stringification: {syntax: YAML}` cases.
///
/// Mirrors `dump_yaml(vs, default_flow_style=True, allow_unicode=False).strip()`
/// (`object_transformer.py::_multivalued_to_singlevalued`) with PyYAML's own
/// quoting, folding and (default) key sorting, via [`crate::pyyaml`].
fn yaml_flow(v: &Value) -> String {
    let opts = crate::pyyaml::DumpOptions {
        default_flow_style: true,
        allow_unicode: false,
        sort_keys: true,
    };
    crate::pyyaml::dump_yaml(v, opts).trim().to_owned()
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    // PARITY: the 3.14 test literals mirror upstream Python's numeric-coercion
    // fixtures; keep the exact value and silence approx_constant rather than diverge.
    #![allow(clippy::approx_constant)]
    use super::*;
    use crate::{
        datamodel::{
            ClassDerivation, EnumDerivation, KeyVal, PermissibleValueDerivation, SlotDerivation,
            TransformationSpecification,
        },
        schema::{
            ClassDef, EnumDef, InMemorySchema, InMemorySchemaBuilder, PermissibleValue, RangeKind,
            SlotDef,
        },
    };
    use indexmap::IndexMap;

    // ── Schema helpers ────────────────────────────────────────────────────────

    /// A single-valued string slot (extra `Person` slots used by the nested-object tests).
    fn plain_string_slot(name: &str) -> SlotDef {
        SlotDef {
            name: name.into(),
            range: RangeKind::Type("string".into()),
            multivalued: false,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
            inlined: false,
            inlined_as_list: false,
        }
    }

    fn make_person_schema() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            .add_type("string")
            .add_type("integer")
            .add_type("boolean")
            .add_enum(EnumDef {
                name: "GenderType".into(),
                permissible_values: vec![
                    PermissibleValue {
                        text: "male".into(),
                        description: None,
                        meaning: None,
                        ..Default::default()
                    },
                    PermissibleValue {
                        text: "female".into(),
                        description: None,
                        meaning: None,
                        ..Default::default()
                    },
                    PermissibleValue {
                        text: "nonbinary man".into(),
                        description: None,
                        meaning: None,
                        ..Default::default()
                    },
                ],
                ..Default::default()
            })
            .add_class(ClassDef {
                name: "Person".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Person",
                SlotDef {
                    name: "id".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: true,
                    identifier: true,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "name".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "age_in_years".into(),
                    range: RangeKind::Type("integer".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "gender".into(),
                    range: RangeKind::Enum("GenderType".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "aliases".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Address".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Address",
                SlotDef {
                    name: "street".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Address",
                SlotDef {
                    name: "city".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "current_address".into(),
                    range: RangeKind::Class("Address".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "friends".into(),
                    range: RangeKind::Class("Person".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot("Person", plain_string_slot("first"))
            .add_slot("Person", plain_string_slot("missing_note"))
            .build()
    }

    /// Minimal spec: identity copy of `id` and `name`.
    fn make_identity_spec() -> TransformationSpecification {
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                ..Default::default()
            },
        );
        TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    fn src_person(id: &str, name: &str) -> Value {
        let mut m = IndexMap::new();
        m.insert("id".into(), Value::Str(id.into()));
        m.insert("name".into(), Value::Str(name.into()));
        Value::Map(m)
    }

    // ── Test 1: populated_from copy ───────────────────────────────────────────

    #[test]
    fn test_populated_from_copy() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                populated_from: Some("id".into()),
                ..Default::default()
            },
        );
        slots.insert(
            "label".into(),
            SlotDerivation {
                name: "label".into(),
                populated_from: Some("name".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Alice");
        let result = engine.map_object(&src, Some("Person")).unwrap();

        let m = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(m["id"], Value::Str("P:001".into()));
        assert_eq!(m["label"], Value::Str("Alice".into()));
        assert!(!m.contains_key("name"), "source key should not appear");
    }

    // ── Test 2: expr-derived slot ─────────────────────────────────────────────

    #[test]
    fn test_expr_derived_slot() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "age_str".into(),
            SlotDerivation {
                name: "age_str".into(),
                expr: Some("str(age_in_years) + \" years\"".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("age_in_years".into(), Value::Int(33));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["age_str"], Value::Str("33 years".into()));
    }

    // ── Test 2b: cached-AST expr path matches the string path ─────────────────

    #[test]
    fn test_slot_function_references_previous_derived_slot() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "_label".into(),
            SlotDerivation {
                name: "_label".into(),
                populated_from: Some("name".into()),
                hide: Some(true),
                ..Default::default()
            },
        );
        slots.insert(
            "display".into(),
            SlotDerivation {
                name: "display".into(),
                expr: Some("slot(\"_label\") + \"!\"".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let result = engine
            .map_object(&src_person("P:001", "Alice"), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["display"], Value::Str("Alice!".into()));
        assert!(!out.contains_key("_label"));
    }

    #[test]
    fn test_slot_function_missing_slot_is_null() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "display".into(),
            SlotDerivation {
                name: "display".into(),
                expr: Some("slot(\"missing\")".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let result = engine
            .map_object(&src_person("P:001", "Alice"), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["display"], Value::Null);
    }

    #[test]
    fn test_expression_mappings_evaluate_mapped_expression() {
        let schema = make_person_schema();
        let mut mappings = IndexMap::new();
        mappings.insert(
            "P:001".into(),
            KeyVal {
                key: "P:001".into(),
                value: Some(serde_json::json!("name + \"!\"")),
            },
        );
        let mut slots = IndexMap::new();
        slots.insert(
            "display".into(),
            SlotDerivation {
                name: "display".into(),
                populated_from: Some("id".into()),
                expression_mappings: Some(mappings),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let result = engine
            .map_object(&src_person("P:001", "Alice"), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["display"], Value::Str("Alice!".into()));
    }

    #[test]
    fn test_compiled_exprs_matches_string_path() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "age_str".into(),
            SlotDerivation {
                name: "age_str".into(),
                expr: Some("str(age_in_years) + \" years\"".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };

        // Build the compiled cache once; the engine borrows it.
        let compiled = CompiledExprs::build(&spec).expect("compile exprs");

        for age in [0i64, 33, 100] {
            let mut m = IndexMap::new();
            m.insert("age_in_years".into(), Value::Int(age));
            let src = Value::Map(m);

            // String path (no cache).
            let string_engine = ObjectTransformer::new(spec.clone(), Some(&schema), None);
            let string_out = string_engine.map_object(&src, Some("Person")).unwrap();

            // Cached-AST path.
            let cached_engine = ObjectTransformer::new(spec.clone(), Some(&schema), None)
                .with_compiled_exprs(&compiled);
            let cached_out = cached_engine.map_object(&src, Some("Person")).unwrap();

            assert_eq!(
                cached_out, string_out,
                "cached/string mismatch at age={age}"
            );
            if let Value::Map(om) = &cached_out {
                assert_eq!(om["age_str"], Value::Str(format!("{age} years")));
            } else {
                panic!("expected Map");
            }
        }
    }

    /// A parent derivation with an `expr:` slot, and a child (`is_a` parent)
    /// that inherits it: the shape the shared tables must cover.
    fn inherited_expr_spec() -> TransformationSpecification {
        let slot = |name: &str, expr: &str| {
            let mut m: IndexMap<String, SlotDerivation> = IndexMap::new();
            m.insert(
                name.into(),
                SlotDerivation {
                    name: name.into(),
                    expr: Some(expr.into()),
                    ..Default::default()
                },
            );
            m
        };
        TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Base".into(),
                    slot_derivations: Some(slot("age_str", "str(age_in_years) + \" years\"")),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Agent".into(),
                    is_a: Some("Base".into()),
                    populated_from: Some("Person".into()),
                    slot_derivations: Some(slot("shout", "str(age_in_years) + \"!\"")),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        }
    }

    #[test]
    fn test_compiled_exprs_cover_inherited_slots_and_check_the_text() {
        let spec = inherited_expr_spec();
        let compiled = CompiledExprs::build(&spec).expect("compile exprs");
        // Own slot, and the slot inherited from `Base`, are cached under `Agent`.
        assert!(
            compiled
                .slot("Agent", "shout", "str(age_in_years) + \"!\"")
                .is_some()
        );
        assert!(
            compiled
                .slot("Agent", "age_str", "str(age_in_years) + \" years\"")
                .is_some()
        );
        // A different text for the same (class, slot) is never served from the cache.
        assert!(compiled.slot("Agent", "age_str", "age_in_years").is_none());
        assert!(compiled.slot("Agent", "missing", "age_in_years").is_none());
    }

    #[test]
    fn test_resolved_derivations_match_the_per_row_lookup() {
        let schema = make_person_schema();
        let spec = inherited_expr_spec();
        let compiled = CompiledExprs::build(&spec).expect("compile exprs");
        let resolved = ResolvedDerivations::build(&spec);
        let mut m = IndexMap::new();
        m.insert("age_in_years".into(), Value::Int(33));
        let src = Value::Map(m);

        let plain = ObjectTransformer::new_borrowed(&spec, Some(&schema), None)
            .map_object(&src, Some("Person"))
            .unwrap();
        let shared = ObjectTransformer::new_borrowed(&spec, Some(&schema), None)
            .with_compiled_exprs(&compiled)
            .with_resolved_derivations(&resolved)
            .map_object(&src, Some("Person"))
            .unwrap();
        assert_eq!(shared, plain);
        let Value::Map(out) = &shared else {
            panic!("expected Map");
        };
        assert_eq!(out["age_str"], Value::Str("33 years".into()));
        assert_eq!(out["shout"], Value::Str("33!".into()));

        // A source type with no derivation is absent from the table, so the
        // engine reports the same error with or without it.
        let missing = |engine: ObjectTransformer| {
            engine
                .map_object(&src, Some("Nobody"))
                .unwrap_err()
                .to_string()
        };
        assert_eq!(
            missing(ObjectTransformer::new_borrowed(&spec, Some(&schema), None)),
            missing(
                ObjectTransformer::new_borrowed(&spec, Some(&schema), None)
                    .with_resolved_derivations(&resolved)
            )
        );
    }

    // ── Test 3: constant value ────────────────────────────────────────────────

    #[test]
    fn test_constant_value() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "source".into(),
            SlotDerivation {
                name: "source".into(),
                value: Some(serde_json::json!("database")),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Alice");
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["source"], Value::Str("database".into()));
    }

    // ── Test 4: value_mappings ────────────────────────────────────────────────

    #[test]
    fn test_value_mappings() {
        let schema = make_person_schema();
        let mut vm: IndexMap<String, KeyVal> = IndexMap::new();
        vm.insert(
            "M".into(),
            KeyVal {
                key: "M".into(),
                value: Some(serde_json::json!("male")),
            },
        );
        vm.insert(
            "F".into(),
            KeyVal {
                key: "F".into(),
                value: Some(serde_json::json!("female")),
            },
        );
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "sex".into(),
            SlotDerivation {
                name: "sex".into(),
                populated_from: Some("gender".into()),
                value_mappings: Some(vm),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            // The `gender` slot ranges over an enum, so upstream's `map_object` runs the
            // value through `transform_enum`, which needs an enum derivation.
            enum_derivations: Some(IndexMap::from([(
                "GenderType".to_owned(),
                EnumDerivation {
                    name: "GenderType".into(),
                    mirror_source: Some(true),
                    ..Default::default()
                },
            )])),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("gender".into(), Value::Str("M".into()));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["sex"], Value::Str("male".into()));
    }

    // ── Test 5: enum PV mapping ───────────────────────────────────────────────

    #[test]
    fn test_enum_pv_mapping() {
        let schema = make_person_schema();
        let mut pvds: IndexMap<String, PermissibleValueDerivation> = IndexMap::new();
        pvds.insert(
            "ACTIVE".into(),
            PermissibleValueDerivation {
                name: "ACTIVE".into(),
                populated_from: Some(vec!["active".into()]),
                ..Default::default()
            },
        );
        pvds.insert(
            "INACTIVE".into(),
            PermissibleValueDerivation {
                name: "INACTIVE".into(),
                populated_from: Some(vec!["inactive".into()]),
                ..Default::default()
            },
        );
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "StatusType".into(),
            EnumDerivation {
                name: "StatusType".into(),
                populated_from: Some("GenderType".into()),
                permissible_value_derivations: Some(pvds),
                ..Default::default()
            },
        );
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "gender".into(),
            SlotDerivation {
                name: "gender".into(),
                populated_from: Some("gender".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("gender".into(), Value::Str("active".into()));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        // "active" → "ACTIVE" via PV derivation.
        assert_eq!(out["gender"], Value::Str("ACTIVE".into()));
    }

    // ── Test 6: scalar → list cardinality coercion ────────────────────────────

    #[test]
    fn test_scalar_to_list_coercion() {
        let schema = make_person_schema();
        let target_schema = InMemorySchemaBuilder::new()
            .add_class(ClassDef {
                name: "Agent".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Agent",
                SlotDef {
                    name: "aliases".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "aliases".into(),
            SlotDerivation {
                name: "aliases".into(),
                populated_from: Some("name".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Agent".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), Some(&target_schema));
        let mut m = IndexMap::new();
        m.insert("name".into(), Value::Str("Alice".into()));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(
            out["aliases"],
            Value::List(vec![Value::Str("Alice".into())])
        );
    }

    // ── Test 7: list → dict (identifier_slot key) cardinality ─────────────────
    // NOTE: list→keyed-dict via dictionary_key is in _reshape_collection (Python),
    // which is a post-processing step that needs the source SchemaView for
    // identifier_slot lookup. We test the cast_collection_as=SingleValued path
    // (list → scalar) instead, which is purely in coerce_cardinality.

    #[test]
    fn test_list_to_single_coercion() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                populated_from: Some("aliases".into()),
                cast_collection_as: Some(CollectionType::SingleValued),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("aliases".into(), Value::List(vec![Value::Str("Al".into())]));
        let src = Value::Map(m);
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["name"], Value::Str("Al".into()));
    }

    // ── Test 8: nested object recursion ──────────────────────────────────────

    #[test]
    fn test_nested_object_recursion() {
        let schema = make_person_schema();
        // Address passthrough.
        let mut addr_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        addr_slots.insert(
            "street".into(),
            SlotDerivation {
                name: "street".into(),
                ..Default::default()
            },
        );
        addr_slots.insert(
            "city".into(),
            SlotDerivation {
                name: "city".into(),
                ..Default::default()
            },
        );
        // Person with current_address.
        let mut person_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        person_slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        person_slots.insert(
            "current_address".into(),
            SlotDerivation {
                name: "current_address".into(),
                populated_from: Some("current_address".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Person".into(),
                    populated_from: Some("Person".into()),
                    slot_derivations: Some(person_slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Address".into(),
                    populated_from: Some("Address".into()),
                    slot_derivations: Some(addr_slots),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        let mut addr = IndexMap::new();
        addr.insert("street".into(), Value::Str("1 Oak St".into()));
        addr.insert("city".into(), Value::Str("Oaktown".into()));

        let mut person = IndexMap::new();
        person.insert("id".into(), Value::Str("P:001".into()));
        person.insert("current_address".into(), Value::Map(addr));

        let result = engine
            .map_object(&Value::Map(person), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        let addr_out = match &out["current_address"] {
            Value::Map(m) => m,
            _ => panic!("expected nested Map"),
        };
        assert_eq!(addr_out["street"], Value::Str("1 Oak St".into()));
        assert_eq!(addr_out["city"], Value::Str("Oaktown".into()));
    }

    // ── Test 9: multivalued nested list recursion ─────────────────────────────

    #[test]
    fn test_multivalued_nested_list() {
        let schema = make_person_schema();
        let mut person_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        person_slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        person_slots.insert(
            "friends".into(),
            SlotDerivation {
                name: "friends".into(),
                populated_from: Some("friends".into()),
                ..Default::default()
            },
        );
        // Friend class (same as Person for recursion).
        let mut friend_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        friend_slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(person_slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        let mut f1 = IndexMap::new();
        f1.insert("id".into(), Value::Str("P:002".into()));
        let mut f2 = IndexMap::new();
        f2.insert("id".into(), Value::Str("P:003".into()));

        let mut person = IndexMap::new();
        person.insert("id".into(), Value::Str("P:001".into()));
        person.insert(
            "friends".into(),
            Value::List(vec![Value::Map(f1), Value::Map(f2)]),
        );

        let result = engine
            .map_object(&Value::Map(person), Some("Person"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        if let Value::List(friends) = &out["friends"] {
            assert_eq!(friends.len(), 2);
            if let Value::Map(f) = &friends[0] {
                assert_eq!(f["id"], Value::Str("P:002".into()));
            } else {
                panic!("expected Map in list");
            }
        } else {
            panic!("expected List");
        }
    }

    // ── Test 10: implicit same-name copy (no populated_from) ─────────────────

    #[test]
    fn test_implicit_copy() {
        let schema = make_person_schema();
        let spec = make_identity_spec();
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:042", "Bob");
        let result = engine.map_object(&src, None).unwrap(); // None → tree_root
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["id"], Value::Str("P:042".into()));
        assert_eq!(out["name"], Value::Str("Bob".into()));
    }

    // ── Test 11: NULL expr sets slot to Null ──────────────────────────────────

    #[test]
    fn test_null_expr() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                expr: Some("NULL".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Alice");
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!("expected Map"),
        };
        assert_eq!(out["name"], Value::Null);
    }

    // (v0.6.0) `sources` removed — the multi-source-single-wins behaviour no
    // longer exists; slots use `populated_from` (single) instead.

    // ── Test 13: enum mirror_source ───────────────────────────────────────────

    #[test]
    fn test_enum_mirror_source() {
        let schema = make_person_schema();
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "GenderType".into(),
            EnumDerivation {
                name: "GenderType".into(),
                populated_from: Some("GenderType".into()),
                mirror_source: Some(true),
                ..Default::default()
            },
        );
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "gender".into(),
            SlotDerivation {
                name: "gender".into(),
                populated_from: Some("gender".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("gender".into(), Value::Str("nonbinary man".into()));
        let result = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(out["gender"], Value::Str("nonbinary man".into()));
    }

    // ── enum expr: scalar `src` binding + error propagation ──────────────────

    /// Enum `expr:` referencing `src` against a scalar source value.
    #[test]
    fn test_enum_expr_scalar_src_binding() {
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "GenderType".into(),
            EnumDerivation {
                name: "GenderType".into(),
                populated_from: Some("GenderType".into()),
                expr: Some("'X:' + src".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let v = engine
            .transform_enum(
                &Value::Str("red".into()),
                &["GenderType".to_string()],
                &Value::Str("red".into()),
            )
            .unwrap();
        assert_eq!(v, Value::Str("X:red".into()));
    }

    /// Enum `expr:` against a Map source still spreads its keys (no `src`).
    #[test]
    fn test_enum_expr_map_source_binds_keys() {
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "GenderType".into(),
            EnumDerivation {
                name: "GenderType".into(),
                populated_from: Some("GenderType".into()),
                expr: Some("'X:' + code".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let mut m = IndexMap::new();
        m.insert("code".into(), Value::Str("red".into()));
        let v = engine
            .transform_enum(
                &Value::Str("ignored".into()),
                &["GenderType".to_string()],
                &Value::Map(m),
            )
            .unwrap();
        assert_eq!(v, Value::Str("X:red".into()));
    }

    /// Enum `expr:` that fails to evaluate returns an error instead of
    /// silently falling through to the permissible-value derivations.
    #[test]
    fn test_enum_expr_error_propagates() {
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "GenderType".into(),
            EnumDerivation {
                name: "GenderType".into(),
                populated_from: Some("GenderType".into()),
                expr: Some("1 +".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let result = engine.transform_enum(
            &Value::Str("red".into()),
            &["GenderType".to_string()],
            &Value::Str("red".into()),
        );
        assert!(matches!(result, Err(Error::ExprEval { .. })));
    }

    /// Enum `expr:` evaluating to null still falls through to the PV
    /// derivations — upstream returns only when `v is not None`.
    #[test]
    fn test_enum_expr_null_falls_through_to_pv() {
        let mut pvds: IndexMap<String, PermissibleValueDerivation> = IndexMap::new();
        pvds.insert(
            "ACTIVE".into(),
            PermissibleValueDerivation {
                name: "ACTIVE".into(),
                populated_from: Some(vec!["red".into()]),
                ..Default::default()
            },
        );
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "GenderType".into(),
            EnumDerivation {
                name: "GenderType".into(),
                populated_from: Some("GenderType".into()),
                expr: Some("None".into()),
                permissible_value_derivations: Some(pvds),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let v = engine
            .transform_enum(
                &Value::Str("red".into()),
                &["GenderType".to_string()],
                &Value::Str("red".into()),
            )
            .unwrap();
        assert_eq!(v, Value::Str("ACTIVE".into()));
    }

    /// Run enum `expr:` against a one-key Map source (`color`), in the given modes.
    fn enum_expr_result(expr: &str, strict: bool, unrestricted: bool) -> Result<Value> {
        let mut enum_derivations: IndexMap<String, EnumDerivation> = IndexMap::new();
        enum_derivations.insert(
            "SimplePrimary".into(),
            EnumDerivation {
                name: "SimplePrimary".into(),
                populated_from: Some("SimplePrimary".into()),
                expr: Some(expr.into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            enum_derivations: Some(enum_derivations),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None)
            .with_strict_exprs(strict)
            .with_unrestricted_eval(unrestricted);
        let mut m = IndexMap::new();
        m.insert("color".into(), Value::Str("light_red".into()));
        engine.transform_enum(
            &Value::Str("light_red".into()),
            &["SimplePrimary".to_string()],
            &Value::Map(m),
        )
    }

    /// Upstream `test_enum_expr_restricted_evaluates`: restricted syntax needs no opt-in.
    #[test]
    fn test_enum_expr_restricted_evaluates() {
        assert_eq!(
            enum_expr_result("'EXPR:' + color", false, false).unwrap(),
            Value::Str("EXPR:light_red".into())
        );
    }

    /// Upstream `test_enum_expr_unrestricted_fallback`: an expression simpleeval
    /// rejects (here `src` is unbound for a mapping source, so `None['color']`
    /// is a `TypeError`) falls back to asteval only with the opt-in.
    #[test]
    fn test_enum_expr_unrestricted_fallback() {
        assert_eq!(
            enum_expr_result("target = src['color']", false, true).unwrap(),
            Value::Str("light_red".into())
        );
    }

    /// Upstream `test_enum_expr_restricted_raises_without_opt_in`.
    #[test]
    fn test_enum_expr_restricted_raises_without_opt_in() {
        assert!(matches!(
            enum_expr_result("target = src['color']", false, false),
            Err(Error::ExprEval { .. })
        ));
    }

    /// Upstream `test_enum_expr_strict_unknown_name_raises_without_fallback`:
    /// a `NameError` is not in the catch tuple, so it never escalates.
    #[test]
    fn test_enum_expr_strict_unknown_name_raises_without_fallback() {
        for unrestricted in [false, true] {
            assert!(
                matches!(
                    enum_expr_result("colorr", true, unrestricted),
                    Err(Error::ExprEval { .. })
                ),
                "unrestricted={unrestricted}"
            );
        }
    }

    // ── Test 14: coerce_datatype ──────────────────────────────────────────────

    // ── Test 15: json_to_value round-trip ─────────────────────────────────────

    #[test]
    fn test_json_to_value() {
        assert_eq!(Value::from(&serde_json::json!(null)), Value::Null);
        assert_eq!(Value::from(&serde_json::json!(true)), Value::Bool(true));
        assert_eq!(Value::from(&serde_json::json!(42)), Value::Int(42));
        assert_eq!(Value::from(&serde_json::json!(3.14)), Value::Float(3.14));
        assert_eq!(
            Value::from(&serde_json::json!("hello")),
            Value::Str("hello".into())
        );
        assert_eq!(
            Value::from(&serde_json::json!([1, 2])),
            Value::List(vec![Value::Int(1), Value::Int(2)])
        );
    }

    // ── Test 16: no class derivation → error ─────────────────────────────────

    #[test]
    fn test_no_class_derivation_error() {
        let schema = make_person_schema();
        let spec = make_identity_spec();
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Alice");
        let err = engine.map_object(&src, Some("Organization")).unwrap_err();
        assert!(matches!(err, Error::NoClassDerivation { .. }));
    }

    // (v0.6.0) class-level `sources` multi-source dispatch removed — cover
    // multiple source classes with one ClassDerivation each (per upstream).

    // ── Test: _source_class virtual binding (#193) ────────────────────────────

    #[test]
    fn test_source_class_binding() {
        // `populated_from: _source_class` returns the source class name, enabling
        // value_mappings keyed by source class.
        let schema = make_person_schema();
        let mut vm: IndexMap<String, KeyVal> = IndexMap::new();
        vm.insert(
            "PersonV1".into(),
            KeyVal {
                key: "PersonV1".into(),
                value: Some(serde_json::Value::String("visit_1".into())),
            },
        );
        vm.insert(
            "PersonV2".into(),
            KeyVal {
                key: "PersonV2".into(),
                value: Some(serde_json::Value::String("visit_2".into())),
            },
        );
        let visit_slot = || {
            let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
            slots.insert(
                "visit_label".into(),
                SlotDerivation {
                    name: "visit_label".into(),
                    populated_from: Some("_source_class".into()),
                    value_mappings: Some(vm.clone()),
                    ..Default::default()
                },
            );
            slots
        };
        // One ClassDerivation per source class (v0.6.0 multi-source pattern).
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "FromV1".into(),
                    populated_from: Some("PersonV1".into()),
                    slot_derivations: Some(visit_slot()),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "FromV2".into(),
                    populated_from: Some("PersonV2".into()),
                    slot_derivations: Some(visit_slot()),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        let obj = Value::Map(IndexMap::new());
        let r1 = engine.map_object(&obj, Some("PersonV1")).unwrap();
        let r2 = engine.map_object(&obj, Some("PersonV2")).unwrap();
        if let (Value::Map(m1), Value::Map(m2)) = (&r1, &r2) {
            assert_eq!(m1["visit_label"], Value::Str("visit_1".into()));
            assert_eq!(m2["visit_label"], Value::Str("visit_2".into()));
        } else {
            panic!("expected maps");
        }
    }

    // ── Test: _source_class in expr (#193) ───────────────────────────────────

    #[test]
    fn test_source_class_in_expr() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "label".into(),
            SlotDerivation {
                name: "label".into(),
                expr: Some("{_source_class}".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "FromV1".into(),
                populated_from: Some("PersonV1".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let obj = Value::Map(IndexMap::new());
        let r = engine.map_object(&obj, Some("PersonV1")).unwrap();
        if let Value::Map(m) = r {
            assert_eq!(m["label"], Value::Str("PersonV1".into()));
        } else {
            panic!("expected map");
        }
    }

    // ── Test 17: is_a ancestor slot inheritance ───────────────────────────────

    #[test]
    fn test_is_a_ancestor_slots() {
        let schema = make_person_schema();
        // Entity has id slot. Agent is_a Entity and adds label.
        let mut entity_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        entity_slots.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        let mut agent_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        agent_slots.insert(
            "label".into(),
            SlotDerivation {
                name: "label".into(),
                populated_from: Some("name".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Entity".into(),
                    slot_derivations: Some(entity_slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Agent".into(),
                    is_a: Some("Entity".into()),
                    populated_from: Some("Person".into()),
                    slot_derivations: Some(agent_slots),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let src = src_person("P:001", "Dave");
        let result = engine.map_object(&src, Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!(),
        };
        // Inherited from Entity.
        assert_eq!(out["id"], Value::Str("P:001".into()));
        // Own slot.
        assert_eq!(out["label"], Value::Str("Dave".into()));
    }

    // ── Test 18: string split to list (stringification.reversed) ─────────────

    #[test]
    fn test_string_split_to_list() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "aliases".into(),
            SlotDerivation {
                name: "aliases".into(),
                populated_from: Some("name".into()),
                stringification: Some(crate::datamodel::StringificationConfiguration {
                    delimiter: Some("|".into()),
                    reversed: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("name".into(), Value::Str("Alice|Bob|Carol".into()));
        let result = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(
            out["aliases"],
            Value::List(vec![
                Value::Str("Alice".into()),
                Value::Str("Bob".into()),
                Value::Str("Carol".into()),
            ])
        );
    }

    // ── Test 19: list join to string (stringification delimiter) ─────────────

    #[test]
    fn test_list_join_to_string() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                populated_from: Some("aliases".into()),
                stringification: Some(crate::datamodel::StringificationConfiguration {
                    delimiter: Some(", ".into()),
                    reversed: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert(
            "aliases".into(),
            Value::List(vec![Value::Str("Alice".into()), Value::Str("Smith".into())]),
        );
        let result = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(out["name"], Value::Str("Alice, Smith".into()));
    }

    // ── FK object-index / flattening tests ────────────────────────────────────

    /// Build the flattening source schema (normalized mappings model):
    ///   MappingSet (tree_root): mappings -> Mapping[], entities -> Entity[]
    ///   Mapping: subject -> Entity, object -> Entity, predicate
    ///   Entity: id (identifier), name
    fn make_mappings_norm_schema() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: "MappingSet".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "MappingSet",
                SlotDef {
                    name: "mappings".into(),
                    range: RangeKind::Class("Mapping".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "MappingSet",
                SlotDef {
                    name: "entities".into(),
                    range: RangeKind::Class("Entity".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Mapping".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Mapping",
                SlotDef {
                    name: "subject".into(),
                    range: RangeKind::Class("Entity".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Mapping",
                SlotDef {
                    name: "object".into(),
                    range: RangeKind::Class("Entity".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Mapping",
                SlotDef {
                    name: "predicate".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Entity".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Entity",
                SlotDef {
                    name: "id".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: true,
                    identifier: true,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Entity",
                SlotDef {
                    name: "name".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build()
    }

    /// Build the flattening input container: one mapping referencing two
    /// entities by id, plus an `entities` dict keyed by identifier.
    fn make_mappings_norm_input() -> Value {
        let mut x1 = IndexMap::new();
        x1.insert("name".into(), Value::Str("x1".into()));
        let mut y1 = IndexMap::new();
        y1.insert("name".into(), Value::Str("y1".into()));
        let mut entities = IndexMap::new();
        entities.insert("X:1".into(), Value::Map(x1));
        entities.insert("Y:1".into(), Value::Map(y1));

        let mut mapping = IndexMap::new();
        mapping.insert("subject".into(), Value::Str("X:1".into()));
        mapping.insert("object".into(), Value::Str("Y:1".into()));
        mapping.insert("predicate".into(), Value::Str("P:1".into()));

        let mut root = IndexMap::new();
        root.insert("mappings".into(), Value::List(vec![Value::Map(mapping)]));
        root.insert("entities".into(), Value::Map(entities));
        Value::Map(root)
    }

    #[test]
    fn object_index_keys_dict_inlined_entities_by_id() {
        let schema = make_mappings_norm_schema();
        let input = make_mappings_norm_input();
        let idx = ObjectIndex::build(&input, Some("MappingSet"), Some(&schema));
        assert!(!idx.is_empty());
        assert!(idx.contains_id("X:1"));
        assert!(idx.contains_id("Y:1"));
        // The resolved object carries its identifier slot back as a field.
        let x = idx.get(Some("Entity"), "X:1").unwrap();
        match x {
            Value::Map(m) => {
                assert_eq!(m["id"], Value::Str("X:1".into()));
                assert_eq!(m["name"], Value::Str("x1".into()));
            }
            _ => panic!("expected map"),
        }
        // Flat fallback also resolves without the class hint.
        assert!(idx.get(None, "Y:1").is_some());
    }

    #[test]
    fn fk_expr_resolves_subject_name_and_id() {
        // subject.id / subject.name / object.id / object.name flatten the FK.
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        for (slot, expr) in [
            ("subject_id", "subject.id"),
            ("subject_name", "subject.name"),
            ("object_id", "object.id"),
            ("object_name", "object.name"),
        ] {
            slots.insert(
                slot.into(),
                SlotDerivation {
                    name: slot.into(),
                    expr: Some(expr.into()),
                    ..Default::default()
                },
            );
        }
        // predicate_id is a plain copy (populated_from), not an FK.
        slots.insert(
            "predicate_id".into(),
            SlotDerivation {
                name: "predicate_id".into(),
                populated_from: Some("predicate".into()),
                ..Default::default()
            },
        );

        let mappings_slot = {
            let mut m: IndexMap<String, SlotDerivation> = IndexMap::new();
            m.insert(
                "mappings".into(),
                SlotDerivation {
                    name: "mappings".into(),
                    populated_from: Some("mappings".into()),
                    ..Default::default()
                },
            );
            m
        };

        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "MappingSet".into(),
                    populated_from: Some("MappingSet".into()),
                    slot_derivations: Some(mappings_slot),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Mapping".into(),
                    populated_from: Some("Mapping".into()),
                    slot_derivations: Some(slots),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        let schema = make_mappings_norm_schema();
        let input = make_mappings_norm_input();
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        // map_container builds the FK index and resolves the references.
        let out = engine.map_container(&input, Some("MappingSet")).unwrap();
        let mappings = match &out {
            Value::Map(m) => match &m["mappings"] {
                Value::List(items) => items.clone(),
                _ => panic!("mappings not a list"),
            },
            _ => panic!("not a map"),
        };
        assert_eq!(mappings.len(), 1);
        let row = match &mappings[0] {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(row["subject_id"], Value::Str("X:1".into()));
        assert_eq!(row["subject_name"], Value::Str("x1".into()));
        assert_eq!(row["object_id"], Value::Str("Y:1".into()));
        assert_eq!(row["object_name"], Value::Str("y1".into()));
        assert_eq!(row["predicate_id"], Value::Str("P:1".into()));
    }

    #[test]
    fn non_fk_spec_unaffected_by_index_path() {
        // A spec with no FK access produces the same result via map_container as
        // via map_object — the index is built but never consulted.
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                populated_from: Some("name".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("id".into(), Value::Str("P1".into()));
        m.insert("name".into(), Value::Str("Alice".into()));
        let input = Value::Map(m);
        let via_object = engine.map_object(&input, Some("Person")).unwrap();
        let via_container = engine.map_container(&input, Some("Person")).unwrap();
        assert_eq!(via_object, via_container);
        let out = match via_container {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(out["name"], Value::Str("Alice".into()));
    }

    // ── unit_conversion tests ────────────────────────────────────────

    /// Source schema with a height slot whose schema unit annotation is `in`.
    fn make_measure_schema() -> InMemorySchema {
        InMemorySchemaBuilder::new()
            .add_type("float")
            .add_type("string")
            .add_class(ClassDef {
                name: "Obs".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Obs",
                SlotDef {
                    name: "height_in".into(),
                    range: RangeKind::Type("float".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: Some(crate::schema::UnitRef {
                        code: "in".into(),
                        system: crate::schema::UnitSystem::Ucum,
                    }),
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Obs",
                SlotDef {
                    name: "glucose".into(),
                    range: RangeKind::Type("float".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build()
    }

    /// Convert height (schema unit `in`) to cm via target_unit only.
    #[test]
    fn unit_conversion_schema_unit_to_cm() {
        let schema = make_measure_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "height_cm".into(),
            SlotDerivation {
                name: "height_cm".into(),
                populated_from: Some("height_in".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    target_unit: Some("cm".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "ObsOut".into(),
                populated_from: Some("Obs".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("height_in".into(), Value::Float(10.0));
        let out = match engine.map_object(&Value::Map(m), Some("Obs")).unwrap() {
            Value::Map(m) => m,
            _ => panic!(),
        };
        // 10 in = 25.4 cm
        match out["height_cm"] {
            Value::Float(f) => assert!((f - 25.4).abs() < 1e-6, "got {f}"),
            ref other => panic!("expected Float, got {other:?}"),
        }
    }

    /// Convert using spec source_unit + target_unit (no schema unit).
    #[test]
    fn unit_conversion_spec_units_mg_to_g() {
        let schema = make_measure_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "dose_g".into(),
            SlotDerivation {
                name: "dose_g".into(),
                populated_from: Some("glucose".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    source_unit: Some("mg".into()),
                    target_unit: Some("g".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "ObsOut".into(),
                populated_from: Some("Obs".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("glucose".into(), Value::Float(2500.0));
        let out = match engine.map_object(&Value::Map(m), Some("Obs")).unwrap() {
            Value::Map(m) => m,
            _ => panic!(),
        };
        match out["dose_g"] {
            Value::Float(f) => assert!((f - 2.5).abs() < 1e-9, "got {f}"),
            ref other => panic!("expected Float, got {other:?}"),
        }
    }

    /// Incompatible / unknown units now raise (parity with Python
    /// `DimensionalityError`), rather than silently passing the value through.
    #[test]
    fn unit_conversion_incompatible_raises() {
        let schema = make_measure_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "out".into(),
            SlotDerivation {
                name: "out".into(),
                populated_from: Some("glucose".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    source_unit: Some("mmol/L".into()),
                    target_unit: Some("mg/dL".into()), // needs molecular weight
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "ObsOut".into(),
                populated_from: Some("Obs".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);
        let mut m = IndexMap::new();
        m.insert("glucose".into(), Value::Float(5.0));
        // mmol/L → mg/dL is a dimensionality mismatch (molar↔mass) → error.
        // (The slot-level UnitConversion error is wrapped by the outer handler.)
        let err = engine.map_object(&Value::Map(m), Some("Obs")).unwrap_err();
        assert!(
            err.to_string().contains("incompatible dimensions"),
            "expected a dimensionality error, got {err:?}"
        );
    }

    /// Structured {value, unit} input with target_magnitude_slot output.
    #[test]
    fn unit_conversion_structured_value_and_output_map() {
        // No schema (units come entirely from the structured value + spec).
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "height".into(),
            SlotDerivation {
                name: "height".into(),
                populated_from: Some("height".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    target_unit: Some("m".into()),
                    source_unit_slot: Some("unit".into()),
                    source_magnitude_slot: Some("value".into()),
                    target_magnitude_slot: Some("value".into()),
                    target_unit_slot: Some("unit".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Out".into(),
                populated_from: Some("Rec".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let mut inner = IndexMap::new();
        inner.insert("value".into(), Value::Float(150.0));
        inner.insert("unit".into(), Value::Str("cm".into()));
        let mut m = IndexMap::new();
        m.insert("height".into(), Value::Map(inner));
        let out = match engine.map_object(&Value::Map(m), Some("Rec")).unwrap() {
            Value::Map(m) => m,
            _ => panic!(),
        };
        let hm = match &out["height"] {
            Value::Map(m) => m,
            other => panic!("{other:?}"),
        };
        match hm["value"] {
            Value::Float(f) => assert!((f - 1.5).abs() < 1e-9, "got {f}"),
            ref other => panic!("expected Float, got {other:?}"),
        }
        assert_eq!(hm["unit"], Value::Str("m".into()));
    }

    /// Non-numeric magnitude with none_if_non_numeric returns Null.
    #[test]
    fn unit_conversion_non_numeric_none() {
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "out".into(),
            SlotDerivation {
                name: "out".into(),
                populated_from: Some("v".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    source_unit: Some("cm".into()),
                    target_unit: Some("m".into()),
                    none_if_non_numeric: Some(true),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Out".into(),
                populated_from: Some("Rec".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let mut m = IndexMap::new();
        m.insert("v".into(), Value::Str("not-a-number".into()));
        let out = match engine.map_object(&Value::Map(m), Some("Rec")).unwrap() {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(out["out"], Value::Null);
    }

    /// Non-numeric magnitude with `none_if_non_numeric` unset raises, mirroring
    /// upstream's bare `raise` (object_transformer.py:1060-1065).
    #[test]
    fn unit_conversion_non_numeric_raises_unset() {
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "out".into(),
            SlotDerivation {
                name: "out".into(),
                populated_from: Some("v".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    source_unit: Some("cm".into()),
                    target_unit: Some("m".into()),
                    none_if_non_numeric: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Out".into(),
                populated_from: Some("Rec".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let mut m = IndexMap::new();
        m.insert("v".into(), Value::Str("not-a-number".into()));
        let err = engine.map_object(&Value::Map(m), Some("Rec")).unwrap_err();
        assert!(matches!(err, Error::SlotTransform { .. }));
    }

    /// Same, with `none_if_non_numeric: false` stated explicitly.
    #[test]
    fn unit_conversion_non_numeric_raises_false() {
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "out".into(),
            SlotDerivation {
                name: "out".into(),
                populated_from: Some("v".into()),
                unit_conversion: Some(crate::datamodel::UnitConversionConfiguration {
                    source_unit: Some("cm".into()),
                    target_unit: Some("m".into()),
                    none_if_non_numeric: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Out".into(),
                populated_from: Some("Rec".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, None, None);
        let mut m = IndexMap::new();
        m.insert("v".into(), Value::Str("not-a-number".into()));
        let err = engine.map_object(&Value::Map(m), Some("Rec")).unwrap_err();
        assert!(matches!(err, Error::SlotTransform { .. }));
    }

    // ── reshape_collection tests ──────────────────────────────────────────────

    /// Helper: build a two-class schema with identifier slots used by reshape tests.
    fn build_reshape_schema() -> InMemorySchema {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};
        InMemorySchemaBuilder::new()
            .add_type("string")
            // "Item" class with identifier slot "id"
            .add_class(ClassDef {
                name: "Item".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Item",
                SlotDef {
                    name: "id".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: true,
                    identifier: true,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Item",
                SlotDef {
                    name: "value".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            // "Container" class with a multivalued "items" slot (range: Item)
            .add_class(ClassDef {
                name: "Container".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Container",
                SlotDef {
                    name: "items".into(),
                    range: RangeKind::Class("Item".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build()
    }

    /// List of objects → keyed dict via explicit `dictionary_key`.
    ///
    /// Python equivalent:
    ///   `v = {v1["id"]: v1 for v1 in v}; for v1 in v.values(): del v1["id"]`
    #[test]
    fn reshape_list_to_dict_via_dictionary_key() {
        let schema = build_reshape_schema();

        // SlotDerivation for "items" with dictionary_key = "id"
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "items".into(),
            SlotDerivation {
                name: "items".into(),
                populated_from: Some("items".into()),
                dictionary_key: Some("id".into()),
                ..Default::default()
            },
        );
        // Identity class derivation for "Item" so map_value_by_range can recurse.
        let mut item_slot_derivs: IndexMap<String, SlotDerivation> = IndexMap::new();
        item_slot_derivs.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        item_slot_derivs.insert(
            "value".into(),
            SlotDerivation {
                name: "value".into(),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Container".into(),
                    populated_from: Some("Container".into()),
                    slot_derivations: Some(slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Item".into(),
                    populated_from: Some("Item".into()),
                    slot_derivations: Some(item_slot_derivs),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        // Source: [{id: "a", value: "alpha"}, {id: "b", value: "beta"}]
        let mut item_a = IndexMap::new();
        item_a.insert("id".into(), Value::Str("a".into()));
        item_a.insert("value".into(), Value::Str("alpha".into()));
        let mut item_b = IndexMap::new();
        item_b.insert("id".into(), Value::Str("b".into()));
        item_b.insert("value".into(), Value::Str("beta".into()));
        let mut src = IndexMap::new();
        src.insert(
            "items".into(),
            Value::List(vec![Value::Map(item_a), Value::Map(item_b)]),
        );

        let result = engine
            .map_object(&Value::Map(src), Some("Container"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };

        // Result should be a dict keyed by "id", with "id" dropped from values.
        let items_val = &out["items"];
        let dict = match items_val {
            Value::Map(m) => m,
            other => panic!("expected Map (keyed dict), got {other:?}"),
        };
        assert_eq!(dict.len(), 2);
        let val_a = match dict.get("a") {
            Some(Value::Map(m)) => m,
            other => panic!("expected Map for key 'a', got {other:?}"),
        };
        // "id" key must have been dropped from the value
        assert!(
            !val_a.contains_key("id"),
            "key 'id' should be dropped from value"
        );
        assert_eq!(val_a["value"], Value::Str("alpha".into()));

        let val_b = match dict.get("b") {
            Some(Value::Map(m)) => m,
            other => panic!("expected Map for key 'b', got {other:?}"),
        };
        assert!(
            !val_b.contains_key("id"),
            "key 'id' should be dropped from value"
        );
        assert_eq!(val_b["value"], Value::Str("beta".into()));
    }

    /// Keyed dict → list round-trip via `cast_collection_as: MultiValuedList`.
    ///
    /// Python equivalent:
    ///   `[{**v1, id_slot: k} for k, v1 in v.items()]`
    ///   where id_slot = schema identifier slot name ("id").
    #[test]
    fn reshape_dict_to_list_via_cast_collection_as() {
        let schema = build_reshape_schema();

        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "items".into(),
            SlotDerivation {
                name: "items".into(),
                populated_from: Some("items".into()),
                cast_collection_as: Some(CollectionType::MultiValuedList),
                ..Default::default()
            },
        );
        // Identity class derivation for "Item" so map_value_by_range can recurse.
        let mut item_slot_derivs2: IndexMap<String, SlotDerivation> = IndexMap::new();
        item_slot_derivs2.insert(
            "id".into(),
            SlotDerivation {
                name: "id".into(),
                ..Default::default()
            },
        );
        item_slot_derivs2.insert(
            "value".into(),
            SlotDerivation {
                name: "value".into(),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![
                ClassDerivation {
                    name: "Container".into(),
                    populated_from: Some("Container".into()),
                    slot_derivations: Some(slots),
                    ..Default::default()
                },
                ClassDerivation {
                    name: "Item".into(),
                    populated_from: Some("Item".into()),
                    slot_derivations: Some(item_slot_derivs2),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        // Source: {a: {value: "alpha"}, b: {value: "beta"}}
        let mut val_a = IndexMap::new();
        val_a.insert("value".into(), Value::Str("alpha".into()));
        let mut val_b = IndexMap::new();
        val_b.insert("value".into(), Value::Str("beta".into()));
        let mut items_dict = IndexMap::new();
        items_dict.insert("a".into(), Value::Map(val_a));
        items_dict.insert("b".into(), Value::Map(val_b));
        let mut src = IndexMap::new();
        src.insert("items".into(), Value::Map(items_dict));

        let result = engine
            .map_object(&Value::Map(src), Some("Container"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };

        let items_val = &out["items"];
        let list = match items_val {
            Value::List(v) => v,
            other => panic!("expected List, got {other:?}"),
        };
        assert_eq!(list.len(), 2);

        // Each element should be a map with the key re-injected as "id".
        for item in list {
            let m = match item {
                Value::Map(m) => m,
                other => panic!("expected Map element, got {other:?}"),
            };
            assert!(m.contains_key("id"), "re-injected 'id' key must be present");
            assert!(m.contains_key("value"));
        }
        // Check specific entries (insertion order preserved via IndexMap).
        let m0 = match &list[0] {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(m0["id"], Value::Str("a".into()));
        assert_eq!(m0["value"], Value::Str("alpha".into()));
        let m1 = match &list[1] {
            Value::Map(m) => m,
            _ => panic!(),
        };
        assert_eq!(m1["id"], Value::Str("b".into()));
        assert_eq!(m1["value"], Value::Str("beta".into()));
    }

    // ── object_derivations tests ──────────────────────────────────────────────

    /// Single nested object produced by `object_derivations`.
    ///
    /// Source class has flat fields; the transform produces a nested target
    /// class by picking fields via an explicit ClassDerivation inside
    /// `object_derivations`.
    #[test]
    fn object_derivations_single_nested() {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};

        // Source: flat Person {first: "Ada", last: "Lovelace", age: 36}
        // Target: Person { full_name: <nested FullName>, age: 36 }
        // FullName { first: "Ada", last: "Lovelace" }

        // Build a target schema that says `full_name` is single-valued.
        let target_schema = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_type("integer")
            .add_class(ClassDef {
                name: "Person".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Person",
                SlotDef {
                    name: "full_name".into(),
                    range: RangeKind::Class("FullName".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "Person",
                SlotDef {
                    name: "age".into(),
                    range: RangeKind::Type("integer".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "FullName".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "FullName",
                SlotDef {
                    name: "first".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_slot(
                "FullName",
                SlotDef {
                    name: "last".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build();

        // Build the nested ClassDerivation for FullName.
        let mut fullname_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        fullname_slots.insert(
            "first".into(),
            SlotDerivation {
                name: "first".into(),
                populated_from: Some("first".into()),
                ..Default::default()
            },
        );
        fullname_slots.insert(
            "last".into(),
            SlotDerivation {
                name: "last".into(),
                populated_from: Some("last".into()),
                ..Default::default()
            },
        );
        let fullname_cls_deriv = ClassDerivation {
            name: "FullName".into(),
            populated_from: Some("Person".into()),
            slot_derivations: Some(fullname_slots),
            ..Default::default()
        };
        let inner_cls_derivs = vec![fullname_cls_deriv];

        // Outer spec: Person → Person with full_name via slot-level
        // class_derivations (v0.6.0), age direct.
        let mut outer_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        outer_slots.insert(
            "full_name".into(),
            SlotDerivation {
                name: "full_name".into(),
                class_derivations: Some(inner_cls_derivs),
                ..Default::default()
            },
        );
        outer_slots.insert(
            "age".into(),
            SlotDerivation {
                name: "age".into(),
                populated_from: Some("age".into()),
                ..Default::default()
            },
        );

        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(outer_slots),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let engine = ObjectTransformer::new(spec, None, Some(&target_schema));

        let mut src = IndexMap::new();
        src.insert("first".into(), Value::Str("Ada".into()));
        src.insert("last".into(), Value::Str("Lovelace".into()));
        src.insert("age".into(), Value::Int(36));

        let result = engine.map_object(&Value::Map(src), Some("Person")).unwrap();
        let out = match result {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };

        // age passes through directly
        assert_eq!(out["age"], Value::Int(36));

        // full_name should be a single Map (not a List), because target schema says single-valued
        let fn_val = match &out["full_name"] {
            Value::Map(m) => m,
            other => panic!("expected Map for full_name, got {other:?}"),
        };
        assert_eq!(fn_val["first"], Value::Str("Ada".into()));
        assert_eq!(fn_val["last"], Value::Str("Lovelace".into()));
    }

    /// Multivalued `object_derivations`: target slot is multivalued, result is List.
    #[test]
    fn object_derivations_multivalued_from_target_schema() {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};

        // Target schema: Container.tags is multivalued (List of Tag).
        let target_schema = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: "Container".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Container",
                SlotDef {
                    name: "tags".into(),
                    range: RangeKind::Class("Tag".into()),
                    multivalued: true, // <-- key: forces List output
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .add_class(ClassDef {
                name: "Tag".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Tag",
                SlotDef {
                    name: "label".into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build();

        // ObjectDerivation with a single ClassDerivation for Tag.
        let mut tag_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        tag_slots.insert(
            "label".into(),
            SlotDerivation {
                name: "label".into(),
                populated_from: Some("tag".into()),
                ..Default::default()
            },
        );
        let tag_cls_deriv = ClassDerivation {
            name: "Tag".into(),
            populated_from: Some("Container".into()),
            slot_derivations: Some(tag_slots),
            ..Default::default()
        };
        let inner = vec![tag_cls_deriv];

        let mut outer_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        outer_slots.insert(
            "tags".into(),
            SlotDerivation {
                name: "tags".into(),
                class_derivations: Some(inner),
                ..Default::default()
            },
        );

        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Container".into(),
                populated_from: Some("Container".into()),
                slot_derivations: Some(outer_slots),
                ..Default::default()
            }]),
            ..Default::default()
        };

        let engine = ObjectTransformer::new(spec, None, Some(&target_schema));

        let mut src = IndexMap::new();
        src.insert("tag".into(), Value::Str("rust".into()));

        let result = engine
            .map_object(&Value::Map(src), Some("Container"))
            .unwrap();
        let out = match result {
            Value::Map(m) => m,
            other => panic!("expected Map, got {other:?}"),
        };

        // tags must be a List because target slot is multivalued
        let tags = match &out["tags"] {
            Value::List(v) => v,
            other => panic!("expected List for tags (multivalued target), got {other:?}"),
        };
        assert_eq!(tags.len(), 1, "one ObjectDerivation → one element");
        let tag0 = match &tags[0] {
            Value::Map(m) => m,
            other => panic!("expected Map in tags list, got {other:?}"),
        };
        assert_eq!(tag0["label"], Value::Str("rust".into()));
    }

    /// #269: a sentinel source value maps to null; real values pass through.
    #[test]
    fn missing_values_maps_sentinel_to_null() {
        let schema = make_person_schema();
        let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        slots.insert(
            "name".into(),
            SlotDerivation {
                name: "name".into(),
                populated_from: Some("name".into()),
                missing_values: Some(vec![serde_json::json!("NA"), serde_json::json!(-9)]),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), None);

        let mut sentinel = IndexMap::new();
        sentinel.insert("name".into(), Value::Str("NA".into()));
        let r = engine
            .map_object(&Value::Map(sentinel), Some("Person"))
            .unwrap();
        match r {
            Value::Map(o) => assert_eq!(o["name"], Value::Null, "sentinel -> null"),
            _ => panic!("expected map"),
        }

        let mut real = IndexMap::new();
        real.insert("name".into(), Value::Str("Bob".into()));
        let r2 = engine
            .map_object(&Value::Map(real), Some("Person"))
            .unwrap();
        match r2 {
            Value::Map(o) => assert_eq!(o["name"], Value::Str("Bob".into())),
            _ => panic!("expected map"),
        }
    }

    /// #217 (a19eb095): a same-table nested object whose inner slot is null is
    /// RETAINED with the key present (value `{first: null}`), and a scalar null
    /// slot also stays present. Upstream `_derive_nested_objects` only collapses
    /// a nested slot to null/[] on a genuine cross-table *join* miss; a
    /// same-table hollow object is kept. (This corrects the earlier local #266
    /// behaviour that omitted any hollow nested object.)
    #[test]
    fn same_table_hollow_nested_object_is_retained() {
        let schema = make_person_schema();
        let mut inner_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        inner_slots.insert(
            "first".into(),
            SlotDerivation {
                name: "first".into(),
                populated_from: Some("first".into()),
                ..Default::default()
            },
        );
        let nested = ClassDerivation {
            name: "FullName".into(),
            populated_from: Some("Person".into()),
            slot_derivations: Some(inner_slots),
            ..Default::default()
        };
        let inner = vec![nested];

        let mut outer: IndexMap<String, SlotDerivation> = IndexMap::new();
        outer.insert(
            "full_name".into(),
            SlotDerivation {
                name: "full_name".into(),
                class_derivations: Some(inner),
                ..Default::default()
            },
        );
        // A scalar slot that resolves to null must stay present (null != absent).
        outer.insert(
            "note".into(),
            SlotDerivation {
                name: "note".into(),
                populated_from: Some("missing_note".into()),
                ..Default::default()
            },
        );
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "Person".into(),
                populated_from: Some("Person".into()),
                slot_derivations: Some(outer),
                ..Default::default()
            }]),
            ..Default::default()
        };
        // Upstream raises unless the target schema resolves every derived slot
        // (`_derive_nested_objects` / `_coerce_cardinality` look them up there).
        let target = {
            use crate::schema::{ClassDef, InMemorySchemaBuilder};
            InMemorySchemaBuilder::new()
                .add_type("string")
                .add_class(ClassDef {
                    name: "Person".into(),
                    tree_root: true,
                    is_a: None,
                    mixins: vec![],
                })
                .add_slot("Person", plain_string_slot("full_name"))
                .add_slot("Person", plain_string_slot("note"))
                .add_class(ClassDef {
                    name: "FullName".into(),
                    tree_root: false,
                    is_a: None,
                    mixins: vec![],
                })
                .add_slot("FullName", plain_string_slot("first"))
                .build()
        };
        let engine = ObjectTransformer::new(spec, Some(&schema), Some(&target));

        // No `first` in source, same-table nested (no join miss) → nested object
        // is retained as `{first: null}`; scalar null slot also stays present.
        let r = engine
            .map_object(&Value::Map(IndexMap::new()), Some("Person"))
            .unwrap();
        match &r {
            Value::Map(o) => {
                assert!(
                    o.contains_key("full_name"),
                    "same-table hollow nested object retained"
                );
                match &o["full_name"] {
                    Value::Map(inner) => {
                        assert_eq!(inner["first"], Value::Null, "inner null slot stays present")
                    }
                    other => panic!("expected Map for full_name, got {other:?}"),
                }
                assert_eq!(o["note"], Value::Null, "scalar null stays present");
            }
            _ => panic!("expected map"),
        }

        // With `first` present → full_name is a real object.
        let mut m = IndexMap::new();
        m.insert("first".into(), Value::Str("Ada".into()));
        let r2 = engine.map_object(&Value::Map(m), Some("Person")).unwrap();
        match &r2 {
            Value::Map(o) => assert!(o.contains_key("full_name"), "populated nested object kept"),
            _ => panic!("expected map"),
        }
    }

    /// #232: strict mode errors on an unbound name in an expr; lax yields null.
    #[test]
    fn strict_exprs_error_on_unbound_name() {
        let schema = make_person_schema();
        let build_spec = || {
            let mut slots: IndexMap<String, SlotDerivation> = IndexMap::new();
            slots.insert(
                "label".into(),
                SlotDerivation {
                    name: "label".into(),
                    expr: Some("{nonexistent_slot}".into()),
                    ..Default::default()
                },
            );
            TransformationSpecification {
                class_derivations: Some(vec![ClassDerivation {
                    name: "Person".into(),
                    populated_from: Some("Person".into()),
                    slot_derivations: Some(slots),
                    ..Default::default()
                }]),
                ..Default::default()
            }
        };
        let obj = Value::Map(IndexMap::new());

        // Lax (default): unbound name → null, no error.
        let lax = ObjectTransformer::new(build_spec(), Some(&schema), None);
        match lax.map_object(&obj, Some("Person")).unwrap() {
            Value::Map(o) => assert_eq!(o["label"], Value::Null),
            _ => panic!("expected map"),
        }

        // Strict: unbound name → error.
        let strict =
            ObjectTransformer::new(build_spec(), Some(&schema), None).with_strict_exprs(true);
        assert!(
            strict.map_object(&obj, Some("Person")).is_err(),
            "strict mode must error on unbound name"
        );
    }
}

#[cfg(test)]
mod op_tests {
    //! Tests for offset / aggregation / pivot operations.
    use super::*;
    use crate::datamodel::{
        AggregationOperation, AggregationType, ClassDerivation, Offset, PivotDirectionType,
        PivotOperation, SlotDerivation, TransformationSpecification,
    };

    fn pivot(direction: PivotDirectionType) -> PivotOperation {
        PivotOperation {
            direction,
            variable_slot: "variable".into(),
            value_slot: "value".into(),
            unmelt_to_class: None,
            unmelt_to_slots: None,
            unit_slot: None,
            slot_name_template: "{variable}".into(),
            source_slots: None,
            id_slots: None,
        }
    }

    fn run(spec: TransformationSpecification, src: IndexMap<String, Value>, ty: &str) -> Value {
        ObjectTransformer::new(spec, None, None)
            .map_object(&Value::Map(src), Some(ty))
            .unwrap()
    }

    fn map(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn class_with_slot(name: &str, sd: SlotDerivation) -> TransformationSpecification {
        let mut slots = IndexMap::new();
        slots.insert(sd.name.clone(), sd);
        TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: name.into(),
                populated_from: Some(name.into()),
                slot_derivations: Some(slots),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    #[test]
    fn offset_adds_and_reverses() {
        // result = base + offset_value * factor
        let sd = SlotDerivation {
            name: "out".into(),
            populated_from: Some("base".into()),
            offset: Some(Offset {
                offset_value: 2.0,
                offset_field: "factor".into(),
                offset_reverse: None,
            }),
            ..Default::default()
        };
        let out = run(
            class_with_slot("C", sd),
            map(&[("base", Value::Int(10)), ("factor", Value::Int(3))]),
            "C",
        );
        // 10 + 2*3 = 16
        assert_eq!(out, Value::Map(map(&[("out", Value::Float(16.0))])));

        let sd = SlotDerivation {
            name: "out".into(),
            populated_from: Some("base".into()),
            offset: Some(Offset {
                offset_value: 2.0,
                offset_field: "factor".into(),
                offset_reverse: Some(true),
            }),
            ..Default::default()
        };
        let out = run(
            class_with_slot("C", sd),
            map(&[("base", Value::Int(10)), ("factor", Value::Int(3))]),
            "C",
        );
        // 10 - 2*3 = 4
        assert_eq!(out, Value::Map(map(&[("out", Value::Float(4.0))])));
    }

    fn agg(op: AggregationType) -> AggregationOperation {
        AggregationOperation {
            operator: op,
            null_handling: None,
            invalid_value_handling: None,
        }
    }

    #[test]
    fn aggregation_operators() {
        let list = Value::List(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let cases = [
            (AggregationType::Sum, Value::Int(6)),
            (AggregationType::Count, Value::Int(3)),
            (AggregationType::Average, Value::Float(2.0)),
            (AggregationType::Min, Value::Int(1)),
            (AggregationType::Max, Value::Int(3)),
            (AggregationType::Median, Value::Float(2.0)),
        ];
        for (op, expected) in cases {
            let sd = SlotDerivation {
                name: "out".into(),
                populated_from: Some("vals".into()),
                aggregation_operation: Some(agg(op)),
                ..Default::default()
            };
            let out = run(
                class_with_slot("C", sd),
                map(&[("vals", list.clone())]),
                "C",
            );
            assert_eq!(out, Value::Map(map(&[("out", expected)])), "op mismatch");
        }
    }

    #[test]
    fn pivot_melt_to_eav() {
        let mut pv = pivot(PivotDirectionType::Melt);
        pv.source_slots = Some(vec!["height".into(), "weight".into()]);
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "C".into(),
                populated_from: Some("C".into()),
                pivot_operation: Some(pv),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let out = run(
            spec,
            map(&[
                ("height", Value::Float(1.8)),
                ("weight", Value::Float(75.0)),
            ]),
            "C",
        );
        assert_eq!(
            out,
            Value::List(vec![
                Value::Map(map(&[
                    ("variable", Value::Str("height".into())),
                    ("value", Value::Float(1.8))
                ])),
                Value::Map(map(&[
                    ("variable", Value::Str("weight".into())),
                    ("value", Value::Float(75.0))
                ])),
            ])
        );
    }

    #[test]
    fn pivot_unmelt_collection_to_wide() {
        let spec = TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: "C".into(),
                populated_from: Some("C".into()),
                pivot_operation: Some(pivot(PivotDirectionType::Unmelt)),
                ..Default::default()
            }]),
            ..Default::default()
        };
        let records = Value::List(vec![
            Value::Map(map(&[
                ("variable", Value::Str("h".into())),
                ("value", Value::Float(1.8)),
            ])),
            Value::Map(map(&[
                ("variable", Value::Str("w".into())),
                ("value", Value::Float(75.0)),
            ])),
        ]);
        let out = run(spec, map(&[("measurements", records)]), "C");
        assert_eq!(
            out,
            Value::Map(map(&[("h", Value::Float(1.8)), ("w", Value::Float(75.0))]))
        );
    }
}

#[cfg(test)]
mod lookup_index_tests {
    //! Tests for LookupIndex join wiring (#188).
    use super::*;
    use crate::datamodel::{
        AggregationOperation, AggregationType, AliasedClass, ClassDerivation, SlotDerivation,
        TransformationSpecification,
    };
    use crate::engine::lookup_index::LookupIndex;
    use indexmap::IndexMap;
    use std::sync::Arc;

    /// A target schema whose class `class` has one single-valued string slot `slot`.
    /// Upstream's `_derive_nested_objects` looks the slot up on the target schema
    /// (`target_schemaview.induced_slot`), so a nested derivation needs one.
    fn target_with_slot(class: &str, slot_name: &str) -> crate::schema::InMemorySchema {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};
        InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: class.into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                class,
                SlotDef {
                    name: slot_name.into(),
                    range: RangeKind::Type("string".into()),
                    multivalued: false,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: false,
                },
            )
            .build()
    }

    fn get(v: &Value, key: &str) -> Value {
        match v {
            Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    fn imap(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn val_map(pairs: &[(&str, Value)]) -> Value {
        Value::Map(imap(pairs))
    }

    fn slot(name: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            ..Default::default()
        }
    }

    fn slot_pf(name: &str, pf: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            populated_from: Some(pf.to_string()),
            ..Default::default()
        }
    }

    fn slot_expr(name: &str, expr: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            expr: Some(expr.to_string()),
            ..Default::default()
        }
    }

    fn slot_agg(name: &str, pf: &str, agg: AggregationType) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            populated_from: Some(pf.to_string()),
            aggregation_operation: Some(AggregationOperation {
                operator: agg,
                null_handling: None,
                invalid_value_handling: None,
            }),
            ..Default::default()
        }
    }

    fn make_spec(
        class_name: &str,
        slots: Vec<SlotDerivation>,
        joins: Option<IndexMap<String, AliasedClass>>,
    ) -> TransformationSpecification {
        let mut cd = ClassDerivation {
            name: class_name.to_string(),
            ..Default::default()
        };
        cd.joins = joins;
        let mut sd: IndexMap<String, SlotDerivation> = IndexMap::new();
        for s in slots {
            sd.insert(s.name.clone(), s);
        }
        cd.slot_derivations = Some(sd);
        let mut spec = TransformationSpecification::default();
        spec.class_derivations = Some(vec![cd]);
        spec
    }

    fn join_alias(alias: &str, table: &str, src_key: &str) -> AliasedClass {
        AliasedClass {
            alias: alias.to_string(),
            class_named: Some(table.to_string()),
            source_key: Some(src_key.to_string()),
            lookup_key: None,
            join_on: None,
        }
    }

    // ── Test 1: populated_from "alias.field" single-row join ─────────────────

    #[test]
    fn test_join_populated_from_dot_notation() {
        // demographics table keyed by patient_id
        let demo_rows = vec![val_map(&[
            ("patient_id", Value::Str("P1".into())),
            ("age", Value::Int(42)),
            ("city", Value::Str("London".into())),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("demographics", &demo_rows, "patient_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert(
            "demo".to_string(),
            join_alias("demo", "demographics", "pid"),
        );

        let spec = make_spec(
            "Patient",
            vec![
                slot("pid"),
                slot_pf("age", "demo.age"),
                slot_pf("city", "demo.city"),
            ],
            Some(joins),
        );

        let source = val_map(&[("pid", Value::Str("P1".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Patient"))
            .unwrap();

        assert_eq!(get(&result, "age"), Value::Int(42));
        assert_eq!(get(&result, "city"), Value::Str("London".into()));
    }

    // ── Test 2: expr "{alias.field}" join binding ─────────────────────────────

    #[test]
    fn test_join_expr_binding() {
        let demo_rows = vec![val_map(&[
            ("patient_id", Value::Str("P2".into())),
            ("score", Value::Int(10)),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("scores", &demo_rows, "patient_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert("sc".to_string(), join_alias("sc", "scores", "pid"));

        let spec = make_spec(
            "Result",
            vec![slot_expr("doubled", "{sc.score} * 2")],
            Some(joins),
        );

        let source = val_map(&[("pid", Value::Str("P2".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Result"))
            .unwrap();

        assert_eq!(get(&result, "doubled"), Value::Int(20));
    }

    // ── Test 3: aggregation_operation + multi-row join (COUNT / SUM) ──────────

    #[test]
    fn test_join_multi_row_aggregation() {
        // Multiple visits for patient P3
        let visits = vec![
            val_map(&[
                ("patient_id", Value::Str("P3".into())),
                ("cost", Value::Float(100.0)),
            ]),
            val_map(&[
                ("patient_id", Value::Str("P3".into())),
                ("cost", Value::Float(200.0)),
            ]),
            val_map(&[
                ("patient_id", Value::Str("P3".into())),
                ("cost", Value::Float(50.0)),
            ]),
        ];
        let mut li = LookupIndex::new();
        li.register_table("visits", &visits, "patient_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert("v".to_string(), join_alias("v", "visits", "pid"));

        let spec = make_spec(
            "Summary",
            vec![
                slot_agg("visit_count", "v.cost", AggregationType::Count),
                slot_agg("total_cost", "v.cost", AggregationType::Sum),
            ],
            Some(joins),
        );

        let source = val_map(&[("pid", Value::Str("P3".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Summary"))
            .unwrap();

        assert_eq!(get(&result, "visit_count"), Value::Int(3));
        assert_eq!(get(&result, "total_cost"), Value::Float(350.0));
    }

    // ── Test 4: no index → dot-notation falls back to source_map get ─────────

    #[test]
    fn test_join_no_index_fallback() {
        // Without a LookupIndex, "alias.field" just does a raw source_map lookup
        // (which returns Null since "demo.age" is not a key).
        let spec = make_spec("Patient", vec![slot_pf("age", "demo.age")], None);
        let source = val_map(&[("pid", Value::Str("P1".into()))]);
        let result = ObjectTransformer::new(spec, None, None)
            .map_object(&source, Some("Patient"))
            .unwrap();
        assert_eq!(get(&result, "age"), Value::Null);
    }

    // ── #217 (a19eb095): nested cross-table join MISS retains the key ─────────

    /// Build a slot whose value is a nested class_derivation joined to `table`.
    fn slot_nested(name: &str, nested_class: &str, table: &str, inner: &str) -> SlotDerivation {
        let mut inner_slots: IndexMap<String, SlotDerivation> = IndexMap::new();
        inner_slots.insert(inner.to_string(), slot_pf(inner, inner));
        let nested = ClassDerivation {
            name: nested_class.to_string(),
            populated_from: Some(table.to_string()),
            slot_derivations: Some(inner_slots),
            ..Default::default()
        };
        let cds = vec![nested];
        SlotDerivation {
            name: name.to_string(),
            class_derivations: Some(cds),
            ..Default::default()
        }
    }

    /// Singular nested slot, cross-table join with no matching row → the key is
    /// RETAINED with value `null` (not omitted, not `{value: null}`). Mirrors
    /// upstream `_derive_nested_objects` `continue` on a sparse miss.
    #[test]
    fn nested_join_miss_singular_retains_null() {
        // readings keyed by subject_id — only S_OTHER present.
        let readings = vec![val_map(&[
            ("subject_id", Value::Str("S_OTHER".into())),
            ("score", Value::Float(95.5)),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("readings", &readings, "subject_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        // Join keyed by the nested source ("readings"), mirroring upstream
        // `joins[nested_source]`. Source key column is `subject_id`.
        joins.insert(
            "readings".to_string(),
            join_alias("readings", "readings", "subject_id"),
        );

        let spec = make_spec(
            "Measurement",
            vec![
                slot_pf("id", "id"),
                slot_nested("observation", "Observation", "readings", "score"),
            ],
            Some(joins),
        );

        // subject_id S_NODATA has no matching reading → sparse miss.
        let source = val_map(&[
            ("id", Value::Str("M2".into())),
            ("subject_id", Value::Str("S_NODATA".into())),
        ]);
        let target = target_with_slot("Measurement", "observation");
        let result = ObjectTransformer::new(spec, None, Some(&target))
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Measurement"))
            .unwrap();

        match &result {
            Value::Map(o) => {
                assert!(o.contains_key("observation"), "key retained on miss");
                assert_eq!(o["observation"], Value::Null, "miss → null, not omitted");
                assert_eq!(o["id"], Value::Str("M2".into()));
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    /// A matching joined row is NOT collapsed: the nested object is built and
    /// retained (control for the miss test above -- `derive_nested_objects` skips
    /// the derivation only when the join finds no row). The inner `score` reads
    /// the merge of the parent and joined rows, so it is the joined row's.
    #[test]
    fn nested_join_hit_builds_object() {
        let readings = vec![val_map(&[
            ("subject_id", Value::Str("S1".into())),
            ("score", Value::Float(95.5)),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("readings", &readings, "subject_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert(
            "readings".to_string(),
            join_alias("readings", "readings", "subject_id"),
        );

        let spec = make_spec(
            "Measurement",
            vec![slot_nested(
                "observation",
                "Observation",
                "readings",
                "score",
            )],
            Some(joins),
        );

        // subject_id S1 HAS a matching reading → no miss → object built.
        let source = val_map(&[("subject_id", Value::Str("S1".into()))]);
        let target = target_with_slot("Measurement", "observation");
        let result = ObjectTransformer::new(spec, None, Some(&target))
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Measurement"))
            .unwrap();

        match &result {
            Value::Map(o) => {
                assert!(o.contains_key("observation"), "hit keeps the slot");
                assert!(
                    matches!(o["observation"], Value::Map(_)),
                    "hit builds a real nested object (not collapsed to null), got {:?}",
                    o["observation"]
                );
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    /// Multivalued nested slot whose join misses → `[]` retained (not `[null]`,
    /// not omitted). Requires a target schema marking the slot multivalued.
    #[test]
    fn nested_join_miss_multivalued_retains_empty_list() {
        use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};

        // Target: Result.observations is multivalued (List of Observation).
        let target = InMemorySchemaBuilder::new()
            .add_type("float")
            .add_class(ClassDef {
                name: "Result".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Result",
                SlotDef {
                    name: "observations".into(),
                    range: RangeKind::Class("Observation".into()),
                    multivalued: true,
                    required: false,
                    identifier: false,
                    key: false,
                    unit: None,
                    any_of_enums: vec![],
                    inlined: false,
                    inlined_as_list: true,
                },
            )
            .build();

        let readings = vec![val_map(&[
            ("subject_id", Value::Str("S_OTHER".into())),
            ("score", Value::Float(95.5)),
        ])];
        let mut li = LookupIndex::new();
        li.register_table("readings", &readings, "subject_id");

        let mut joins: IndexMap<String, AliasedClass> = IndexMap::new();
        joins.insert(
            "readings".to_string(),
            join_alias("readings", "readings", "subject_id"),
        );

        // Parent class must be "Result" so the target-schema slot lookup matches.
        let spec = make_spec(
            "Result",
            vec![slot_nested(
                "observations",
                "Observation",
                "readings",
                "score",
            )],
            Some(joins),
        );

        let source = val_map(&[("subject_id", Value::Str("S_NODATA".into()))]);
        let result = ObjectTransformer::new(spec, None, Some(&target))
            .with_lookup_index(Arc::new(li))
            .map_object(&source, Some("Result"))
            .unwrap();

        match &result {
            Value::Map(o) => {
                assert!(o.contains_key("observations"), "multivalued key retained");
                assert_eq!(
                    o["observations"],
                    Value::List(vec![]),
                    "all-miss multivalued → [] retained"
                );
            }
            other => panic!("expected map, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod inline_dotpath_tests {
    //! Tests for inlined `populated_from` dot-path traversal (issue #247).
    //!
    //! Mirrors upstream `tests/test_transformer/test_inline_dotpath.py`
    //! (commit b5fca196). `populated_from` dot-paths normally walk FK joins via
    //! a LookupIndex; #247 extends them to traverse *inlined* nested data
    //! (XML/JSON/OWL/EML-shaped trees) by walking into the nested object
    //! structurally instead of treating it as a foreign key.
    use super::*;
    use crate::datamodel::SlotDerivation;
    use crate::schema::{ClassDef, InMemorySchemaBuilder, RangeKind, SlotDef};

    fn get(v: &Value, key: &str) -> Value {
        match v {
            Value::Map(m) => m.get(key).cloned().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    fn imap(pairs: &[(&str, Value)]) -> IndexMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn slot_pf(name: &str, pf: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.to_string(),
            populated_from: Some(pf.to_string()),
            ..Default::default()
        }
    }

    fn make_spec(class_name: &str, slots: Vec<SlotDerivation>) -> TransformationSpecification {
        let mut cd = ClassDerivation {
            name: class_name.to_string(),
            ..Default::default()
        };
        let mut sd: IndexMap<String, SlotDerivation> = IndexMap::new();
        for s in slots {
            sd.insert(s.name.clone(), s);
        }
        cd.slot_derivations = Some(sd);
        TransformationSpecification {
            class_derivations: Some(vec![cd]),
            ..Default::default()
        }
    }

    fn class(name: &str, tree_root: bool) -> ClassDef {
        ClassDef {
            name: name.into(),
            tree_root,
            is_a: None,
            mixins: vec![],
        }
    }

    fn scalar_slot(name: &str) -> SlotDef {
        SlotDef {
            name: name.into(),
            range: RangeKind::Type("string".into()),
            multivalued: false,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
            inlined: false,
            inlined_as_list: false,
        }
    }

    fn class_slot(name: &str, range: &str, multivalued: bool, inlined_as_list: bool) -> SlotDef {
        SlotDef {
            name: name.into(),
            range: RangeKind::Class(range.into()),
            multivalued,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
            inlined: !inlined_as_list,
            inlined_as_list,
        }
    }

    /// Build the EML-ish inlined source schema:
    /// `EMLDocument -> dataset (inlined) -> {title, dataTable (inlined list)}`.
    /// When `declare_inlined` is false the `dataset` slot omits `inlined`, to
    /// exercise the runtime dict fallback.
    fn eml_schema(declare_inlined: bool) -> InMemorySchemaBuilder {
        let dataset = SlotDef {
            inlined: declare_inlined,
            ..class_slot("dataset", "Dataset", false, false)
        };
        InMemorySchemaBuilder::new()
            .add_class(class("EMLDocument", true))
            .add_slot("EMLDocument", dataset)
            .add_class(class("Dataset", false))
            .add_slot("Dataset", scalar_slot("title"))
            .add_slot("Dataset", class_slot("dataTable", "DataTable", true, true))
            .add_class(class("DataTable", false))
            .add_slot("DataTable", scalar_slot("entityName"))
    }

    fn input_data() -> Value {
        Value::Map(imap(&[(
            "dataset",
            Value::Map(imap(&[
                ("title", Value::Str("My Dataset".into())),
                (
                    "dataTable",
                    Value::List(vec![
                        Value::Map(imap(&[("entityName", Value::Str("table_one".into()))])),
                        Value::Map(imap(&[("entityName", Value::Str("table_two".into()))])),
                    ]),
                ),
            ])),
        )]))
    }

    /// `title: {populated_from: dataset.title}` spec.
    fn title_spec() -> TransformationSpecification {
        make_spec("EMLDocument", vec![slot_pf("title", "dataset.title")])
    }

    // #247 test 1: dot-path into an inlined object reaches the scalar leaf,
    // declaratively (dataset is marked `inlined: true`), with no ObjectIndex.
    #[test]
    fn test_slot_level_inline_deep_scalar() {
        let schema = eml_schema(true).build();
        let result = ObjectTransformer::new(title_spec(), Some(&schema), None)
            .map_object(&input_data(), Some("EMLDocument"))
            .unwrap();
        assert_eq!(get(&result, "title"), Value::Str("My Dataset".into()));
    }

    // #247 test 2: runtime fallback — a dict value traverses even when the
    // source schema omits `inlined: true` on the first segment.
    #[test]
    fn test_inline_path_via_runtime_fallback_without_inlined_declaration() {
        let schema = eml_schema(false).build();
        let result = ObjectTransformer::new(title_spec(), Some(&schema), None)
            .map_object(&input_data(), Some("EMLDocument"))
            .unwrap();
        assert_eq!(get(&result, "title"), Value::Str("My Dataset".into()));
    }

    // #247 test 3: a legitimately missing leaf yields Null rather than erroring.
    #[test]
    fn test_inline_path_absent_segment_yields_none() {
        let schema = eml_schema(true).build();
        let source = Value::Map(imap(&[("dataset", Value::Map(IndexMap::new()))]));
        let result = ObjectTransformer::new(title_spec(), Some(&schema), None)
            .map_object(&source, Some("EMLDocument"))
            .unwrap();
        assert_eq!(get(&result, "title"), Value::Null);
    }

    // #265 test: a list segment mid-path is the not-yet-supported inline
    // fan-out case — it raises naming the multivalued segment, not silent Null.
    #[test]
    fn test_multivalued_inline_segment_raises_clear_diagnostic() {
        let schema = eml_schema(true).build();
        let spec = make_spec("EMLDocument", vec![slot_pf("classes", "dataset.dataTable")]);
        let err = ObjectTransformer::new(spec, Some(&schema), None)
            .map_object(&input_data(), Some("EMLDocument"))
            .unwrap_err();
        // The engine wraps per-slot errors in `SlotTransform` (mirroring Python
        // `map_object`'s incremental context enrichment); the `InlinePath`
        // diagnostic — segment name + #265 + slot context — is preserved verbatim.
        let msg = err.to_string();
        assert!(
            msg.contains("dataTable") && msg.contains("#265"),
            "diagnostic should name the segment and cite #265: {msg}"
        );
        assert!(
            msg.contains("slot_derivation=classes"),
            "diagnostic should carry slot-derivation context: {msg}"
        );
    }

    // #247: a non-dict value mid-path (scalar where a nested object is expected)
    // errors rather than silently returning Null.
    #[test]
    fn test_non_dict_mid_path_raises() {
        let schema = eml_schema(true).build();
        let spec = make_spec(
            "EMLDocument",
            vec![slot_pf("title", "dataset.title.deeper")],
        );
        // `dataset.title` is a scalar string; descending into `.deeper` must error.
        let err = ObjectTransformer::new(spec, Some(&schema), None)
            .map_object(&input_data(), Some("EMLDocument"))
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("expected a nested object") && msg.contains("deeper"),
            "diagnostic should explain the non-object segment: {msg}"
        );
    }
}

/// Failures upstream raises where the engine used to carry on: each test cites the
/// upstream raise site (`linkml_map.transformer`, v0.6.0-rc1).
#[cfg(test)]
mod upstream_errors {
    use super::*;
    use crate::datamodel::{
        AliasedClass, ClassDerivation, EnumDerivation, SlotDerivation, TransformationSpecification,
    };
    use crate::schema::{
        ClassDef, EnumDef, InMemorySchema, InMemorySchemaBuilder, PermissibleValue, RangeKind,
        SlotDef,
    };

    fn slot_def(name: &str, range: RangeKind, multivalued: bool) -> SlotDef {
        SlotDef {
            name: name.into(),
            range,
            multivalued,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
            inlined: false,
            inlined_as_list: false,
        }
    }

    /// One class with string slots (and optionally an enum-ranged `status` slot).
    fn schema(class: &str, slots: &[&str], enum_slot: bool) -> InMemorySchema {
        let mut b = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: class.into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            });
        for s in slots {
            b = b.add_slot(class, slot_def(s, RangeKind::Type("string".into()), false));
        }
        if enum_slot {
            b = b
                .add_enum(EnumDef {
                    name: "Status".into(),
                    permissible_values: vec![PermissibleValue {
                        text: "A".into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .add_slot(
                    class,
                    slot_def("status", RangeKind::Enum("Status".into()), false),
                );
        }
        b.build()
    }

    fn spec_with(
        class: &str,
        populated_from: &str,
        slots: Vec<SlotDerivation>,
    ) -> TransformationSpecification {
        TransformationSpecification {
            class_derivations: Some(vec![ClassDerivation {
                name: class.into(),
                populated_from: Some(populated_from.into()),
                slot_derivations: Some(slots.into_iter().map(|s| (s.name.clone(), s)).collect()),
                ..Default::default()
            }]),
            ..Default::default()
        }
    }

    fn pf(name: &str, from: &str) -> SlotDerivation {
        SlotDerivation {
            name: name.into(),
            populated_from: Some(from.into()),
            ..Default::default()
        }
    }

    fn row(pairs: &[(&str, &str)]) -> Value {
        Value::Map(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), Value::Str((*v).to_owned())))
                .collect(),
        )
    }

    fn err_text(
        spec: TransformationSpecification,
        source: &InMemorySchema,
        target: Option<&InMemorySchema>,
        input: &Value,
    ) -> String {
        ObjectTransformer::new(spec, Some(source), target.map(|t| t as &dyn SchemaProvider))
            .map_object(input, Some("Person"))
            .unwrap_err()
            .to_string()
    }

    /// `_resolve_fk_or_literal` / `_derive_slot`: `induced_slot(name, source_type)` raises for
    /// a slot the source class does not have, for `populated_from` and the implicit copy
    /// (corpus cases ec97df02fa7b, daba191bd3ff: `--source-type` forced onto every block).
    #[test]
    fn source_slot_missing_from_the_source_class_raises() {
        let source = schema("Person", &["id"], false);
        let input = row(&[("id", "p1")]);
        for slot in [
            pf("out", "nosuch"),
            SlotDerivation {
                name: "nosuch".into(),
                ..Default::default()
            },
        ] {
            let msg = err_text(
                spec_with("Agent", "Person", vec![slot]),
                &source,
                None,
                &input,
            );
            assert!(
                msg.contains("No such slot nosuch as an attribute of Person ancestors"),
                "{msg}"
            );
        }
    }

    /// `_derive_nested_objects` ends in `self.target_schemaview.induced_slot(...)`: with no
    /// target schema that is an `AttributeError` (corpus cases 3b8230615745, 5f42931ad2d0).
    #[test]
    fn nested_derivation_without_a_target_schema_raises() {
        let source = schema("Person", &["id"], false);
        let nested = ClassDerivation {
            name: "Inner".into(),
            populated_from: Some("Person".into()),
            slot_derivations: Some([("a".to_owned(), pf("a", "id"))].into_iter().collect()),
            ..Default::default()
        };
        let outer = SlotDerivation {
            name: "inner".into(),
            class_derivations: Some(vec![nested]),
            ..Default::default()
        };
        let input = row(&[("id", "p1")]);
        let msg = err_text(
            spec_with("Agent", "Person", vec![outer.clone()]),
            &source,
            None,
            &input,
        );
        assert!(
            msg.contains("'NoneType' object has no attribute 'induced_slot'"),
            "{msg}"
        );

        // With a target schema that has the slot it works.
        let target = InMemorySchemaBuilder::new()
            .add_type("string")
            .add_class(ClassDef {
                name: "Agent".into(),
                tree_root: true,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Agent",
                slot_def("inner", RangeKind::Type("string".into()), false),
            )
            .add_class(ClassDef {
                name: "Inner".into(),
                tree_root: false,
                is_a: None,
                mixins: vec![],
            })
            .add_slot(
                "Inner",
                slot_def("a", RangeKind::Type("string".into()), false),
            )
            .build();
        let ok = ObjectTransformer::new(
            spec_with("Agent", "Person", vec![outer]),
            Some(&source),
            Some(&target),
        )
        .map_object(&input, Some("Person"));
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// `_coerce_cardinality` -> `_is_coerce_to_multivalued` -> `induced_slot`: a derived slot the
    /// target class lacks fails the row (the `type_coercion` example's `id_as_url`), unless a
    /// `MultiValued` cast short-circuits the probe for a scalar and the spec gives a `range`.
    #[test]
    fn derived_slot_missing_from_the_target_class_raises() {
        let source = schema("Person", &["id"], false);
        let target = schema("Agent", &["id"], false);
        let input = row(&[("id", "p1")]);
        let msg = err_text(
            spec_with("Agent", "Person", vec![pf("id", "id"), pf("extra", "id")]),
            &source,
            Some(&target),
            &input,
        );
        assert!(
            msg.contains("No such slot extra as an attribute of Agent ancestors"),
            "{msg}"
        );

        let mut cast = pf("extra", "id");
        cast.cast_collection_as = Some(crate::datamodel::CollectionType::MultiValued);
        cast.range = Some("string".into());
        let ok = ObjectTransformer::new(
            spec_with("Agent", "Person", vec![pf("id", "id"), cast]),
            Some(&source),
            Some(&target),
        )
        .map_object(&input, Some("Person"));
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// `Bindings.__getitem__` -> `dynamic_object` -> `induced_slot`: an `expr:` naming a row key
    /// that is not a slot of the source type raises; a name the row lacks, or an expression that
    /// does not use the stray key, does not (corpus case d398794d299d).
    #[test]
    fn expr_naming_a_non_slot_row_key_raises() {
        let source = schema("Person", &["id"], false);
        let expr = |e: &str| SlotDerivation {
            name: "out".into(),
            expr: Some(e.into()),
            ..Default::default()
        };
        let input = row(&[("id", "p1"), ("age", "3")]);
        let msg = err_text(
            spec_with("Agent", "Person", vec![expr("{age} * 2")]),
            &source,
            None,
            &input,
        );
        assert!(
            msg.contains("No such slot age as an attribute of Person ancestors"),
            "{msg}"
        );
        let ok = ObjectTransformer::new(
            spec_with("Agent", "Person", vec![expr("{id}")]),
            Some(&source),
            None,
        )
        .map_object(&input, Some("Person"));
        assert!(ok.is_ok(), "{ok:?}");
    }

    /// `_get_enum_derivation` raises `ValueError` unless exactly one derivation matches
    /// (corpus cases 7d8cef5c3417, 8ee12775ef35).
    #[test]
    fn enum_ranged_slot_needs_exactly_one_enum_derivation() {
        let source = schema("Person", &["id"], true);
        let input = row(&[("id", "p1"), ("status", "A")]);
        let msg = err_text(
            spec_with("Agent", "Person", vec![pf("out", "status")]),
            &source,
            None,
            &input,
        );
        assert!(
            msg.contains("Could not find what to derive from a source Status"),
            "{msg}"
        );

        let derivation = |name: &str| EnumDerivation {
            name: name.into(),
            populated_from: Some("Status".into()),
            mirror_source: Some(true),
            ..Default::default()
        };
        let mut spec = spec_with("Agent", "Person", vec![pf("out", "status")]);
        spec.enum_derivations = Some([("S1".to_owned(), derivation("S1"))].into_iter().collect());
        let out = ObjectTransformer::new(spec.clone(), Some(&source), None)
            .map_object(&input, Some("Person"))
            .unwrap();
        assert_eq!(out, row(&[("out", "A")]));

        // Two derivations of one enum are as bad as none.
        spec.enum_derivations
            .as_mut()
            .unwrap()
            .insert("S2".into(), derivation("S2"));
        let msg = err_text(spec, &source, None, &input);
        assert!(
            msg.contains("Could not find what to derive from a source Status"),
            "{msg}"
        );
    }

    fn join_spec(index_slot: SlotDerivation) -> TransformationSpecification {
        let mut spec = spec_with("Agent", "Person", vec![index_slot]);
        spec.class_derivations.as_mut().unwrap()[0].joins = Some(
            [(
                "Order".to_owned(),
                AliasedClass {
                    alias: "Order".into(),
                    source_key: Some("order_ref".into()),
                    lookup_key: Some("id".into()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        );
        spec
    }

    /// `Bindings._resolve_join` for an `expr:` reading a join alias (corpus case
    /// 2a5a0d33540d, a single-object input): a row without the key sees `None`; with one and
    /// no `lookup_index` the `ValueError` is a restricted-eval rejection, which
    /// `unrestricted_eval` retries (a single expression then yields `None`); a missing table
    /// raises in both modes.
    #[test]
    fn expr_reading_an_unresolvable_join_alias() {
        let source = schema("Person", &["id", "order_ref"], false);
        let target = schema("Agent", &["o"], false);
        let spec = || {
            join_spec(SlotDerivation {
                name: "o".into(),
                expr: Some("{Order.id}".into()),
                ..Default::default()
            })
        };
        let null_row = Value::Map([("o".to_owned(), Value::Null)].into_iter().collect());

        let keyless = row(&[("id", "p1")]);
        let out = ObjectTransformer::new(spec(), Some(&source), Some(&target))
            .map_object(&keyless, Some("Person"))
            .unwrap();
        assert_eq!(out, null_row);

        let keyed = row(&[("id", "p1"), ("order_ref", "o1")]);
        let msg = err_text(spec(), &source, Some(&target), &keyed);
        assert!(
            msg.contains("Join configured for \"Order\" but lookup_index has not been initialized"),
            "{msg}"
        );
        let out = ObjectTransformer::new(spec(), Some(&source), Some(&target))
            .with_unrestricted_eval(true)
            .map_object(&keyed, Some("Person"))
            .unwrap();
        assert_eq!(out, null_row);

        let mut index = LookupIndex::new();
        index.register_table("Other", &[], "id");
        for unrestricted in [false, true] {
            let err = ObjectTransformer::new(spec(), Some(&source), Some(&target))
                .with_unrestricted_eval(unrestricted)
                .with_lookup_index(Arc::new(index.clone()))
                .map_object(&keyed, Some("Person"))
                .unwrap_err();
            assert!(
                err.to_string()
                    .contains("Table with name Order does not exist"),
                "{err}"
            );
        }
    }

    /// `_resolve_joined_row`: no value for the join key is a miss; a value with no
    /// `lookup_index` raises `ValueError` (corpus case e5c5ef4b88ee); with an index that does
    /// not hold the table DuckDB raises.
    #[test]
    fn join_with_a_key_needs_a_registered_lookup_table() {
        let source = schema("Person", &["id", "order_ref"], false);
        let target = schema("Agent", &["o"], false);
        let spec = || join_spec(pf("o", "Order.id"));

        let keyless = row(&[("id", "p1")]);
        let out = ObjectTransformer::new(spec(), Some(&source), Some(&target))
            .map_object(&keyless, Some("Person"))
            .unwrap();
        assert_eq!(
            out,
            Value::Map([("o".to_owned(), Value::Null)].into_iter().collect())
        );

        let keyed = row(&[("id", "p1"), ("order_ref", "o1")]);
        let msg = err_text(spec(), &source, Some(&target), &keyed);
        assert!(
            msg.contains("ValueError: Join configured for \"Order\" but lookup_index has not been initialized"),
            "{msg}"
        );

        let mut index = LookupIndex::new();
        index.register_table("Other", &[], "id");
        let err = ObjectTransformer::new(spec(), Some(&source), Some(&target))
            .with_lookup_index(Arc::new(index))
            .map_object(&keyed, Some("Person"))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("Table with name Order does not exist"),
            "{err}"
        );

        let mut index = LookupIndex::new();
        index.register_table("Order", &[row(&[("id", "o1")])], "id");
        let out = ObjectTransformer::new(spec(), Some(&source), Some(&target))
            .with_lookup_index(Arc::new(index))
            .map_object(&keyed, Some("Person"))
            .unwrap();
        assert_eq!(
            out,
            Value::Map(
                [("o".to_owned(), Value::Str("o1".into()))]
                    .into_iter()
                    .collect()
            )
        );
    }

    /// Upstream parses an `expr:` when a row first evaluates it, so a spec whose expression
    /// the parser rejects still loads (`CompiledExprs::build` no longer fails the plan); the
    /// row path reports it, and valid expressions stay cached.
    #[test]
    fn malformed_expressions_fail_on_first_use_not_at_build() {
        let source = schema("Person", &["id"], false);
        let bad = SlotDerivation {
            name: "bad".into(),
            expr: Some("{id} +".into()),
            ..Default::default()
        };
        let good = SlotDerivation {
            name: "good".into(),
            expr: Some("{id}".into()),
            ..Default::default()
        };
        let mut spec = spec_with("Agent", "Person", vec![good, bad]);
        spec.enum_derivations = Some(
            [(
                "E".to_owned(),
                EnumDerivation {
                    name: "E".into(),
                    expr: Some("(".into()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        );
        let compiled = CompiledExprs::build(&spec).expect("build never fails on a parse error");
        assert!(
            compiled.slot("Agent", "good", "{id}").is_some(),
            "valid expr stays cached"
        );
        // A text that is not Python parses into a block that remembers its
        // `SyntaxError`: raised when evaluated (restricted), swallowed by asteval.
        assert!(compiled.slot("Agent", "bad", "{id} +").is_some());
        let engine =
            ObjectTransformer::new(spec, Some(&source), None).with_compiled_exprs(&compiled);
        let err = engine
            .map_object(&row(&[("id", "p1")]), Some("Person"))
            .unwrap_err();
        assert!(err.to_string().contains("bad"), "{err}");
    }
}

/// Engine behaviours ported from upstream's `ObjectTransformer` and checked
/// against it by the differential corpus (`tests/differential/corpus/`).
#[cfg(test)]
mod parity_tests {
    use super::*;
    use crate::datamodel::{TransformationSpecification, normalise_spec_json};

    fn spec(yaml: &str) -> TransformationSpecification {
        let mut j: serde_json::Value = serde_yaml_ng::from_str(yaml).unwrap();
        normalise_spec_json(&mut j);
        serde_json::from_value(j).unwrap()
    }

    fn row(yaml: &str) -> Value {
        serde_yaml_ng::from_str(yaml).unwrap()
    }

    fn map_row(spec_yaml: &str, row_yaml: &str) -> Result<Value> {
        ObjectTransformer::new(spec(spec_yaml), None, None).map_object(&row(row_yaml), Some("P"))
    }

    // upstream test_pivot_operation.py::TestMeltBasic::test_melt_with_id_slots —
    // a slot-level `pivot_operation` melts into the slot (corpus 0911eed31f69).
    #[test]
    fn slot_level_pivot_operation_melts() {
        let out = map_row(
            "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      measurements:\n        pivot_operation:\n          direction: MELT\n          source_slots: [height, weight]\n          id_slots: [id]\n",
            "{id: p1, height: 1.8, weight: 75.0}",
        )
        .unwrap();
        assert_eq!(
            out,
            row(
                "measurements:\n- {id: p1, variable: height, value: 1.8}\n- {id: p1, variable: weight, value: 75.0}\n"
            )
        );
    }

    // `value_mappings` look the source value up by Python's `str(v)`: a boolean
    // is `'True'` (corpus 211702f6cc07).
    #[test]
    fn value_mappings_key_a_boolean_as_python_str() {
        let spec_yaml = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      out:\n        populated_from: flag\n        value_mappings:\n          'True': X\n          'false': wrong\n";
        assert_eq!(map_row(spec_yaml, "flag: true").unwrap(), row("out: X"));
        // `'false'` is not `str(False)`: a miss maps to null.
        assert_eq!(map_row(spec_yaml, "flag: false").unwrap(), row("out: null"));
    }

    // `_nullify_missing_values` runs before the mappings.
    #[test]
    fn missing_values_precede_value_mappings() {
        let spec_yaml = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      out:\n        populated_from: code\n        missing_values: -9\n        value_mappings:\n          '-9': mapped\n";
        assert_eq!(map_row(spec_yaml, "code: -9").unwrap(), row("out: null"));
    }

    // An offset is applied before the range coercion (which, given a source
    // schema, turns `30 + 1.0 * 5` into the integer 35 for an integer range:
    // corpus 3361f875206e, 97f1e012a222). Without a source slot the float stays.
    #[test]
    fn offset_arithmetic_is_python_float_arithmetic() {
        let spec_yaml = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      age:\n        populated_from: age_at_start\n        offset: {offset_value: 1.0, offset_field: days}\n";
        assert_eq!(
            map_row(spec_yaml, "{age_at_start: 30, days: 5}").unwrap(),
            Value::Map(IndexMap::from([("age".to_owned(), Value::Float(35.0))]))
        );
    }

    // `float * str` and `str + float` are `TypeError`s upstream.
    #[test]
    fn offset_with_a_non_numeric_operand_raises() {
        let spec_yaml = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      age:\n        populated_from: base\n        offset: {offset_value: 2.0, offset_field: days}\n";
        let err = map_row(spec_yaml, "{base: '30', days: 5}").unwrap_err();
        assert!(err.to_string().contains("TypeError"), "{err}");
        let err = map_row(spec_yaml, "{base: 30, days: '5'}").unwrap_err();
        assert!(err.to_string().contains("TypeError"), "{err}");
        // An absent offset field leaves the value alone.
        assert_eq!(map_row(spec_yaml, "{base: 30}").unwrap(), row("age: 30"));
    }

    // `_coerce_cardinality`: a scalar bound for a multivalued target is split on
    // the `stringification` delimiter (corpus 466ed494c991, be6418ab2fcb).
    #[test]
    fn scalar_into_multivalued_splits_on_the_delimiter() {
        let spec_yaml = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      out:\n        expr: s\n        cast_collection_as: MultiValued\n        stringification: {delimiter: '|'}\n";
        let slot_row = |s: &str| format!("s: '{s}'");
        assert_eq!(
            map_row(spec_yaml, &slot_row("a|b")).unwrap(),
            row("out: [a, b]")
        );
        assert_eq!(
            map_row(spec_yaml, &slot_row("p1")).unwrap(),
            row("out: [p1]")
        );
        assert_eq!(map_row(spec_yaml, &slot_row("")).unwrap(), row("out: []"));
    }

    // `delimiter.join(vs)` and `v.split(delimiter)` raise for non-strings.
    #[test]
    fn delimiter_join_and_split_need_strings() {
        let join = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      out:\n        expr: s\n        cast_collection_as: SingleValued\n        stringification: {delimiter: '|'}\n";
        assert_eq!(map_row(join, "s: [a, b]").unwrap(), row("out: a|b"));
        let err = map_row(join, "s: [1, 2]").unwrap_err();
        assert!(
            err.to_string().contains("expected str instance, int found"),
            "{err}"
        );
        let split = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      out:\n        expr: s\n        stringification: {delimiter: '|', reversed: true}\n";
        let err = map_row(split, "s: 5").unwrap_err();
        assert!(err.to_string().contains("no attribute 'split'"), "{err}");
    }

    // The JSON syntax round trips; reversed, the scalar is parsed (`json.loads`).
    #[test]
    fn reversed_json_stringification_parses_the_value() {
        let spec_yaml = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      out:\n        expr: s\n        stringification: {syntax: JSON, reversed: true}\n";
        assert_eq!(
            map_row(spec_yaml, "s: '[\"a\", 2]'").unwrap(),
            row("out: [a, 2]")
        );
    }

    // `unit_conversion` without any unit is a `ValueError` (the magnitude is not
    // passed through), and equal units are never looked up.
    #[test]
    fn unit_conversion_needs_a_source_unit() {
        let spec_yaml = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      out:\n        populated_from: h\n        unit_conversion: {target_unit: m}\n";
        let err = map_row(spec_yaml, "h: 150").unwrap_err();
        assert!(err.to_string().contains("No source unit provided"), "{err}");
        let same = "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      out:\n        populated_from: h\n        unit_conversion: {source_unit: furlong, target_unit: furlong}\n";
        assert_eq!(map_row(same, "h: 3").unwrap(), row("out: 3.0"));
    }

    // Slot-level `class_derivations` is a list: two entries with one name each
    // produce an object (corpus a8873701d43d).
    #[test]
    fn nested_class_derivations_keep_repeated_names() {
        let s = spec(
            "class_derivations:\n  A:\n    populated_from: P\n    slot_derivations:\n      conds:\n        class_derivations:\n        - {name: Cond, populated_from: P, slot_derivations: {c: {expr: \"'one'\"}}}\n        - {name: Cond, populated_from: P, slot_derivations: {c: {expr: \"'two'\"}}}\n",
        );
        let nested = s.class_derivations.as_ref().unwrap()[0]
            .slot_derivations
            .as_ref()
            .unwrap()["conds"]
            .class_derivations
            .as_ref()
            .unwrap();
        assert_eq!(nested.len(), 2);
    }
}
