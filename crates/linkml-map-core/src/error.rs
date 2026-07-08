use thiserror::Error;

/// Core transformation errors.
#[derive(Debug, Error)]
pub enum Error {
    #[error("stub error")]
    Stub,

    // ── Engine errors ─────────────────────────────────────────────────────────
    /// The source type could not be resolved (not provided and not inferable).
    #[error("cannot resolve source type: {0}")]
    SourceTypeUnresolvable(String),

    /// No matching ClassDerivation was found for the given source type.
    #[error("no class derivation for source type '{source_type}' (found {count} candidates)")]
    NoClassDerivation { source_type: String, count: usize },

    /// No matching EnumDerivation was found.
    #[error("no enum derivation for source enum '{0}'")]
    NoEnumDerivation(String),

    /// Expression evaluation failed on a slot.
    #[error("expr eval failed for slot '{slot}' in class '{class}': {cause}")]
    ExprEval {
        class: String,
        slot: String,
        cause: String,
    },

    /// Schema introspection error during transformation.
    #[error("schema error during transform of '{class}.{slot}': {cause}")]
    SchemaLookup {
        class: String,
        slot: String,
        cause: String,
    },

    /// Type coercion failed.
    #[error("coercion failed for value '{value}' to type '{target}': {cause}")]
    Coercion {
        value: String,
        target: String,
        cause: String,
    },

    /// Cardinality constraint violated (e.g. multiple values when single expected).
    #[error("cardinality error for slot '{slot}': {msg}")]
    Cardinality { slot: String, msg: String },

    /// Unit conversion failed: an unknown unit or dimensionally incompatible
    /// conversion. Mirrors Python `UndefinedUnitError` / `DimensionalityError`.
    #[error("unit conversion error for slot '{slot}': {msg}")]
    UnitConversion { slot: String, msg: String },

    /// A transformation specification could not be inverted (e.g. a non-trivial
    /// expression with no reverse). Mirrors Python `NonInvertibleSpecificationError`.
    #[error("non-invertible specification: {0}")]
    NonInvertible(String),

    /// Implicit-join synthesis (spec normalisation) failed loud: a cross-table
    /// reference that cannot be keyed, resolved, or hosted. Mirrors the
    /// `ValueError`s raised by Python
    /// `Transformer._synthesize_implicit_joins` and its helpers
    /// (`_synthesize_join`, `_reject_unknown_qualified_roots`,
    /// `_reject_unhostable_cross_table_refs`).
    #[error("{0}")]
    JoinSynthesis(String),

    /// Wrap any other error with context.
    #[error("transformation error in '{class}.{slot}': {cause}")]
    SlotTransform {
        class: String,
        slot: String,
        cause: String,
    },

    /// A `populated_from` dot-path that traverses inlined nested data hit an
    /// unsupported or malformed segment: a list segment (multivalued inline
    /// fan-out, not yet supported — upstream issue #265) or a non-dict value
    /// mid-path. Mirrors Python `TransformationError` raised from
    /// `ObjectTransformer._resolve_inline_path`, carrying the same
    /// `slot_derivation_name` / `slot_populated_from` context.
    #[error("{message}; slot_derivation={slot_derivation_name} (from {slot_populated_from})")]
    InlinePath {
        message: String,
        slot_derivation_name: String,
        slot_populated_from: String,
    },
}

pub type Result<T> = std::result::Result<T, Error>;
