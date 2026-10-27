//! Mutated real specifications: randomly delete, rename, retype, duplicate, swap and
//! corrupt keys and values of the fixture specs, then load them the way the CLI does
//! (spec + schemas -> `CompiledPlan`) and transform a real row. Errors are fine;
//! panics are not, and the result must be deterministic.

#[path = "../../linkml-map-core/tests/common/mod.rs"]
mod common;
mod fixtures;

use std::path::Path;

use fixtures::Fixture;
use linkml_map_core::value::Value;
use linkml_map_pipeline::CompiledPlan;
use proptest::prelude::*;
use serde_json::{Value as Json, json};

#[derive(Debug, Clone)]
enum Mutation {
    Delete,
    Rename(String),
    Retype(Json),
    Wrap(bool),
    Duplicate(usize),
    Truncate(usize),
    ReplaceString(String),
    Swap(usize),
}

#[derive(Debug, Clone)]
enum Seg {
    Key(String),
    Idx(usize),
}

fn paths(v: &Json, here: &mut Vec<Seg>, out: &mut Vec<Vec<Seg>>) {
    out.push(here.clone());
    match v {
        Json::Object(m) => {
            for (k, c) in m {
                here.push(Seg::Key(k.clone()));
                paths(c, here, out);
                here.pop();
            }
        }
        Json::Array(a) => {
            for (i, c) in a.iter().enumerate() {
                here.push(Seg::Idx(i));
                paths(c, here, out);
                here.pop();
            }
        }
        _ => {}
    }
}

fn get_mut<'a>(v: &'a mut Json, path: &[Seg]) -> Option<&'a mut Json> {
    let mut cur = v;
    for seg in path {
        cur = match (seg, cur) {
            (Seg::Key(k), Json::Object(m)) => m.get_mut(k)?,
            (Seg::Idx(i), Json::Array(a)) => a.get_mut(*i)?,
            _ => return None,
        };
    }
    Some(cur)
}

/// Apply one mutation at `path` (a no-op if the shape does not fit).
fn apply(root: &mut Json, path: &[Seg], m: &Mutation, all: &[Vec<Seg>]) {
    let Some((last, parent_path)) = path.split_last() else {
        // The root itself: only retyping makes sense.
        if let Mutation::Retype(v) = m {
            *root = v.clone();
        }
        return;
    };
    let other = |n: usize| all[n % all.len()].clone();
    match m {
        Mutation::Delete => {
            if let Some(parent) = get_mut(root, parent_path) {
                match (last, parent) {
                    (Seg::Key(k), Json::Object(o)) => {
                        o.shift_remove(k);
                    }
                    (Seg::Idx(i), Json::Array(a)) if *i < a.len() => {
                        a.remove(*i);
                    }
                    _ => {}
                }
            }
        }
        Mutation::Rename(new) => {
            if let (Seg::Key(k), Some(Json::Object(o))) = (last, get_mut(root, parent_path))
                && let Some(v) = o.shift_remove(k)
            {
                o.insert(new.clone(), v);
            }
        }
        Mutation::Retype(v) => {
            if let Some(slot) = get_mut(root, path) {
                *slot = v.clone();
            }
        }
        Mutation::Wrap(as_list) => {
            if let Some(slot) = get_mut(root, path) {
                let inner = slot.take();
                *slot = if *as_list {
                    json!([inner])
                } else {
                    json!({ "x": inner })
                };
            }
        }
        Mutation::Duplicate(n) => {
            let src = other(*n);
            let copy = get_mut(root, &src).map(|v| v.clone());
            if let (Some(copy), Some(slot)) = (copy, get_mut(root, path)) {
                match slot {
                    Json::Object(o) => {
                        o.insert("dup".into(), copy);
                    }
                    Json::Array(a) => a.push(copy),
                    other => *other = copy,
                }
            }
        }
        Mutation::Truncate(n) => {
            if let Some(Json::String(s)) = get_mut(root, path) {
                *s = s.chars().take(*n).collect();
            }
        }
        Mutation::ReplaceString(text) => {
            if let Some(slot @ Json::String(_)) = get_mut(root, path) {
                *slot = Json::String(text.clone());
            }
        }
        Mutation::Swap(n) => {
            let a = get_mut(root, path).map(|v| v.clone());
            let b = get_mut(root, &other(*n)).map(|v| v.clone());
            if let (Some(a), Some(b)) = (a, b) {
                if let Some(slot) = get_mut(root, path) {
                    *slot = b;
                }
                if let Some(slot) = get_mut(root, &other(*n)) {
                    *slot = a;
                }
            }
        }
    }
}

fn arb_mutation() -> BoxedStrategy<Mutation> {
    let junk = prop_oneof![
        Just(Json::Null),
        Just(json!(true)),
        Just(json!(0)),
        Just(json!(-1)),
        Just(json!(1.5)),
        Just(json!("")),
        Just(json!("x")),
        Just(json!([])),
        Just(json!({})),
        Just(json!([null])),
        Just(json!({"name": "Person"})),
        any::<i64>().prop_map(|i| json!(i)),
        any::<String>().prop_map(Json::String),
    ];
    prop_oneof![
        3 => Just(Mutation::Delete),
        3 => prop_oneof![
            common::arb_name_for_mutation(),
            any::<String>(),
        ].prop_map(Mutation::Rename),
        3 => junk.prop_map(Mutation::Retype),
        1 => any::<bool>().prop_map(Mutation::Wrap),
        2 => any::<usize>().prop_map(Mutation::Duplicate),
        1 => (0usize..6).prop_map(Mutation::Truncate),
        3 => common::arb_expr_source(true).prop_map(Mutation::ReplaceString),
        2 => any::<usize>().prop_map(Mutation::Swap),
    ]
    .boxed()
}

