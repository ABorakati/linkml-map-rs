//! LinkML transformation specification datamodel.
//!
//! This module provides Rust serde structs for deserializing LinkML-map
//! transform specifications from YAML. The structs mirror the Python dataclasses
//! in linkml_map.datamodel.transformer_model.

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

mod reference_strings;

fn deserialize_stringish_option<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    Ok(value.map(|v| match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    }))
}

/// Deserialize a field that accepts either a single scalar or a list of
/// scalars into `Option<Vec<String>>`. Used for v0.6.0 list-form
/// `populated_from` on permissible-value derivations (#250): `"A"` and
/// `["A", "C"]` both parse to a vector. Scalars are coerced stringish so
/// numeric PV codes (e.g. `1`) survive.
fn deserialize_string_or_seq_option<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let stringify = |v: serde_json::Value| match v {
        serde_json::Value::String(s) => s,
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        other => other.to_string(),
    };
    Ok(match value {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::Array(arr)) => Some(arr.into_iter().map(stringify).collect()),
        // PARITY: ReferenceValidator `ensure_list` turns a mapping into the list
        // of its values.
        Some(serde_json::Value::Object(map)) => Some(map.into_values().map(stringify).collect()),
        Some(scalar) => Some(vec![stringify(scalar)]),
    })
}

/// Deserialize a list field whose items are `Any`, accepting a bare value as
/// shorthand for a one-element list.
///
/// PARITY: `SlotDerivation.coerce_missing_values` (a `mode='before'` validator,
/// "accept a single scalar as shorthand for a one-element list") — and
/// `ReferenceValidator`, which wraps a scalar bound for any multivalued field.
fn deserialize_any_seq_option<'de, D>(
    deserializer: D,
) -> Result<Option<Vec<serde_json::Value>>, D::Error>
where
    D: Deserializer<'de>,
{
    Ok(
        match Option::<serde_json::Value>::deserialize(deserializer)? {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Array(items)) => Some(items),
            Some(scalar) => Some(vec![scalar]),
        },
    )
}

/// Serialize `Option<Vec<String>>` as a bare scalar when it holds exactly one
/// element, otherwise as a list — mirroring the input shape of list-form
/// `populated_from`.
fn serialize_seq_or_scalar_option<S>(
    value: &Option<Vec<String>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(v) if v.len() == 1 => serializer.serialize_str(&v[0]),
        Some(v) => v.serialize(serializer),
        None => serializer.serialize_none(),
    }
}

/// Deserialize `source_schema` / `target_schema`, accepting either a bare
/// string (a schema name/path) or a full mapping. A string `s` becomes
/// `SchemaReference { name: Some(s), .. }`. v0.6.0 (#215).
fn deserialize_schema_reference_option<'de, D>(
    deserializer: D,
) -> Result<Option<SchemaReference>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(s)) => Ok(Some(SchemaReference {
            name: Some(s),
            ..Default::default()
        })),
        Some(other) => serde_json::from_value(other)
            .map(Some)
            .map_err(serde::de::Error::custom),
    }
}

/// A reference to a schema, by name and/or location. v0.6.0 replaces the plain
/// string `source_schema`/`target_schema` (#215). Mirrors Python
/// `SchemaReference` (`name`, `version`, `schema_uri`, `source_file`;
/// pydantic `extra="forbid"`).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SchemaReference {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_stringish_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema_uri: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
}

impl SchemaReference {
    /// The usable path/identifier for loading: `source_file`, falling back to
    /// `name` (upstream `validator._resolve_schema_path`).
    pub fn path(&self) -> Option<&str> {
        self.source_file.as_deref().or(self.name.as_deref())
    }
}

/// Collection type for slot derivations.
///
/// Mirrors Python `CollectionType` enum.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "PascalCase")]
pub enum CollectionType {
    #[default]
    SingleValued,
    MultiValued,
    MultiValuedList,
    MultiValuedDict,
}

/// Serialization syntax type for stringification.
///
/// Mirrors Python `SerializationSyntaxType` enum.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum SerializationSyntaxType {
    #[default]
    Json,
    Yaml,
    Turtle,
}

/// Aggregation operation types.
///
/// Mirrors Python `AggregationType` enum.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AggregationType {
    #[default]
    Sum,
    Average,
    Count,
    Min,
    Max,
    StdDev,
    Variance,
    Median,
    Mode,
    Custom,
    Set,
    List,
    Array,
}

/// Strategy for handling invalid values.
///
/// Mirrors Python `InvalidValueHandlingStrategy` enum.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum InvalidValueHandlingStrategy {
    #[default]
    Ignore,
    TreatAsZero,
    ErrorOut,
}

/// Pivot direction type (melt vs unmelt).
///
/// Mirrors Python `PivotDirectionType` enum.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(rename_all = "UPPERCASE")]
pub enum PivotDirectionType {
    #[default]
    Melt,
    Unmelt,
}

/// Base class for specification components.
///
/// Mirrors Python `SpecificationComponent`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SpecificationComponent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub implements: Option<Vec<String>>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub comments: Option<Vec<String>>,
}

/// A key-value pair used in mappings.
///
/// Mirrors Python `KeyVal`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KeyVal {
    pub key: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

/// Configuration for unit conversion.
///
/// Mirrors Python `UnitConversionConfiguration`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UnitConversionConfiguration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_unit: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_unit_scheme: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_unit: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_unit_scheme: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_unit_slot: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_magnitude_slot: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_unit_slot: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_magnitude_slot: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub none_if_non_numeric: Option<bool>,

    /// Molecular weight in g/mol, enabling molar↔mass bridging for an
    /// analyte-specific conversion (e.g. glucose `mg/dL` ↔ `mmol/L`). Required
    /// because the unit token alone does not identify the substance — the same
    /// reason `pint` refuses without a substance context.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub molecular_weight: Option<f64>,

    /// Ion valence (charge number), enabling equivalents↔molar bridging
    /// (e.g. `mEq/L` ↔ `mmol/L`): `mmol = mEq / valence`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub valence: Option<f64>,
}

/// Configuration for offset calculations.
///
/// Used for longitudinal data transformations where measurements are recorded
/// relative to a baseline. Calculation: result = baseline ± (offset_value * offset_field_value).
///
/// Mirrors Python `Offset`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Offset {
    pub offset_value: f64,
    pub offset_field: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_reverse: Option<bool>,
}

/// Configuration for stringification of values.
///
/// Mirrors Python `StringificationConfiguration`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StringificationConfiguration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delimiter: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reversed: Option<bool>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub over_slots: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub syntax: Option<SerializationSyntaxType>,
}

/// Inverse (back-reference) specification for relational mappings.
///
/// Mirrors Python `Inverse`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Inverse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot_name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_name: Option<String>,
}

