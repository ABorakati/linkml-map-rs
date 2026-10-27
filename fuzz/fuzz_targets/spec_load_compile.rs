#![no_main]
//! Specification YAML -> normalise -> deserialise -> join synthesis -> expression
//! compilation -> semantic validation -> the engine on a small record.

use fuzz_support::{MAX_EXPR_LEN, UNGUARDED, has_repetition, tiny_schema};
use libfuzzer_sys::fuzz_target;
use linkml_map_core::datamodel::{TransformationSpecification, normalise_spec_json};
use linkml_map_core::engine::{CompiledExprs, ObjectTransformer};
use linkml_map_core::normalize::synthesize_implicit_joins;
use linkml_map_core::schema::SchemaProvider;
use linkml_map_core::validate::validate_spec_semantics;
use linkml_map_core::value::Value;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    if !UNGUARDED && (text.len() > 4 * MAX_EXPR_LEN || has_repetition(text)) {
        return;
    }
    let Ok(mut json) = serde_yaml_ng::from_str::<serde_json::Value>(text) else {
        return;
    };
    normalise_spec_json(&mut json);
    let Ok(mut spec) = serde_json::from_value::<TransformationSpecification>(json) else {
        return;
    };
    spec.mint_ids = Some(true);
    let schema = tiny_schema();
    let sv: &dyn SchemaProvider = &schema;
    let _ = validate_spec_semantics(&spec, Some(sv), Some(sv), false);
    if synthesize_implicit_joins(&mut spec, sv).is_err() {
        return;
    }
    let Ok(compiled) = CompiledExprs::build(&spec) else {
        return;
    };
    let record: Value = serde_json::from_str(
        r#"{"id": "P:1", "name": "n", "x": 3, "y": 1.5, "value": "2.50", "kind": "A", "address": {"id": "A:1", "name": "a"}}"#,
    )
    .unwrap();
    let source = spec
        .class_derivations
        .iter()
        .flatten()
        .next()
        .map(|cd| cd.populated_from.clone().unwrap_or_else(|| cd.name.clone()));
    for strict in [false, true] {
        let engine = ObjectTransformer::new(spec.clone(), Some(sv), Some(sv))
            .with_compiled_exprs(&compiled)
            .with_strict_exprs(strict);
        let _ = engine.map_object(&record, source.as_deref());
    }
});
