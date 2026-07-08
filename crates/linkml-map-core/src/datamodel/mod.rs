//! LinkML transformation specification datamodel.
//!
//! This module provides Rust serde structs for deserializing LinkML-map
//! transform specifications from YAML. The structs mirror the Python dataclasses
//! in linkml_map.datamodel.transformer_model.

use indexmap::IndexMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

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
        Some(scalar) => Some(vec![stringify(scalar)]),
    })
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
/// `SchemaReference`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct SchemaReference {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

impl SchemaReference {
    /// The usable path/identifier for loading: prefers `local_path`, falling
    /// back to `name`, then `url`.
    pub fn path(&self) -> Option<&str> {
        self.local_path
            .as_deref()
            .or(self.name.as_deref())
            .or(self.url.as_deref())
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
pub struct SpecificationComponent {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub implements: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<String>>,
}

/// A key-value pair used in mappings.
///
/// Mirrors Python `KeyVal`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct KeyVal {
    pub key: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,
}

/// Configuration for unit conversion.
///
/// Mirrors Python `UnitConversionConfiguration`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
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
pub struct StringificationConfiguration {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delimiter: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub reversed: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub over_slots: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub syntax: Option<SerializationSyntaxType>,
}

/// Inverse (back-reference) specification for relational mappings.
///
/// Mirrors Python `Inverse`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
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
pub struct GroupingOperation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub null_handling: Option<InvalidValueHandlingStrategy>,
}

/// Pivot (melt/unmelt) operation configuration.
///
/// Mirrors Python `PivotOperation`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PivotOperation {
    pub direction: PivotDirectionType,

    #[serde(default = "default_variable_slot")]
    pub variable_slot: String,

    #[serde(default = "default_value_slot")]
    pub value_slot: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmelt_to_class: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmelt_to_slots: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_slot: Option<String>,

    #[serde(default = "default_slot_name_template")]
    pub slot_name_template: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_slots: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
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
#[serde(tag = "type", rename_all = "PascalCase")]
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
pub struct SlotDerivation {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub populated_from: Option<String>,

    /// Nested target objects derived from source data (v0.6.0: replaces the
    /// removed `object_derivations` — one nesting level flatter). Mirrors
    /// Python `SlotDerivation.class_derivations`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_derivations: Option<IndexMap<String, ClassDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,

    /// Sentinel source values that map to null (e.g. `-9`, `999`, `"NA"`).
    /// When the derived value equals one of these, the slot is set to null.
    /// v0.6.0 (#269). Mirrors Python `SlotDerivation.missing_values`.
    #[serde(skip_serializing_if = "Option::is_none")]
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

    #[serde(skip_serializing_if = "Option::is_none")]
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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub implements: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<String>>,
}

/// A specification of how to derive a target enum PV from a source enum PV.
///
/// Mirrors Python `PermissibleValueDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct PermissibleValueDerivation {
    pub name: String,

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

    #[serde(skip_serializing_if = "Option::is_none")]
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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub implements: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<String>>,
}

/// A specification of how to derive a target enum from a source enum.
///
/// Mirrors Python `EnumDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct EnumDerivation {
    pub name: String,

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

    #[serde(skip_serializing_if = "Option::is_none")]
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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub implements: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<String>>,
}

// `ObjectDerivation` removed in v0.6.0 — nested object derivations now live
// directly on `SlotDerivation.class_derivations` (one nesting level flatter).

/// A specification of how to derive a target class from source class(es).
///
/// Mirrors Python `ClassDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ClassDerivation {
    pub name: String,

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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_directives: Option<IndexMap<String, CopyDirective>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub overrides: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_a: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub implements: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<String>>,
}