/// Abstract transformation operation.
///
/// Mirrors Python `TransformationOperation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct TransformationOperation;

/// Aggregation operation configuration.
///
/// Mirrors Python `AggregationOperation`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AggregationOperation {
    pub operator: AggregationType,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub null_handling: Option<InvalidValueHandlingStrategy>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub invalid_value_handling: Option<InvalidValueHandlingStrategy>,
}

/// Grouping operation configuration.
///
/// Mirrors Python `GroupingOperation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct GroupingOperation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub null_handling: Option<InvalidValueHandlingStrategy>,
}

/// Pivot (melt/unmelt) operation configuration.
///
/// Mirrors Python `PivotOperation`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PivotOperation {
    pub direction: PivotDirectionType,

    #[serde(default = "default_variable_slot")]
    pub variable_slot: String,

    #[serde(default = "default_value_slot")]
    pub value_slot: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmelt_to_class: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub unmelt_to_slots: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_slot: Option<String>,

    #[serde(default = "default_slot_name_template")]
    pub slot_name_template: String,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_slots: Option<Vec<String>>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub id_slots: Option<Vec<String>>,
}

fn default_variable_slot() -> String {
    "variable".to_string()
}

fn default_value_slot() -> String {
    "value".to_string()
}

fn default_slot_name_template() -> String {
    "{variable}".to_string()
}

/// Alias specification for joined classes.
///
/// Mirrors Python `AliasedClass`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AliasedClass {
    pub alias: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_named: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_key: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub lookup_key: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub join_on: Option<String>,
}

/// Copy directive for schema transformation.
///
/// Instructs a schema mapper on how to copy elements. Not used for data transformation.
///
/// Mirrors Python `CopyDirective`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CopyDirective {
    pub element_name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_all: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_all: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub include: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub add: Option<serde_json::Value>,
}

/// Base class for agents (creator, author, etc).
///
/// Mirrors Python `Agent`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "PascalCase", deny_unknown_fields)]
pub enum Agent {
    #[serde(rename = "Agent")]
    Agent {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Person {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        orcid: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        affiliation: Option<String>,
    },
    Organization {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        ror_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Software {
        id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(
            default,
            deserialize_with = "deserialize_stringish_option",
            skip_serializing_if = "Option::is_none"
        )]
        version: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        repository_url: Option<String>,
    },
}

/// A specification of how to derive a target slot from source slot(s).
///
/// Mirrors Python `SlotDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SlotDerivation {
    pub name: String,

    /// Deprecated upstream (use `populated_from`); accepted so legacy specs load.
    #[serde(
        default,
        deserialize_with = "reference_strings::deserialize_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub sources: Option<Vec<String>>,

    /// Deprecated upstream and ignored by the runtime; accepted so legacy specs load.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub populated_from: Option<String>,

    /// Nested target objects derived from source data (v0.6.0: replaces the
    /// removed `object_derivations` — one nesting level flatter). Mirrors
    /// Python `SlotDerivation.class_derivations`, a *list*: entries may share a
    /// `name` and each one produces its own object.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_derivations: Option<Vec<ClassDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,

    /// Sentinel source values that map to null (e.g. `-9`, `999`, `"NA"`).
    /// When the derived value equals one of these, the slot is set to null.
    /// v0.6.0 (#269). Mirrors Python `SlotDerivation.missing_values`.
    #[serde(
        default,
        deserialize_with = "deserialize_any_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub missing_values: Option<Vec<serde_json::Value>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_conversion: Option<UnitConversionConfiguration>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub inverse_of: Option<Inverse>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_designator: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_definition: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub cast_collection_as: Option<CollectionType>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub dictionary_key: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub stringification: Option<StringificationConfiguration>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregation_operation: Option<AggregationOperation>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pivot_operation: Option<PivotOperation>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<Offset>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_directives: Option<IndexMap<String, CopyDirective>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_a: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub mixins: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_to_value_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_to_expression_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_source: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub implements: Option<Vec<String>>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub comments: Option<Vec<String>>,
}

/// A specification of how to derive a target enum PV from a source enum PV.
///
/// Mirrors Python `PermissibleValueDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PermissibleValueDerivation {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,

    /// Source permissible value(s) that map to this target PV. v0.6.0 accepts
    /// either a single string or a list (list-form `populated_from`, #250),
    /// replacing the removed `sources` list. Mirrors Python
    /// `PermissibleValueDerivation.populated_from`.
    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        serialize_with = "serialize_seq_or_scalar_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub populated_from: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_directives: Option<IndexMap<String, CopyDirective>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_a: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub mixins: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_to_value_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_to_expression_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_source: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub implements: Option<Vec<String>>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub comments: Option<Vec<String>>,
}

/// A specification of how to derive a target enum from a source enum.
///
/// Mirrors Python `EnumDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EnumDerivation {
    pub name: String,

    /// Deprecated upstream (use `populated_from`); accepted so legacy specs load.
    #[serde(
        default,
        deserialize_with = "reference_strings::deserialize_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub sources: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub populated_from: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub hide: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissible_value_derivations: Option<IndexMap<String, PermissibleValueDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_directives: Option<IndexMap<String, CopyDirective>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_a: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub mixins: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_to_value_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_to_expression_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_source: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub implements: Option<Vec<String>>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub comments: Option<Vec<String>>,
}

// `ObjectDerivation` removed in v0.6.0 — nested object derivations now live
// directly on `SlotDerivation.class_derivations` (one nesting level flatter).

/// Settings applied to every class derivation that targets one class, unless
/// the derivation sets its own. Mirrors Python `ClassDefault` (upstream #342).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClassDefault {
    /// Name of the class in the target schema.
    #[serde(default)]
    pub name: String,

    /// Whether derivations of this class mint content-hash identifiers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_ids: Option<bool>,
}