fn mutate(spec: &Json, ops: &[(prop::sample::Index, Mutation)]) -> Json {
    let mut out = spec.clone();
    for (index, m) in ops {
        let mut all = Vec::new();
        paths(&out, &mut Vec::new(), &mut all);
        let path = all[index.index(all.len())].clone();
        apply(&mut out, &path, m, &all);
    }
    out
}

fn load_spec_json(fx: &Fixture) -> Json {
    serde_yaml_ng::from_str(&std::fs::read_to_string(&fx.spec).unwrap()).unwrap()
}

/// Write the mutated spec, load a plan from it and transform the fixture's real row.
fn exercise(fx: &Fixture, spec: &Json, dir: &Path, row: &Value) -> Result<(), TestCaseError> {
    let path = dir.join("mutated.transform.yaml");
    std::fs::write(&path, serde_yaml_ng::to_string(spec).unwrap()).unwrap();
    let Ok(mut plan) = CompiledPlan::load(
        &path,
        &fx.source_schema,
        fx.target_schema.as_deref(),
        Some(fx.class),
    ) else {
        return Ok(());
    };
    // Both evaluator modes: restricted (the default) and `unrestricted_eval`.
    for unrestricted in [false, true] {
        plan.set_unrestricted_eval(unrestricted);
        let first = plan.transform(row.clone());
        let second = plan.transform(row.clone());
        prop_assert_eq!(format!("{first:?}"), format!("{second:?}"));
    }
    plan.set_unrestricted_eval(true);
    let _ = plan.transform(Value::Null);
    let _ = plan.transform(Value::Map(Default::default()));
    let _ = plan.missing_identifiers();
    let _ = plan.requires_object_index();
    Ok(())
}

fn config(default_cases: u32) -> ProptestConfig {
    let mut c = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        c.cases = default_cases;
    }
    c.failure_persistence = None;
    c
}

macro_rules! mutation_test {
    ($name:ident, $fixture:ident) => {
        proptest! {
            #![proptest_config(config(48))]
            #[test]
            fn $name(ops in prop::collection::vec((any::<prop::sample::Index>(), arb_mutation()), 1..5)) {
                let fx = fixtures::$fixture();
                let mutated = mutate(&load_spec_json(&fx), &ops);
                let dir = tempfile::tempdir().unwrap();
                exercise(&fx, &mutated, dir.path(), &fx.data_row)?;
            }
        }
    };
}

mutation_test!(mutated_measurements_spec, measurements);
mutation_test!(mutated_personinfo_spec, personinfo);
mutation_test!(mutated_flattening_spec, flattening);
mutation_test!(mutated_type_coercion_spec, type_coercion);

/// Sanity: the unmutated fixtures load and transform their real row.
#[test]
fn unmutated_fixtures_load() {
    for fx in fixtures::all() {
        let mut plan = CompiledPlan::load(
            &fx.spec,
            &fx.source_schema,
            fx.target_schema.as_deref(),
            Some(fx.class),
        )
        .unwrap_or_else(|e| panic!("{}: {e:#}", fx.name));
        plan.set_unrestricted_eval(fx.unrestricted);
        // A foreign-key spec (flattening) cannot transform a lone row without an object index;
        // `proptest_pipeline.rs` runs it over whole containers.
        if !plan.requires_object_index() {
            plan.transform(fx.data_row.clone())
                .unwrap_or_else(|e| panic!("{} rejects its own row: {e:#}", fx.name));
        }
    }
}

/// Guard against vacuous mutation tests: a good share of mutants must still load.
#[test]
fn mutants_often_still_load() {
    use proptest::strategy::ValueTree;
    let fx = fixtures::personinfo();
    let spec = load_spec_json(&fx);
    let dir = tempfile::tempdir().unwrap();
    let mut runner = proptest::test_runner::TestRunner::deterministic();
    let strat = prop::collection::vec((any::<prop::sample::Index>(), arb_mutation()), 1..3);
    let (mut loaded, mut ran) = (0, 0);
    for _ in 0..60 {
        let ops = strat.new_tree(&mut runner).unwrap().current();
        let mutated = mutate(&spec, &ops);
        let path = dir.path().join("m.yaml");
        std::fs::write(&path, serde_yaml_ng::to_string(&mutated).unwrap()).unwrap();
        if let Ok(mut plan) = CompiledPlan::load(
            &path,
            &fx.source_schema,
            fx.target_schema.as_deref(),
            Some(fx.class),
        ) {
            plan.set_unrestricted_eval(true);
            loaded += 1;
            if plan.transform(fx.data_row.clone()).is_ok() {
                ran += 1;
            }
        }
    }
    assert!(loaded >= 10, "only {loaded}/60 mutants load");
    assert!(ran >= 5, "only {ran}/60 mutants transform the row");
}
