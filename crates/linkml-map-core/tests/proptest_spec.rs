//! Property tests for specification loading and the engine's spec-driven set-up.
//!
//! Whatever YAML-shaped value arrives as a "spec" must either load or produce an
//! error, and everything downstream of a loaded spec (join synthesis, expression
//! compilation, semantic validation, content-id set-up, inversion and finally the
//! engine itself) must not panic either.

mod common;

use common::*;
use linkml_map_core::datamodel::{TransformationSpecification, normalise_spec_json};
use linkml_map_core::engine::content_id::ContentIds;
use linkml_map_core::engine::{CompiledExprs, ObjectTransformer};
use linkml_map_core::inference::TransformationSpecificationInverter;
use linkml_map_core::normalize::synthesize_implicit_joins;
use linkml_map_core::schema::SchemaProvider;
use linkml_map_core::validate::validate_spec_semantics;
use proptest::prelude::*;
use serde_json::Value as Json;

/// `load_transform_spec` minus the file: normalise, then deserialise.
fn load(mut json: Json) -> Result<TransformationSpecification, serde_json::Error> {
    normalise_spec_json(&mut json);
    serde_json::from_value(json)
}

/// Everything the CLI/pipeline does with a spec before the first row.
fn exercise_setup(spec: &TransformationSpecification) {
    let schema = tiny_schema();
    let sv: &dyn SchemaProvider = &schema;

    let mut joined = spec.clone();
    let _ = synthesize_implicit_joins(&mut joined, sv);
    let _ = CompiledExprs::build(&joined);
    let _ = ContentIds::for_spec(sv, &joined);
    for strict in [false, true] {
        let _ = validate_spec_semantics(&joined, Some(sv), Some(sv), strict);
        let _ = validate_spec_semantics(&joined, Some(sv), None, strict);
    }
    let _ = TransformationSpecificationInverter::new(sv).invert(spec);
    let _ = TransformationSpecificationInverter::non_strict(sv).invert(spec);
}

proptest! {
    #![proptest_config(config(256))]

    /// Normalisation + deserialisation + set-up never panic on arbitrary YAML-shaped input.
    #[test]
    fn spec_loading_never_panics(json in arb_spec_json()) {
        if let Ok(spec) = load(json) {
            exercise_setup(&spec);
        }
    }

    /// Arbitrary JSON of any shape (not just spec-vocabulary keys) is also safe.
    #[test]
    fn normalise_never_panics_on_arbitrary_json(json in arb_json()) {
        let _ = load(json);
    }

    /// Normalisation is idempotent: a normalised spec is a fixed point.
    #[test]
    fn normalise_is_idempotent(json in arb_spec_json()) {
        let mut once = json;
        normalise_spec_json(&mut once);
        let mut twice = once.clone();
        normalise_spec_json(&mut twice);
        prop_assert_eq!(once, twice);
    }

    /// A loaded spec survives a serialise/deserialise round trip unchanged.
    #[test]
    fn spec_serde_round_trip(json in arb_structured_spec()) {
        if let Ok(spec) = load(json) {
            let back = serde_json::to_value(&spec).expect("a loaded spec serialises");
            let again: TransformationSpecification =
                serde_json::from_value(back).expect("a serialised spec deserialises");
            prop_assert_eq!(spec, again);
        }
    }

    /// Run the engine over arbitrary records with an arbitrary (loadable) spec.
    #[test]
    #[allow(clippy::redundant_clone)]
    fn engine_never_panics_on_arbitrary_specs(
        json in arb_structured_spec(),
        record in arb_record(),
        pick in 0usize..4,
        mint in prop::bool::weighted(0.7),
    ) {
        run_engine(json, &record, pick, mint)?;
    }
}

/// Load `json`, run the engine on `record`, and check it is a pure function.
///
/// `pick` chooses the source type: 0 = let the engine resolve it, otherwise the
/// source class of the `pick`-th class derivation (so that a derivation actually
/// runs). `mint` turns on `mint_ids`, since most generated specs derive no id.
fn run_engine(
    json: Json,
    record: &linkml_map_core::value::Value,
    pick: usize,
    mint: bool,
) -> Result<(), TestCaseError> {
    let Ok(mut spec) = load(json) else {
        return Ok(());
    };
    if mint {
        spec.mint_ids = Some(true);
    }
    let schema = tiny_schema();
    let sv: &dyn SchemaProvider = &schema;
    if synthesize_implicit_joins(&mut spec, sv).is_err() {
        return Ok(());
    }
    let Ok(compiled) = CompiledExprs::build(&spec) else {
        return Ok(());
    };
    let source_type: Option<String> = match pick {
        0 => None,
        n => spec
            .class_derivations
            .iter()
            .flatten()
            .nth(n - 1)
            .map(|cd| cd.populated_from.clone().unwrap_or_else(|| cd.name.clone())),
    };
    let source_type = source_type.as_deref();
    for strict in [false, true] {
        let engine = ObjectTransformer::new(spec.clone(), Some(sv), Some(sv))
            .with_compiled_exprs(&compiled)
            .with_strict_exprs(strict);
        let first = engine.map_object(record, source_type);
        let second = engine.map_object(record, source_type);
        // Pure function of (spec, record): same answer twice.
        prop_assert_eq!(format!("{first:?}"), format!("{second:?}"));
    }
    // Container path (builds the foreign-key index first).
    let _ = ObjectTransformer::new(spec, Some(sv), None).map_container(record, source_type);
    Ok(())
}

/// Guard against vacuous tests: a deterministic sample of the generators must reach
/// the engine and produce output reasonably often, not just fail to load.
#[test]
fn generators_reach_the_engine() {
    use proptest::strategy::{Strategy, ValueTree};
    use proptest::test_runner::TestRunner;

    let mut runner = TestRunner::deterministic();
    let strategy = (arb_structured_spec(), arb_record());
    let (mut loaded, mut produced) = (0, 0);
    for _ in 0..400 {
        let (json, record) = strategy.new_tree(&mut runner).unwrap().current();
        let Ok(mut spec) = load(json) else { continue };
        loaded += 1;
        spec.mint_ids = Some(true);
        let schema = tiny_schema();
        let sv: &dyn SchemaProvider = &schema;
        if synthesize_implicit_joins(&mut spec, sv).is_err() {
            continue;
        }
        let source = spec
            .class_derivations
            .iter()
            .flatten()
            .next()
            .map(|cd| cd.populated_from.clone().unwrap_or_else(|| cd.name.clone()));
        let engine = ObjectTransformer::new(spec, Some(sv), Some(sv));
        if engine.map_object(&record, source.as_deref()).is_ok() {
            produced += 1;
        }
    }
    assert!(loaded >= 40, "only {loaded}/400 generated specs load");
    assert!(
        produced >= 25,
        "only {produced}/400 generated specs transform a record"
    );
}