/// Deserialize `class_defaults`, accepting a mapping (`{Class: {mint_ids: b}}`,
/// compact `{Class: b}`, or `{Class: null}` meaning unset) or a list of
/// `{name, mint_ids}` entries. Mirrors the shapes upstream's
/// `_class_default_mint_ids` and `ReferenceValidator` normalisation accept.
fn deserialize_class_defaults_option<'de, D>(
    deserializer: D,
) -> Result<Option<IndexMap<String, ClassDefault>>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let value = Option::<serde_json::Value>::deserialize(deserializer)?;
    let mut out: IndexMap<String, ClassDefault> = IndexMap::new();
    match value {
        None | Some(serde_json::Value::Null) => return Ok(None),
        Some(serde_json::Value::Object(map)) => {
            for (name, body) in map {
                let mint_ids = match body {
                    serde_json::Value::Null => None,
                    serde_json::Value::Bool(b) => Some(b),
                    serde_json::Value::Object(o) => {
                        match o
                            .get("mint_ids")
                            .cloned()
                            .unwrap_or(serde_json::Value::Null)
                        {
                            serde_json::Value::Null => None,
                            serde_json::Value::Bool(b) => Some(b),
                            other => {
                                return Err(D::Error::custom(format!(
                                    "class_defaults[{name}].mint_ids must be a boolean, got {other}"
                                )));
                            }
                        }
                    }
                    other => {
                        return Err(D::Error::custom(format!(
                            "class_defaults[{name}] must be a mapping or a boolean, got {other}"
                        )));
                    }
                };
                out.insert(name.clone(), ClassDefault { name, mint_ids });
            }
        }
        Some(serde_json::Value::Array(items)) => {
            for item in items {
                let cd: ClassDefault = serde_json::from_value(item).map_err(D::Error::custom)?;
                out.insert(cd.name.clone(), cd);
            }
        }
        Some(other) => {
            return Err(D::Error::custom(format!(
                "class_defaults must be a mapping or a list, got {other}"
            )));
        }
    }
    Ok(Some(out))
}

/// A specification of how to derive a target class from source class(es).
///
/// Mirrors Python `ClassDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ClassDerivation {
    pub name: String,

    /// Deprecated upstream (use `populated_from`); accepted so legacy specs load.
    #[serde(
        default,
        deserialize_with = "reference_strings::deserialize_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub sources: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub populated_from: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub joins: Option<IndexMap<String, AliasedClass>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot_derivations: Option<IndexMap<String, SlotDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_definition: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pivot_operation: Option<PivotOperation>,

    /// Overrides the specification's `mint_ids`, and the target class's entry
    /// in `class_defaults`, for this class derivation (upstream #342).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_ids: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_directives: Option<IndexMap<String, CopyDirective>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_a: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub mixins: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_to_value_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression_to_expression_mappings: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_source: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub implements: Option<Vec<String>>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub comments: Option<Vec<String>>,
}

/// The root transformation specification.
///
/// A collection of mappings between source and target classes, along with
/// enum derivations and global configuration.
///
/// Mirrors Python `TransformationSpecification`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransformationSpecification {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub publication_date: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_stringish_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefixes: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_directives: Option<IndexMap<String, CopyDirective>>,

    #[serde(
        default,
        deserialize_with = "deserialize_schema_reference_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_schema: Option<SchemaReference>,

    #[serde(
        default,
        deserialize_with = "deserialize_schema_reference_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub target_schema: Option<SchemaReference>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_schema_patches: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator: Option<Vec<Agent>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<Vec<Agent>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reviewer: Option<Vec<Agent>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub mapping_method: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_url: Option<String>,

    /// Whether to synthesize an identifier from a hash of the record's content
    /// when a target class has an identifier slot that the class derivation
    /// does not derive (upstream #342). Off by default; an identifier that is
    /// neither derived nor minted is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_ids: Option<bool>,

    /// Per-target-class settings (currently `mint_ids`), keyed by class name.
    /// Accepts the mapping form (`{Class: {mint_ids: true}}`), the compact
    /// `{Class: true}` form and the explicit list form.
    #[serde(
        default,
        deserialize_with = "deserialize_class_defaults_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub class_defaults: Option<IndexMap<String, ClassDefault>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_derivations: Option<Vec<ClassDerivation>>,

    /// Top-level slot derivations that are not nested under any
    /// `class_derivation`. These have nowhere to host a `joins:` block, so a
    /// cross-table reference in one fails loud during join synthesis. The
    /// engine does not otherwise process top-level slot derivations. Mirrors
    /// Python `TransformationSpecification.slot_derivations`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot_derivations: Option<IndexMap<String, SlotDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_derivations: Option<IndexMap<String, EnumDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub implements: Option<Vec<String>>,

    #[serde(
        default,
        deserialize_with = "deserialize_string_or_seq_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub comments: Option<Vec<String>>,
}

/// `spec_normalizer._expand_compact_keys`: turn `{Name: {body}}` / `{Name: null}` list
/// items into `{name: Name, ...body}` in place (items already carrying `name` as their
/// sole key, or not shaped that way, are left alone).
fn expand_compact_keys(items: &mut [serde_json::Value]) {
    use serde_json::Value;
    for item in items {
        let Value::Object(map) = item else { continue };
        if map.len() != 1 {
            continue;
        }
        let Some((key, body)) = map.iter().next() else {
            continue;
        };
        if key == "name" || !matches!(body, Value::Object(_) | Value::Null) {
            continue;
        }
        let mut expanded = match body {
            Value::Object(o) => o.clone(),
            _ => serde_json::Map::new(),
        };
        expanded
            .entry("name")
            .or_insert_with(|| Value::String(key.clone()));
        *item = Value::Object(expanded);
    }
}

/// Accept the list form of a name-keyed collection, as upstream's `ReferenceValidator`
/// does: a list of `{<key_field>: ..., ...}` items becomes the mapping keyed by that
/// field. Items without a string key field are dropped, a repeated key keeps the last.
fn list_to_map(node: &mut serde_json::Value, key_field: &str, compact_keys: bool) {
    use serde_json::Value;
    let Value::Array(items) = node else { return };
    if compact_keys {
        expand_compact_keys(items);
    }
    let keyed: serde_json::Map<String, Value> = items
        .drain(..)
        .filter_map(|item| {
            let key = item.get(key_field)?.as_str()?.to_owned();
            Some((key, item))
        })
        .collect();
    *node = Value::Object(keyed);
}

