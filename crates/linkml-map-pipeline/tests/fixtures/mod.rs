//! The real fixtures the property tests run over, plus row generators for each.
#![allow(dead_code)]

use std::path::PathBuf;

use indexmap::IndexMap;
use linkml_map_core::value::Value;
use proptest::prelude::*;

pub struct Fixture {
    pub name: &'static str,
    pub spec: PathBuf,
    pub source_schema: PathBuf,
    pub target_schema: Option<PathBuf>,
    /// A real input row (used as the base for mutation tests).
    pub data_row: Value,
    /// The source class the rows belong to.
    pub class: &'static str,
    /// Needs `unrestricted_eval` (the spec has `expr:` statement blocks).
    pub unrestricted: bool,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/examples")
}

fn load_yaml(path: PathBuf) -> serde_json::Value {
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_yaml_ng::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

pub fn measurements() -> Fixture {
    let d = root().join("measurements");
    Fixture {
        name: "measurements",
        spec: d.join("transform/qv-to-scalar.transform.yaml"),
        source_schema: d.join("source/quantity_value.yaml"),
        target_schema: Some(d.join("target/quantity_value_flat.yaml")),
        data_row: Value::from(&load_yaml(d.join("data/PersonQuantityValue-001.yaml"))),
        class: "Person",
        unrestricted: false,
    }
}

pub fn personinfo() -> Fixture {
    let d = root().join("personinfo_basic");
    Fixture {
        name: "personinfo",
        spec: d.join("transform/personinfo-to-agent.transform.yaml"),
        source_schema: d.join("source/personinfo.yaml"),
        target_schema: Some(d.join("target/agent.yaml")),
        data_row: Value::from(&load_yaml(d.join("data/Person-001.yaml"))),
        class: "Person",
        unrestricted: true,
    }
}

pub fn flattening() -> Fixture {
    let d = root().join("flattening");
    let container = load_yaml(d.join("data/MappingSet-001.yaml"));
    Fixture {
        name: "flattening",
        spec: d.join("transform/denormalize.transform.yaml"),
        source_schema: d.join("source/normalized.yaml"),
        target_schema: Some(d.join("target/denormalized.yaml")),
        data_row: Value::from(&container["mappings"][0]),
        class: "Mapping",
        unrestricted: false,
    }
}

pub fn type_coercion() -> Fixture {
    let d = root().join("type_coercion");
    Fixture {
        name: "type_coercion",
        spec: d.join("transform/type_coercion.transform.yaml"),
        source_schema: d.join("source/type_coercion.yaml"),
        // PARITY: upstream raises on every row with the example's target schema (its
        // `MyRecord` has no `id_as_url`: `induced_slot` fails, `ValueError`), as does the
        // engine, so this fixture runs without a target schema.
        target_schema: None,
        data_row: Value::from(&load_yaml(d.join("data/MyRecord-001.yaml"))),
        class: "MyRecord",
        unrestricted: false,
    }
}

pub fn all() -> Vec<Fixture> {
    vec![measurements(), personinfo(), flattening(), type_coercion()]
}

// ── Row generators ───────────────────────────────────────────────────────────

fn map(pairs: Vec<(&str, Value)>) -> Value {
    Value::Map(
        pairs
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect::<IndexMap<_, _>>(),
    )
}

fn text() -> BoxedStrategy<Value> {
    prop_oneof![
        4 => "[A-Za-z0-9 :_.-]{0,12}".prop_map(Value::Str),
        1 => any::<String>().prop_map(Value::Str),
        1 => Just(Value::Null),
        1 => any::<i64>().prop_map(Value::Int),
    ]
    .boxed()
}

fn number() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => (-1000.0f64..1000.0).prop_map(|f| Value::Float((f * 100.0).round() / 100.0)),
        2 => (-1000i64..1000).prop_map(Value::Int),
        1 => "[0-9]{1,4}(\\.[0-9]{1,3})?".prop_map(Value::Str),
        1 => any::<String>().prop_map(Value::Str),
        1 => Just(Value::Null),
    ]
    .boxed()
}

