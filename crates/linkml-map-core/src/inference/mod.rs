//! Inference utilities over a [`TransformationSpecification`].
//!
//! [`TransformationSpecificationInverter`] is a port of Python
//! `linkml_map.inference.inverter`: given a forward spec (source -> target) it
//! derives the inverse spec (target -> source). [`SchemaMapper`] ports
//! `schema_mapper.py`. Both read the source schema through [`SchemaView`], a port of
//! the slice of `linkml_runtime.SchemaView` they use.
mod induced_slot;
mod inverter;
mod metamodel;
mod schema_mapper;
mod schema_view;

pub use inverter::{TransformationSpecificationInverter, dump_inverted_spec};
pub use metamodel::MetamodelError;
pub use schema_mapper::SchemaMapper;
pub use schema_view::SchemaView;

#[cfg(test)]
mod schema_mapper_tests;
#[cfg(test)]
mod tests;