/// Normalise the list forms upstream accepts wherever a derivation section may be a
/// mapping or a list (`spec_normalizer._pre_shape_expand_compact_keys` plus
/// `ReferenceValidator`'s dict/list canonicalisation): compact-key items in every
/// `class_derivations` list, and `slot_derivations`, `enum_derivations`,
/// `permissible_value_derivations`, `joins` and `copy_directives` given as lists.
fn expand_list_forms(root: &mut serde_json::Value) {
    use serde_json::Value;

    fn class_derivation(cd: &mut Value) {
        let Value::Object(map) = cd else { return };
        if let Some(sds) = map.get_mut("slot_derivations") {
            list_to_map(sds, "name", true);
            slot_derivations(sds);
        }
        if let Some(joins) = map.get_mut("joins") {
            list_to_map(joins, "alias", false);
        }
        if let Some(dirs) = map.get_mut("copy_directives") {
            list_to_map(dirs, "element_name", false);
        }
    }

    fn slot_derivations(sds: &mut Value) {
        let Value::Object(map) = sds else { return };
        for sd in map.values_mut() {
            let Value::Object(sd) = sd else { continue };
            if let Some(cds) = sd.get_mut("class_derivations") {
                match cds {
                    Value::Array(items) => {
                        expand_compact_keys(items);
                        items.iter_mut().for_each(class_derivation);
                    }
                    Value::Object(by_name) => by_name.values_mut().for_each(class_derivation),
                    _ => {}
                }
            }
            // The deprecated nested form, flattened later by `flatten_object_derivations`.
            if let Some(Value::Array(ods)) = sd.get_mut("object_derivations") {
                for od in ods {
                    let Value::Object(od) = od else { continue };
                    match od.get_mut("class_derivations") {
                        Some(Value::Array(items)) => {
                            expand_compact_keys(items);
                            items.iter_mut().for_each(class_derivation);
                        }
                        Some(Value::Object(by_name)) => {
                            by_name.values_mut().for_each(class_derivation);
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    let Value::Object(obj) = root else { return };
    match obj.get_mut("class_derivations") {
        Some(Value::Array(items)) => {
            expand_compact_keys(items);
            items.iter_mut().for_each(class_derivation);
        }
        Some(Value::Object(by_name)) => by_name.values_mut().for_each(class_derivation),
        _ => {}
    }
    if let Some(eds) = obj.get_mut("enum_derivations") {
        list_to_map(eds, "name", true);
        if let Value::Object(map) = eds {
            for ed in map.values_mut() {
                if let Some(pvds) = ed.get_mut("permissible_value_derivations") {
                    list_to_map(pvds, "name", true);
                }
            }
        }
    }
    if let Some(sds) = obj.get_mut("slot_derivations") {
        list_to_map(sds, "name", true);
        slot_derivations(sds);
    }
    if let Some(dirs) = obj.get_mut("copy_directives") {
        list_to_map(dirs, "element_name", false);
    }
}

/// Normalise a LinkML-map spec JSON value (derived from YAML) in-place.
///
/// Upstream `linkml-map` specs utilize YAML mapping shortcuts that do not map
/// directly to the canonical serialization shapes expected by `serde` for our
/// strongly-typed model. This function transforms those mapping shortcuts in-place:
///
/// - `class_derivations`: mapping (`ClassName` -> body) to `Vec<ClassDerivation>` with injected `name`.
/// - `slot_derivations` within class derivations: inject `name` from the mapping key.
/// - `joins` within class derivations: inject `alias` from the mapping key.
/// - Shorthand null-valued slots (`id:`) inside `slot_derivations` -> `{"name": "id"}`.
/// - `enum_derivations`: inject `name` from mapping key.
/// - `permissible_value_derivations` under enum derivations: inject `name` from mapping key.
/// - `prefixes`: mapping of string values -> mapping of `KeyVal` maps (`{key: string, value: string}`).
/// - `creator` / `author` / `reviewer`: inject default agent `type` -> `"Agent"` if missing.
pub fn normalise_spec_json(root: &mut serde_json::Value) {
    expand_list_forms(root);
    if let Some(obj) = root.as_object_mut() {
        // ── class_derivations: top-level mapping -> Vec ────────────────────────
        if let Some(cd) = obj.get_mut("class_derivations")
            && cd.is_object()
        {
            let mapping = std::mem::replace(cd, serde_json::Value::Null);
            let mut list = Vec::new();
            if let serde_json::Value::Object(m) = mapping {
                for (class_name, mut val) in m {
                    if val.is_null() {
                        val = serde_json::json!({});
                    }
                    if let Some(o) = val.as_object_mut() {
                        o.insert("name".into(), serde_json::Value::String(class_name.clone()));
                        normalise_class_derivation_body(o);
                    }
                    list.push(val);
                }
            }
            *cd = serde_json::Value::Array(list);
        }
        // list form: names are explicit, but slot derivations may still be a mapping
        if let Some(items) = obj
            .get_mut("class_derivations")
            .and_then(|cd| cd.as_array_mut())
        {
            for item in items {
                if let Some(o) = item.as_object_mut() {
                    normalise_class_derivation_body(o);
                }
            }
        }

        // ── top-level slot_derivations: inject `name` from the mapping key ────
        if let Some(sd) = obj.get_mut("slot_derivations")
            && let Some(sdm) = sd.as_object_mut()
        {
            normalise_slot_derivations(sdm);
        }

        // ── enum_derivations: inject `name` ───────────────────────────────────
        if let Some(ed) = obj.get_mut("enum_derivations")
            && let Some(edm) = ed.as_object_mut()
        {
            for (enum_name, enum_val) in edm.iter_mut() {
                if enum_val.is_null() {
                    *enum_val = serde_json::json!({});
                }
                if let Some(eo) = enum_val.as_object_mut() {
                    if !eo.contains_key("name") {
                        eo.insert("name".into(), serde_json::Value::String(enum_name.clone()));
                    }
                    if let Some(pvds) = eo.get_mut("permissible_value_derivations")
                        && let Some(pvm) = pvds.as_object_mut()
                    {
                        for (pv_name, pv_val) in pvm.iter_mut() {
                            if pv_val.is_null() {
                                *pv_val = serde_json::json!({});
                            }
                            if let Some(po) = pv_val.as_object_mut() {
                                if !po.contains_key("name") {
                                    po.insert(
                                        "name".into(),
                                        serde_json::Value::String(pv_name.clone()),
                                    );
                                }
                                migrate_pv_sources(po);
                            }
                        }
                    }
                }
            }
        }

        // source_schema / target_schema accept either a bare string or a full
        // mapping natively (see `deserialize_schema_reference_option`), so no
        // normalisation is needed here.

        // ── prefixes: string values -> KeyVal maps ─────────────────────────────
        if let Some(pfx) = obj.get_mut("prefixes")
            && let Some(m) = pfx.as_object_mut()
        {
            for (_, v) in m.iter_mut() {
                if v.is_string() {
                    let s = v.as_str().unwrap().to_string();
                    *v = serde_json::json!({ "key": s, "value": s });
                }
            }
        }

        // ── creator / author / reviewer: inject default agent `type` ──────────
        for key in &["creator", "author", "reviewer"] {
            if let Some(agents) = obj.get_mut(*key)
                && let Some(arr) = agents.as_array_mut()
            {
                for agent in arr.iter_mut() {
                    if let Some(o) = agent.as_object_mut()
                        && !o.contains_key("type")
                    {
                        o.insert("type".into(), serde_json::Value::String("Agent".into()));
                    }
                }
            }
        }
    }

    inject_copy_directive_names(root);
    collapse_any_lists(root);
}

/// Mimic upstream's shape normalisation of `range: Any` slots.
///
/// PARITY: `spec_normalizer._shape_normalize` runs linkml's `ReferenceValidator`, which
/// treats an `Any`-ranged field as single-valued: a list becomes its first element (an
/// empty list becomes `null`). In the transformer model that hits `SlotDerivation.value`
/// (so `value: [a, b]` is the constant `a`), `CopyDirective`'s `include` / `exclude` /
/// `add` (so `exclude: [a, b]` only ever excludes `a`) and the derivations' `overrides` /
/// `target_definition`. Dicts and nested content are untouched, and so are the `value` of
/// a `KeyVal` (`value_mappings`, `expression_mappings`), which `ReferenceValidator` leaves
/// alone. A list of lists makes upstream raise `NotImplementedError("List of Lists")`
/// at load; here it collapses to its first list.
/// Suggested upstream issue: declare these slots `multivalued` (or stop normalising `Any`);
/// a constant `value: [a, b]` is silently truncated to `a`.
fn collapse_any_lists(node: &mut serde_json::Value) {
    fn first(v: &mut serde_json::Value) {
        if let serde_json::Value::Array(items) = v {
            *v = if items.is_empty() {
                serde_json::Value::Null
            } else {
                items.swap_remove(0)
            };
        }
    }
    match node {
        serde_json::Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                match key.as_str() {
                    "copy_directives" => {
                        if let Some(directives) = value.as_object_mut() {
                            for directive in directives.values_mut() {
                                if let Some(d) = directive.as_object_mut() {
                                    for field in ["include", "exclude", "add"] {
                                        if let Some(v) = d.get_mut(field) {
                                            first(v);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // The constant of each slot derivation (`SlotDerivation.value`).
                    "slot_derivations" => {
                        if let Some(slots) = value.as_object_mut() {
                            for slot in slots.values_mut() {
                                if let Some(v) =
                                    slot.as_object_mut().and_then(|s| s.get_mut("value"))
                                {
                                    first(v);
                                }
                            }
                        }
                    }
                    // Free-form content: collapse the list itself, never look inside.
                    "overrides" | "target_definition" => {
                        first(value);
                        continue;
                    }
                    _ => {}
                }
                collapse_any_lists(value);
            }
        }
        serde_json::Value::Array(items) => items.iter_mut().for_each(collapse_any_lists),
        _ => {}
    }
}

/// `spec_normalizer._migrate_pv_sources_to_populated_from`: move the deprecated
/// `sources` of a permissible-value derivation into `populated_from` (unless that is
/// already set), wrapping a scalar into a one-element list, and drop `sources`.
fn migrate_pv_sources(pv: &mut serde_json::Map<String, serde_json::Value>) {
    use serde_json::Value;
    let Some(sources) = pv.remove("sources") else {
        return;
    };
    if !crate::validate::py_truthy(&sources)
        || pv
            .get("populated_from")
            .is_some_and(crate::validate::py_truthy)
    {
        return;
    }
    pv.insert(
        "populated_from".into(),
        match sources {
            Value::Array(items) => Value::Array(items),
            scalar => Value::Array(vec![scalar]),
        },
    );
}

/// `spec_normalizer._flatten_ods_in_class_deriv` for one slot derivation: flatten the
/// deprecated `object_derivations` into the slot's `class_derivations` list and drop
/// the key. An empty `object_derivations` is simply dropped.
fn flatten_object_derivations(slot: &mut serde_json::Map<String, serde_json::Value>) {
    use serde_json::Value;
    let Some(Value::Array(ods)) = slot.remove("object_derivations") else {
        return;
    };
    if ods.is_empty() {
        return;
    }
    let mut flattened = Vec::new();
    for od in ods {
        let Value::Object(od) = od else { continue };
        match od.get("class_derivations") {
            Some(Value::Object(by_name)) => {
                for (name, body) in by_name {
                    let mut entry = match body {
                        Value::Object(o) => o.clone(),
                        _ => serde_json::Map::new(),
                    };
                    entry
                        .entry("name")
                        .or_insert_with(|| Value::String(name.clone()));
                    flattened.push(Value::Object(entry));
                }
            }
            Some(Value::Array(items)) => flattened.extend(items.iter().cloned()),
            _ => {}
        }
    }
    slot.insert("class_derivations".into(), Value::Array(flattened));
}

/// Mimic `ReferenceValidator`'s dict normalisation of `copy_directives`: the mapping
/// key becomes the identifier `element_name` (`copy_directives: {"*": {copy_all: true}}`).
/// Applied wherever `copy_directives` can occur; free-form content is not searched.
fn inject_copy_directive_names(node: &mut serde_json::Value) {
    use serde_json::Value;
    match node {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                match key.as_str() {
                    "copy_directives" => {
                        if let Value::Object(directives) = value {
                            for (element, directive) in directives.iter_mut() {
                                if directive.is_null() {
                                    *directive = serde_json::json!({});
                                }
                                if let Value::Object(d) = directive {
                                    d.entry("element_name")
                                        .or_insert_with(|| Value::String(element.clone()));
                                }
                            }
                        }
                    }
                    "overrides" | "target_definition" => {}
                    _ => inject_copy_directive_names(value),
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(inject_copy_directive_names),
        _ => {}
    }
}

/// Normalise the body of a single class derivation in place: walk its
/// `slot_derivations`, injecting names and recursing into any slot-level
/// `class_derivations` (v0.6.0 nested objects). The class's own `name` is
/// expected to be set by the caller (from the mapping key).
fn normalise_class_derivation_body(o: &mut serde_json::Map<String, serde_json::Value>) {
    if let Some(sd) = o.get_mut("slot_derivations")
        && let Some(sdm) = sd.as_object_mut()
    {
        normalise_slot_derivations(sdm);
    }
    // ── joins: inject `alias` from the mapping key ─────────────────────────
    // `joins` is a mapping keyed by alias name (`Option<IndexMap<String,
    // AliasedClass>>`); `AliasedClass.alias` is required, so mirror the
    // "map key becomes the name/alias field" rule used for class/slot/enum
    // derivations. Only fill `alias` when absent so an explicit `alias:` in
    // the YAML (e.g. the `class_named` divergence pattern) wins.
    if let Some(j) = o.get_mut("joins")
        && let Some(jm) = j.as_object_mut()
    {
        for (alias_name, join_val) in jm.iter_mut() {
            if join_val.is_null() {
                *join_val = serde_json::json!({});
            }
            if let Some(jo) = join_val.as_object_mut() {
                jo.entry("alias")
                    .or_insert_with(|| serde_json::Value::String(alias_name.clone()));
            }
        }
    }
}

/// Normalise a `slot_derivations` mapping in place: inject `name` from the key,
/// normalise keyval mappings, and recurse into any slot-level
/// `class_derivations` (kept as a mapping / `IndexMap`).
fn normalise_slot_derivations(sdm: &mut serde_json::Map<String, serde_json::Value>) {
    for (slot_name, slot_val) in sdm.iter_mut() {
        if slot_val.is_null() {
            *slot_val = serde_json::json!({});
        }
        if let Some(so) = slot_val.as_object_mut() {
            so.entry("name")
                .or_insert_with(|| serde_json::Value::String(slot_name.clone()));
            flatten_object_derivations(so);
            normalise_keyval_mapping(so, "value_mappings");
            normalise_keyval_mapping(so, "expression_mappings");
            // Slot-level nested class_derivations are a list (`- name: Project
            // ...`; entries may repeat a name). The mapping form
            // (`{Project: ...}`) and the compact-key list form
            // (`- Project: ...`) are first expanded to explicit-name entries.
            if let Some(cd) = so.get_mut("class_derivations") {
                expand_slot_class_derivations(cd);
                if let Some(items) = cd.as_array_mut() {
                    for item in items {
                        if let Some(co) = item.as_object_mut() {
                            normalise_class_derivation_body(co);
                        }
                    }
                }
            }
        }
    }
}

/// Bring a slot-level `class_derivations` to the list-of-`{name, ...}` shape.
///
/// A mapping becomes a list with each key as the entry's `name` (unless it
/// has one); in a list, a compact-key entry (`{Condition: {...}}`, sole key
/// not `name`, a mapping or null value) is expanded the same way. Mirrors
/// `spec_normalizer._expand_compact_keys`.
fn expand_slot_class_derivations(cd: &mut serde_json::Value) {
    let expand = |key: String, val: serde_json::Value| {
        let mut body = if val.is_null() {
            serde_json::json!({})
        } else {
            val
        };
        if let Some(o) = body.as_object_mut() {
            o.entry("name").or_insert(serde_json::Value::String(key));
        }
        body
    };
    match cd {
        serde_json::Value::Object(map) => {
            let list = std::mem::take(map)
                .into_iter()
                .map(|(key, val)| expand(key, val))
                .collect();
            *cd = serde_json::Value::Array(list);
        }
        serde_json::Value::Array(items) => {
            for item in items {
                let compact = match item.as_object() {
                    Some(o) if o.len() == 1 => o
                        .iter()
                        .next()
                        .filter(|(k, v)| *k != "name" && (v.is_object() || v.is_null()))
                        .map(|(k, _)| k.clone()),
                    _ => None,
                };
                if let Some(key) = compact
                    && let Some(val) = item.as_object_mut().and_then(|o| o.remove(&key))
                {
                    *item = expand(key, val);
                }
            }
        }
        _ => {}
    }
}

fn normalise_keyval_mapping(obj: &mut serde_json::Map<String, serde_json::Value>, field: &str) {
    let Some(mapping) = obj.get_mut(field) else {
        return;
    };
    let Some(entries) = mapping.as_object_mut() else {
        return;
    };
    for (key, value) in entries.iter_mut() {
        match value {
            serde_json::Value::Object(o) => {
                o.entry("key")
                    .or_insert_with(|| serde_json::Value::String(key.clone()));
                if !o.contains_key("value")
                    && let Some(expr) = o.remove("expr")
                {
                    o.insert("value".into(), expr);
                }
            }
            _ => {
                let scalar = std::mem::replace(value, serde_json::Value::Null);
                *value = serde_json::json!({ "key": key, "value": scalar });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_simple_transformation_spec_from_json() {
        // Simple inline JSON spec
        let json = r#"
{
  "id": "test-spec-1",
  "title": "Test Transformation",
  "source_schema": "source.yaml",
  "target_schema": "target.yaml",
  "class_derivations": [
    {
      "name": "TargetClass",
      "populated_from": "SourceClass",
      "slot_derivations": {
        "target_slot": {
          "name": "target_slot",
          "populated_from": "source_slot"
        }
      }
    }
  ]
}
        "#;

        let spec: TransformationSpecification =
            serde_json::from_str(json).expect("Failed to deserialize simple transformation spec");

        assert_eq!(spec.id, Some("test-spec-1".to_string()));
        assert_eq!(spec.title, Some("Test Transformation".to_string()));
        // Bare-string schema refs deserialize into SchemaReference { name }.
        assert_eq!(
            spec.source_schema.as_ref().and_then(|s| s.path()),
            Some("source.yaml")
        );
        assert_eq!(
            spec.target_schema.as_ref().and_then(|s| s.path()),
            Some("target.yaml")
        );
        assert!(spec.class_derivations.is_some());

        let class_derivs = spec.class_derivations.unwrap();
        assert_eq!(class_derivs.len(), 1);
        assert_eq!(class_derivs[0].name, "TargetClass");
        assert_eq!(
            class_derivs[0].populated_from,
            Some("SourceClass".to_string())
        );
    }

    #[test]
    fn test_schema_reference_string_or_object() {
        // Object form: full SchemaReference fields parse.
        let json = r#"{
          "source_schema": { "name": "src", "source_file": "schemas/src.yaml" },
          "target_schema": "tgt.yaml"
        }"#;
        let spec: TransformationSpecification = serde_json::from_str(json).unwrap();
        let src = spec.source_schema.unwrap();
        assert_eq!(src.name.as_deref(), Some("src"));
        assert_eq!(src.source_file.as_deref(), Some("schemas/src.yaml"));
        // path() prefers source_file.
        assert_eq!(src.path(), Some("schemas/src.yaml"));
        // String form falls back to name.
        let tgt = spec.target_schema.unwrap();
        assert_eq!(tgt.name.as_deref(), Some("tgt.yaml"));
        assert_eq!(tgt.path(), Some("tgt.yaml"));
    }

    #[test]
    fn test_deserialize_slot_derivation_with_unit_conversion() {
        let json = r#"
{
  "id": "test-unit-spec",
  "title": "Unit Conversion Test",
  "class_derivations": [
    {
      "name": "TargetClass",
      "slot_derivations": {
        "height_cm": {
          "name": "height_cm",
          "populated_from": "height_inches",
          "unit_conversion": {
            "source_unit": "inches",
            "target_unit": "cm"
          },
          "value_mappings": {
            "short": {
              "key": "short",
              "value": "dwarf"
            },
            "tall": {
              "key": "tall",
              "value": "giant"
            }
          }
        }
      }
    }
  ]
}
        "#;

        let spec: TransformationSpecification = serde_json::from_str(json)
            .expect("Failed to deserialize slot derivation with unit conversion");

        let class_derivs = spec.class_derivations.unwrap();
        let slot_derivs = class_derivs[0].slot_derivations.as_ref().unwrap();
        let height_slot = slot_derivs.get("height_cm").unwrap();

        assert_eq!(height_slot.name, "height_cm");
        assert!(height_slot.unit_conversion.is_some());

        let unit_conv = height_slot.unit_conversion.as_ref().unwrap();
        assert_eq!(unit_conv.source_unit, Some("inches".to_string()));
        assert_eq!(unit_conv.target_unit, Some("cm".to_string()));

        assert!(height_slot.value_mappings.is_some());
        let value_mappings = height_slot.value_mappings.as_ref().unwrap();
        assert_eq!(value_mappings.len(), 2);
        assert!(value_mappings.contains_key("short"));
        assert_eq!(
            value_mappings.get("short").unwrap().value,
            Some(serde_json::json!("dwarf"))
        );
    }

    // ── upstream spec loading: pydantic `extra="forbid"`, list forms, deprecated fields ──

    fn load(yaml: &str) -> Result<TransformationSpecification, serde_json::Error> {
        let mut v: serde_json::Value = serde_yaml_ng::from_str(yaml).unwrap();
        normalise_spec_json(&mut v);
        serde_json::from_value(v)
    }

    /// `ConfiguredBaseModel` is `extra="forbid"`: an unknown field fails the load, at any
    /// level (`transformer_model.py`; `test_spec_validation` style).
    #[test]
    fn unknown_fields_are_rejected_at_every_level() {
        for yaml in [
            "id: x\nbogus: 1\n",
            "class_derivations:\n  A:\n    bogus: 1\n",
            "class_derivations:\n  A:\n    slot_derivations:\n      s: {bogus: 2}\n",
            "enum_derivations:\n  E:\n    bogus: 3\n",
            "enum_derivations:\n  E:\n    permissible_value_derivations:\n      P: {bogus: 3}\n",
            "class_derivations:\n  A:\n    joins:\n      J: {bogus: 4}\n",
            "source_schema: {name: s, bogus: 1}\n",
            "class_derivations:\n  A:\n    slot_derivations:\n      s:\n        stringification: {bogus: 1}\n",
        ] {
            let err = load(yaml).unwrap_err().to_string();
            assert!(err.contains("unknown field `bogus`"), "{yaml}: {err}");
        }
    }

    /// The legacy `attic/transform-s1-to-s2.yaml` form: `copy_directives` keyed by the
    /// element name (`"*"`), whose `element_name` is the mapping key.
    #[test]
    fn copy_directives_take_element_name_from_the_key() {
        let spec = load(
            "class_derivations:\n  A:\n    copy_directives:\n      \"*\":\n        copy_all: true\n      B:\n",
        )
        .unwrap();
        let dirs = spec.class_derivations.unwrap()[0]
            .copy_directives
            .clone()
            .unwrap();
        assert_eq!(dirs["*"].element_name, "*");
        assert_eq!(dirs["*"].copy_all, Some(true));
        assert_eq!(dirs["B"].element_name, "B");
    }

    /// `_expand_compact_keys` and `ReferenceValidator`: list forms with compact keys.
    #[test]
    fn list_forms_and_compact_keys_load() {
        let spec = load(
            "class_derivations:\n  - A:\n      populated_from: P\n      slot_derivations:\n        - name: s\n          populated_from: t\n        - u: {expr: '1'}\n  - name: B\n\
             enum_derivations:\n  - E:\n      permissible_value_derivations:\n        - X: {populated_from: x}\n",
        )
        .unwrap();
        let cds = spec.class_derivations.unwrap();
        assert_eq!(
            cds.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["A", "B"]
        );
        let sds = cds[0].slot_derivations.as_ref().unwrap();
        assert_eq!(sds["s"].populated_from.as_deref(), Some("t"));
        assert_eq!(sds["u"].expr.as_deref(), Some("1"));
        let ed = &spec.enum_derivations.unwrap()["E"];
        assert_eq!(
            ed.permissible_value_derivations.as_ref().unwrap()["X"].populated_from,
            Some(vec!["x".to_owned()])
        );
    }

    /// `_migrate_pv_sources_to_populated_from` and `_flatten_object_derivations`.
    #[test]
    fn deprecated_fields_are_migrated_at_load() {
        let spec = load(
            "enum_derivations:\n  E:\n    permissible_value_derivations:\n      P: {sources: q}\n      Q: {sources: [a, b]}\n      R: {sources: z, populated_from: keep}\n\
             class_derivations:\n  A:\n    sources: [ignored]\n    slot_derivations:\n      s:\n        derived_from: [t]\n        object_derivations:\n          - class_derivations:\n              N: {populated_from: A}\n",
        )
        .unwrap();
        let pvds = spec.enum_derivations.unwrap()["E"]
            .permissible_value_derivations
            .clone()
            .unwrap();
        assert_eq!(pvds["P"].populated_from, Some(vec!["q".to_owned()]));
        assert_eq!(
            pvds["Q"].populated_from,
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(pvds["R"].populated_from, Some(vec!["keep".to_owned()]));
        let a = &spec.class_derivations.unwrap()[0];
        let s = &a.slot_derivations.as_ref().unwrap()["s"];
        assert_eq!(s.derived_from, Some(vec!["t".to_owned()]));
        let nested = s.class_derivations.as_ref().unwrap();
        assert_eq!(nested[0].name, "N");
        assert_eq!(nested[0].populated_from.as_deref(), Some("A"));
    }

    #[test]
    fn test_normalise_expression_mappings_shorthand() {
        let mut value = serde_json::json!({
            "class_derivations": {
                "Agent": {
                    "populated_from": "Person",
                    "slot_derivations": {
                        "display": {
                            "populated_from": "id",
                            "expression_mappings": {
                                "P:001": "name + \"!\""
                            }
                        }
                    }
                }
            }
        });
        normalise_spec_json(&mut value);
        let spec: TransformationSpecification = serde_json::from_value(value).unwrap();
        let class_derivations = spec.class_derivations.unwrap();
        let slot = &class_derivations[0].slot_derivations.as_ref().unwrap()["display"];
        let mapping = &slot.expression_mappings.as_ref().unwrap()["P:001"];
        assert_eq!(mapping.key, "P:001");
        assert_eq!(mapping.value, Some(serde_json::json!("name + \"!\"")));
    }

    #[test]
    fn test_normalise_joins_injects_alias_from_key() {
        // A `joins:` entry with only a map key (no explicit `alias:`) must
        // deserialize: the key becomes the required `AliasedClass.alias`.
        let mut value = serde_json::json!({
            "class_derivations": {
                "MeasurementObservation": {
                    "populated_from": "Measurement",
                    "joins": {
                        "Reading": {
                            "join_on": "patient_id"
                        },
                        // null-valued join body must be coerced to `{}` first.
                        "Bare": null
                    }
                }
            }
        });
        normalise_spec_json(&mut value);
        let spec: TransformationSpecification = serde_json::from_value(value)
            .expect("joins entry keyed only by alias should deserialize");
        let cds = spec.class_derivations.unwrap();
        let joins = cds[0].joins.as_ref().unwrap();
        assert_eq!(joins["Reading"].alias, "Reading");
        assert_eq!(joins["Reading"].join_on.as_deref(), Some("patient_id"));
        assert_eq!(joins["Bare"].alias, "Bare");
    }

    #[test]
    fn test_normalise_joins_explicit_alias_wins() {
        // An explicit `alias:` (differing from the map key, e.g. the
        // `class_named` divergence pattern) must NOT be clobbered by the key.
        let mut value = serde_json::json!({
            "class_derivations": {
                "Obs": {
                    "populated_from": "Measurement",
                    "joins": {
                        "Reading": {
                            "alias": "ExplicitReading",
                            "join_on": "patient_id"
                        }
                    }
                }
            }
        });
        normalise_spec_json(&mut value);
        let spec: TransformationSpecification = serde_json::from_value(value).unwrap();
        let cds = spec.class_derivations.unwrap();
        let joins = cds[0].joins.as_ref().unwrap();
        assert_eq!(joins["Reading"].alias, "ExplicitReading");
    }

    #[test]
    fn test_normalise_joins_in_nested_slot_level_class_derivation() {
        // The same fix must cover a slot-level nested `class_derivations`
        // entry's `joins:` (the other call site of
        // `normalise_class_derivation_body`, via `normalise_slot_derivations`).
        let mut value = serde_json::json!({
            "class_derivations": {
                "Parent": {
                    "populated_from": "Source",
                    "slot_derivations": {
                        "child": {
                            "class_derivations": {
                                "NestedTarget": {
                                    "populated_from": "NestedSource",
                                    "joins": {
                                        "Reading": {
                                            "join_on": "patient_id"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        });
        normalise_spec_json(&mut value);
        let spec: TransformationSpecification = serde_json::from_value(value)
            .expect("nested joins entry keyed only by alias should deserialize");
        let cds = spec.class_derivations.unwrap();
        let child = &cds[0].slot_derivations.as_ref().unwrap()["child"];
        let nested = &child.class_derivations.as_ref().unwrap()[0];
        let joins = nested.joins.as_ref().unwrap();
        assert_eq!(joins["Reading"].alias, "Reading");
        assert_eq!(joins["Reading"].join_on.as_deref(), Some("patient_id"));
    }

    fn spec_from_yaml(yaml: &str) -> TransformationSpecification {
        let mut j: serde_json::Value = serde_yaml_ng::from_str(yaml).unwrap();
        normalise_spec_json(&mut j);
        serde_json::from_value(j).unwrap()
    }

    fn first_slot(spec: &TransformationSpecification) -> &SlotDerivation {
        &spec.class_derivations.as_ref().unwrap()[0]
            .slot_derivations
            .as_ref()
            .unwrap()["x"]
    }

    // `SlotDerivation.coerce_missing_values` and `ReferenceValidator` wrap a scalar
    // bound for a list field (corpus 1ddd4049b2aa).
    #[test]
    fn scalar_for_a_list_field_is_wrapped() {
        let spec = spec_from_yaml(
            "class_derivations:\n  A:\n    mixins: B\n    comments: c\n    slot_derivations:\n      x:\n        missing_values: b\n        pivot_operation: {direction: MELT, source_slots: s, id_slots: i}\n",
        );
        let slot = first_slot(&spec);
        assert_eq!(slot.missing_values, Some(vec![serde_json::json!("b")]));
        let pivot = slot.pivot_operation.as_ref().unwrap();
        assert_eq!(pivot.source_slots, Some(vec!["s".to_owned()]));
        assert_eq!(pivot.id_slots, Some(vec!["i".to_owned()]));
        let cd = &spec.class_derivations.as_ref().unwrap()[0];
        assert_eq!(cd.mixins, Some(vec!["B".to_owned()]));
        assert_eq!(cd.comments, Some(vec!["c".to_owned()]));
    }

    // `ReferenceValidator` reads an `Any` slot as single-valued: `value: [a, b]`
    // is `a` (corpus 52b0ea786261, 39e83db9a8fa, c30315a19010), `[]` is null,
    // a mapping is left alone, and a `KeyVal`'s value is not collapsed.
    #[test]
    fn any_ranged_value_collapses_to_its_first_element() {
        let spec = spec_from_yaml(
            "class_derivations:\n  A:\n    slot_derivations:\n      x:\n        value: [a, b]\n      y:\n        value: []\n      z:\n        value: {k: [1, 2]}\n      w:\n        value_mappings:\n          k: {value: [p, q]}\n",
        );
        let slots = spec.class_derivations.as_ref().unwrap()[0]
            .slot_derivations
            .as_ref()
            .unwrap();
        assert_eq!(slots["x"].value, Some(serde_json::json!("a")));
        assert_eq!(slots["y"].value, None);
        assert_eq!(slots["z"].value, Some(serde_json::json!({"k": [1, 2]})));
        let kv = &slots["w"].value_mappings.as_ref().unwrap()["k"];
        assert_eq!(kv.value, Some(serde_json::json!(["p", "q"])));
    }

    // Slot-level `class_derivations` is a list: a repeated name keeps every
    // entry, a mapping and the compact `- Name: {...}` form are expanded
    // (corpus a8873701d43d).
    #[test]
    fn slot_class_derivations_are_a_list() {
        let spec = spec_from_yaml(
            "class_derivations:\n  A:\n    slot_derivations:\n      x:\n        class_derivations:\n        - {name: C, populated_from: P}\n        - {name: C, populated_from: Q}\n        - D: {populated_from: R}\n      y:\n        class_derivations:\n          E: {populated_from: S}\n",
        );
        let slots = spec.class_derivations.as_ref().unwrap()[0]
            .slot_derivations
            .as_ref()
            .unwrap();
        let names = |s: &SlotDerivation| -> Vec<(String, Option<String>)> {
            s.class_derivations
                .as_ref()
                .unwrap()
                .iter()
                .map(|c| (c.name.clone(), c.populated_from.clone()))
                .collect()
        };
        assert_eq!(
            names(&slots["x"]),
            vec![
                ("C".into(), Some("P".into())),
                ("C".into(), Some("Q".into())),
                ("D".into(), Some("R".into())),
            ]
        );
        assert_eq!(names(&slots["y"]), vec![("E".into(), Some("S".into()))]);
    }
}