/// Rows for a fixture; optional fields come and go so that missing/typed-wrong values occur.
pub fn arb_row(name: &'static str) -> BoxedStrategy<Value> {
    let opt = |s: BoxedStrategy<Value>| proptest::option::weighted(0.8, s);
    match name {
        "measurements" => (
            opt("P:[0-9]{1,3}".prop_map(Value::Str).boxed()),
            proptest::option::weighted(
                0.85,
                (
                    opt(number()),
                    opt(prop::sample::select(vec!["cm", "m", "mm", "in", "kg", ""])
                        .prop_map(|u| Value::Str(u.into()))
                        .boxed()),
                )
                    .prop_map(|(v, u)| {
                        let mut pairs = vec![];
                        if let Some(v) = v {
                            pairs.push(("value", v));
                        }
                        if let Some(u) = u {
                            pairs.push(("unit", u));
                        }
                        map(pairs)
                    }),
            ),
        )
            .prop_map(|(id, height)| {
                let mut pairs = vec![];
                if let Some(id) = id {
                    pairs.push(("id", id));
                }
                if let Some(h) = height {
                    pairs.push(("height", h));
                }
                map(pairs)
            })
            .boxed(),
        "personinfo" => {
            let event = (
                prop::sample::select(vec![
                    "BIRTH",
                    "PASSED_DRIVING_TEST",
                    "DEATH",
                    "HIRED",
                    "bogus",
                ]),
                opt("(19|20)[0-9]{2}-[01][0-9]-[0-3][0-9]"
                    .prop_map(Value::Str)
                    .boxed()),
            )
                .prop_map(|(n, d)| {
                    let mut pairs = vec![("event_name", Value::Str(n.into()))];
                    if let Some(d) = d {
                        pairs.push(("important_event_date", d));
                    }
                    map(pairs)
                });
            let rel = (
                prop::sample::select(vec!["SIBLING_OF", "CHILD_OF", "PARENT_OF", "nope"]),
                "P:[0-9]{1,3}",
            )
                .prop_map(|(t, r)| {
                    map(vec![
                        ("type", Value::Str(t.into())),
                        ("related_to", Value::Str(r)),
                    ])
                });
            let address = (opt(text()), opt(text())).prop_map(|(s, c)| {
                let mut pairs = vec![];
                if let Some(s) = s {
                    pairs.push(("street", s));
                }
                if let Some(c) = c {
                    pairs.push(("city", c));
                }
                map(pairs)
            });
            (
                (
                    opt(text()),
                    opt(text()),
                    opt(number()),
                    opt(text()),
                    opt(text()),
                ),
                proptest::option::weighted(0.8, prop::collection::vec(event, 0..4)),
                proptest::option::weighted(0.7, prop::collection::vec(rel, 0..3)),
                proptest::option::weighted(0.7, address),
            )
                .prop_map(|((id, name, age, email, gender), events, rels, addr)| {
                    let mut pairs = vec![];
                    for (k, v) in [
                        ("id", id),
                        ("name", name),
                        ("age_in_years", age),
                        ("primary_email", email.clone()),
                        ("secondary_email", email),
                        ("gender", gender),
                    ] {
                        if let Some(v) = v {
                            pairs.push((k, v));
                        }
                    }
                    if let Some(e) = events {
                        pairs.push(("has_important_life_events", Value::List(e)));
                    }
                    if let Some(r) = rels {
                        pairs.push(("has_familial_relationships", Value::List(r)));
                    }
                    if let Some(a) = addr {
                        pairs.push(("current_address", a));
                    }
                    map(pairs)
                })
                .boxed()
        }
        "flattening" => (opt(text()), opt(text()), opt(text()), opt(text()))
            .prop_map(|(id, s, o, p)| {
                let mut pairs = vec![];
                for (k, v) in [("id", id), ("subject", s), ("object", o), ("predicate", p)] {
                    if let Some(v) = v {
                        pairs.push((k, v));
                    }
                }
                map(pairs)
            })
            .boxed(),
        "type_coercion" => (opt(text()), opt(text()), opt(number()), opt(text()))
            .prop_map(|(id, name, age, url)| {
                let mut pairs = vec![];
                for (k, v) in [
                    ("id", id),
                    ("name", name),
                    ("age_as_integer", age),
                    ("url", url),
                ] {
                    if let Some(v) = v {
                        pairs.push((k, v));
                    }
                }
                map(pairs)
            })
            .boxed(),
        other => panic!("no generator for {other}"),
    }
}