/// The root transformation specification.
///
/// A collection of mappings between source and target classes, along with
/// enum derivations and global configuration.
///
/// Mirrors Python `TransformationSpecification`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub implements: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<String>>,
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
    if let Some(obj) = root.as_object_mut() {
        // ── class_derivations: top-level mapping -> Vec ────────────────────────
        if let Some(cd) = obj.get_mut("class_derivations")
            && cd.is_object() {
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

        // ── top-level slot_derivations: inject `name` from the mapping key ────
        if let Some(sd) = obj.get_mut("slot_derivations")
            && let Some(sdm) = sd.as_object_mut() {
                normalise_slot_derivations(sdm);
            }

        // ── enum_derivations: inject `name` ───────────────────────────────────
        if let Some(ed) = obj.get_mut("enum_derivations")
            && let Some(edm) = ed.as_object_mut() {
                for (enum_name, enum_val) in edm.iter_mut() {
                    if enum_val.is_null() {
                        *enum_val = serde_json::json!({});
                    }
                    if let Some(eo) = enum_val.as_object_mut() {
                        if !eo.contains_key("name") {
                            eo.insert("name".into(), serde_json::Value::String(enum_name.clone()));
                        }
                        if let Some(pvds) = eo.get_mut("permissible_value_derivations")
                            && let Some(pvm) = pvds.as_object_mut() {
                                for (pv_name, pv_val) in pvm.iter_mut() {
                                    if pv_val.is_null() {
                                        *pv_val = serde_json::json!({});
                                    }
                                    if let Some(po) = pv_val.as_object_mut()
                                        && !po.contains_key("name") {
                                            po.insert(
                                                "name".into(),
                                                serde_json::Value::String(pv_name.clone()),
                                            );
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
            && let Some(m) = pfx.as_object_mut() {
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
                && let Some(arr) = agents.as_array_mut() {
                    for agent in arr.iter_mut() {
                        if let Some(o) = agent.as_object_mut()
                            && !o.contains_key("type") {
                                o.insert("type".into(), serde_json::Value::String("Agent".into()));
                            }
                    }
                }
        }
    }
}

/// Normalise the body of a single class derivation in place: walk its
/// `slot_derivations`, injecting names and recursing into any slot-level
/// `class_derivations` (v0.6.0 nested objects). The class's own `name` is
/// expected to be set by the caller (from the mapping key).
fn normalise_class_derivation_body(o: &mut serde_json::Map<String, serde_json::Value>) {
    if let Some(sd) = o.get_mut("slot_derivations")
        && let Some(sdm) = sd.as_object_mut() {
            normalise_slot_derivations(sdm);
        }
    // ── joins: inject `alias` from the mapping key ─────────────────────────
    // `joins` is a mapping keyed by alias name (`Option<IndexMap<String,
    // AliasedClass>>`); `AliasedClass.alias` is required, so mirror the
    // "map key becomes the name/alias field" rule used for class/slot/enum
    // derivations. Only fill `alias` when absent so an explicit `alias:` in
    // the YAML (e.g. the `class_named` divergence pattern) wins.
    if let Some(j) = o.get_mut("joins")
        && let Some(jm) = j.as_object_mut() {
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
            normalise_keyval_mapping(so, "value_mappings");
            normalise_keyval_mapping(so, "expression_mappings");
            // Slot-level nested class_derivations stay a mapping (IndexMap);
            // inject each class name from its key and recurse.
            if let Some(cd) = so.get_mut("class_derivations")
                && let Some(cdm) = cd.as_object_mut() {
                    for (cls_name, cls_val) in cdm.iter_mut() {
                        if cls_val.is_null() {
                            *cls_val = serde_json::json!({});
                        }
                        if let Some(co) = cls_val.as_object_mut() {
                            co.entry("name")
                                .or_insert_with(|| serde_json::Value::String(cls_name.clone()));
                            normalise_class_derivation_body(co);
                        }
                    }
                }
        }
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
                    && let Some(expr) = o.remove("expr") {
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
          "source_schema": { "name": "src", "local_path": "schemas/src.yaml" },
          "target_schema": "tgt.yaml"
        }"#;
        let spec: TransformationSpecification = serde_json::from_str(json).unwrap();
        let src = spec.source_schema.unwrap();
        assert_eq!(src.name.as_deref(), Some("src"));
        assert_eq!(src.local_path.as_deref(), Some("schemas/src.yaml"));
        // path() prefers local_path.
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
        let nested = &child.class_derivations.as_ref().unwrap()["NestedTarget"];
        let joins = nested.joins.as_ref().unwrap();
        assert_eq!(joins["Reading"].alias, "Reading");
        assert_eq!(joins["Reading"].join_on.as_deref(), Some("patient_id"));
    }
}
