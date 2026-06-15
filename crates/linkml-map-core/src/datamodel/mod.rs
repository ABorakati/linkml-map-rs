//! LinkML transformation specification datamodel.
//!
//! This module provides Rust serde structs for deserializing LinkML-map
//! transform specifications from YAML. The structs mirror the Python dataclasses
//! in linkml_map.datamodel.transformer_model.

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

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
        #[serde(skip_serializing_if = "Option::is_none")]
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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_derivations: Option<Vec<ObjectDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub expr: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Value>,

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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub populated_from: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,

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
    pub sources: Option<Vec<String>>,

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

/// A specification of how to derive target object instances.
///
/// Mirrors Python `ObjectDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ObjectDerivation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub class_derivations: Option<IndexMap<String, ClassDerivation>>,

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

/// A specification of how to derive a target class from source class(es).
///
/// Mirrors Python `ClassDerivation`.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ClassDerivation {
    pub name: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub populated_from: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Vec<String>>,

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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub prefixes: Option<IndexMap<String, KeyVal>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub copy_directives: Option<IndexMap<String, CopyDirective>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_schema: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_schema: Option<String>,

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

    #[serde(skip_serializing_if = "Option::is_none")]
    pub enum_derivations: Option<IndexMap<String, EnumDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot_derivations: Option<IndexMap<String, SlotDerivation>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub implements: Option<Vec<String>>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<Vec<String>>,
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
        assert_eq!(spec.source_schema, Some("source.yaml".to_string()));
        assert_eq!(spec.target_schema, Some("target.yaml".to_string()));
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
}
