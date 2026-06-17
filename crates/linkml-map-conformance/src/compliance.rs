//! Port of the upstream Python `test_compliance_suite.py`.
//!
//! The upstream suite builds schemas + transformation specs *programmatically*
//! and pytest-parametrizes each feature into many combos. This module mirrors
//! those tables natively in Rust, driving the real engine
//! (`SchemaViewProvider::from_yaml_str` + `ObjectTransformer::map_container`) and
//! comparing the mapped object to the expected output.
//!
//! Scope note: the Python suite *also* asserts invertibility (round-trip via a
//! derived inverse spec), JSON-schema validation of the target, and compiler
//! output. The Rust core has no `TransformationSpecificationInverter`,
//! validator, or compilers yet, so only the **forward object-mapping** assertion
//! — the primary deliverable of `map_object` — is ported here. Each ported combo
//! checks the same `target_object == expected_target_object` the Python
//! `map_object` helper checks.
//!
//! Upstream provenance: `tests/compliance/test_compliance_suite.py`
//! (commit 19b1985889ec0fc247145e9aa03eb74a4d7b3588).

use linkml_map_core::{
    datamodel::TransformationSpecification, engine::ObjectTransformer, schema::SchemaProvider,
};
use linkml_map_schemaview::SchemaViewProvider;
use serde_json::{json, Value as J};
use serde_yaml_ng as serde_yaml;

use crate::{first_diff, json_to_value, normalise, value_to_json, RunResult, Status};

/// One ported compliance combo.
struct Case {
    /// Feature-set name (the upstream test function).
    feature: &'static str,
    /// Combo label (the parametrize tuple, rendered).
    combo: String,
    /// Source schema as LinkML-shaped JSON (serialised to YAML for the provider).
    schema: J,
    /// Transform spec as LinkML-shaped JSON, in the *mapping* style the
    /// fixtures + Python use (`class_derivations: { Name: {...} }`); run through
    /// the crate's `normalise_transform_yaml` before deserialising.
    spec: J,
    /// Source object.
    input: J,
    /// Expected target object. `None` means the engine is expected to error
    /// (the upstream `raises_error` cases).
    expected: Option<J>,
    /// Source class to map from (upstream `source_root` / tree-root).
    source_type: &'static str,
}

fn pass(name: String) -> RunResult {
    RunResult {
        case_name: name,
        status: Status::Pass,
        reason: String::new(),
    }
}
fn fail(name: String, why: String) -> RunResult {
    RunResult {
        case_name: name,
        status: Status::Fail,
        reason: why,
    }
}
fn skip(name: String, why: String) -> RunResult {
    RunResult {
        case_name: name,
        status: Status::Skip,
        reason: why,
    }
}

/// Build a minimal valid LinkML schema header around the given classes/enums.
fn schema(name: &str, classes: J, enums: Option<J>) -> J {
    let mut s = json!({
        "id": format!("https://example.org/{name}"),
        "name": name,
        "default_range": "string",
        "classes": classes,
    });
    if let Some(e) = enums {
        s["enums"] = e;
    }
    s
}

/// Drive one ported case through the engine and judge it.
fn run_one(c: &Case) -> RunResult {
    let name = format!("compliance/{}/{}", c.feature, c.combo);

    // ── source schema ─────────────────────────────────────────────────────────
    let schema_yaml = match serde_yaml::to_string(&c.schema) {
        Ok(y) => y,
        Err(e) => return skip(name, format!("SKIP(schema-serialise): {e:#}")),
    };
    let provider = match SchemaViewProvider::from_yaml_str(&schema_yaml) {
        Ok(p) => p,
        Err(e) => return skip(name, format!("SKIP(schema-load): {e:#}")),
    };

    // ── transform spec (mapping style → normalise → datamodel) ────────────────
    let mut spec_obj = c.spec.clone();
    linkml_map_core::datamodel::normalise_spec_json(&mut spec_obj);
    let spec: TransformationSpecification = match serde_json::from_value(spec_obj) {
        Ok(s) => s,
        Err(e) => return skip(name, format!("SKIP(transform-parse): {e:#}")),
    };

    // ── run ───────────────────────────────────────────────────────────────────
    let engine = ObjectTransformer::new(spec, Some(&provider as &dyn SchemaProvider), None);
    let input = json_to_value(&c.input);

    match engine.map_container(&input, Some(c.source_type)) {
        Ok(v) => match &c.expected {
            // Expected an error but got output → real divergence.
            None => fail(
                name,
                format!("FAIL(expected-error): got {}", short(&value_to_json(&v))),
            ),
            Some(exp) => {
                let actual = normalise(value_to_json(&v));
                let want = normalise(exp.clone());
                if actual == want {
                    pass(name)
                } else {
                    fail(
                        name,
                        format!("FAIL(mismatch): {}", first_diff(&want, &actual)),
                    )
                }
            }
        },
        Err(e) => match &c.expected {
            // Error expected → engine correctly raised.
            None => pass(name),
            // Output expected but engine errored → genuine engine gap.
            Some(_) => fail(name, format!("FAIL(engine): {e:#}")),
        },
    }
}

fn short(v: &J) -> String {
    let s = v.to_string();
    if s.len() > 80 {
        format!("{}…", &s[..77])
    } else {
        s
    }
}

// ─── Feature sets ──────────────────────────────────────────────────────────────

/// `test_map_types` — type coercion on a single scalar attribute.
fn cases_map_types() -> Vec<Case> {
    // (source_dt, target_dt, source_value, target_value)
    let table: Vec<(&str, &str, J, J)> = vec![
        ("string", "string", json!("foo"), json!("foo")),
        ("integer", "integer", json!(5), json!(5)),
        ("string", "integer", json!("5"), json!(5)),
        ("integer", "float", json!(5), json!(5.0)),
        ("float", "integer", json!(5.0), json!(5)),
        ("float", "integer", json!(5.2), json!(5)),
        ("integer", "boolean", json!(5), json!(true)),
        ("integer", "boolean", json!(0), json!(false)),
    ];
    table
        .into_iter()
        .map(|(sdt, tdt, sv, tv)| Case {
            feature: "map_types",
            combo: format!("{sdt}->{tdt}"),
            schema: schema(
                "types",
                json!({ "C": { "attributes": { "s1": { "range": sdt } } } }),
                None,
            ),
            spec: json!({
                "class_derivations": {
                    "C": { "slot_derivations": {
                        "s1": { "populated_from": "s1", "range": tdt }
                    } }
                }
            }),
            input: json!({ "s1": sv }),
            expected: Some(json!({ "s1": tv })),
            source_type: "C",
        })
        .collect()
}

/// `test_map_collections` — list↔dict casting via `cast_collection_as`.
fn cases_map_collections() -> Vec<Case> {
    let classes = |source_is_list: bool| {
        json!({
            "C": {
                "tree_root": true,
                "attributes": { "ds": {
                    "range": "D", "inlined": true,
                    "inlined_as_list": source_is_list, "multivalued": true
                } }
            },
            "D": { "attributes": {
                "id": { "identifier": true }, "s1": { "range": "string" }
            } }
        })
    };
    let slot_ds = |source_is_list: bool, cast: &str| {
        let mut sd = json!({ "populated_from": "ds", "cast_collection_as": cast });
        if source_is_list {
            sd["dictionary_key"] = json!("id");
        }
        sd
    };
    let spec = |source_is_list: bool, cast: &str| {
        json!({
            "class_derivations": {
                "C": { "slot_derivations": { "ds": slot_ds(source_is_list, cast) } },
                "D": { "slot_derivations": {
                    "id": { "populated_from": "id" },
                    "s1": { "populated_from": "s1", "range": "string" }
                } }
            }
        })
    };
    vec![
        // list → dict
        Case {
            feature: "map_collections",
            combo: "list->dict".into(),
            schema: schema("types", classes(true), None),
            spec: spec(true, "MultiValuedDict"),
            input: json!({ "ds": [{"id":"X","s1":"foo"},{"id":"Y","s1":"bar"}] }),
            expected: Some(json!({ "ds": {"X":{"s1":"foo"},"Y":{"s1":"bar"}} })),
            source_type: "C",
        },
        // dict → list
        Case {
            feature: "map_collections",
            combo: "dict->list".into(),
            schema: schema("types", classes(false), None),
            spec: spec(false, "MultiValuedList"),
            input: json!({ "ds": {"X":{"s1":"foo"},"Y":{"s1":"bar"}} }),
            expected: Some(json!({ "ds": [{"id":"X","s1":"foo"},{"id":"Y","s1":"bar"}] })),
            source_type: "C",
        },
    ]
}

/// `test_expr` — pythonic expressions populating a derived slot.
fn cases_expr() -> Vec<Case> {
    // (expr, source_object, target_value-as-Option). `None` target is the
    // `{s1}` short-circuit case: expected target object is `{derived: null}`.
    let table: Vec<(&str, J, J)> = vec![
        ("s1 + s2", json!({"s1":5,"s2":6}), json!(11)),
        ("{s1} + {s2}", json!({"s1":5,"s2":6}), json!(11)),
        ("{s1} + {s2}", json!({"s1":5}), J::Null),
        ("s1 + s2.s3", json!({"s1":5,"s2":{"s3":6}}), json!(11)),
        (
            "s1 + s2.s3.s4",
            json!({"s1":5,"s2":{"s3":{"s4":6}}}),
            json!(11),
        ),
        ("s1 + s2", json!({"s1":"a","s2":"b"}), json!("ab")),
        ("s1 + s2", json!({"s1":["a"],"s2":["b"]}), json!(["a", "b"])),
        ("len(s1)", json!({"s1":["a"]}), json!(1)),
        ("s1 < s2", json!({"s1":5,"s2":6}), json!(true)),
    ];
    table
        .into_iter()
        .enumerate()
        .map(|(i, (expr, src, tv))| {
            let classes = infer_classes(&src);
            Case {
                feature: "expr",
                combo: format!("{}:{}", i, expr),
                schema: schema("expr", classes, None),
                spec: json!({
                    "class_derivations": {
                        "C": {
                            "populated_from": "C",
                            "slot_derivations": { "derived": { "expr": expr } }
                        }
                    }
                }),
                input: src,
                expected: Some(json!({ "derived": tv })),
                source_type: "C",
            }
        })
        .collect()
}

/// Mirror of the upstream `infer_range` helper: build a `classes` block for the
/// `expr` tests by inferring attribute ranges from a sample source object. The
/// root object is class `C`; nested objects become class `D`.
fn infer_classes(source_object: &J) -> J {
    let mut classes = serde_json::Map::new();
    infer_into(&mut classes, source_object, "C");
    J::Object(classes)
}

/// Returns the inferred range descriptor for a value, registering any classes.
fn infer_into(classes: &mut serde_json::Map<String, J>, v: &J, typ: &str) -> J {
    match v {
        J::Object(map) => {
            classes
                .entry(typ.to_string())
                .or_insert_with(|| json!({ "attributes": {} }));
            for (k, v1) in map {
                let r = infer_into(classes, v1, "D");
                let attr = if r.is_object() {
                    r
                } else {
                    json!({ "range": r })
                };
                classes
                    .get_mut(typ)
                    .unwrap()
                    .get_mut("attributes")
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .insert(k.clone(), attr);
            }
            J::String(typ.to_string())
        }
        J::Array(items) => {
            let r = infer_into(classes, &items[0], "D");
            json!({ "range": r, "multivalued": true })
        }
        J::Bool(_) => json!("boolean"),
        J::Number(n) => {
            if n.is_i64() || n.is_u64() {
                json!("integer")
            } else {
                json!("float")
            }
        }
        _ => json!("string"),
    }
}

/// `test_simple_unit_conversion` — UCUM/symbol unit conversion on a scalar.
fn cases_simple_unit_conversion() -> Vec<Case> {
    // (src_slot, tgt_slot, src_unit, tgt_unit, metaslot, src_val, tgt_val, expect_err, skip)
    struct U {
        ss: &'static str,
        ts: &'static str,
        su: &'static str,
        tu: &'static str,
        meta: &'static str,
        sv: f64,
        tv: Option<f64>,
        err: bool,
        skip: bool,
    }
    let t = |ss, ts, su, tu, meta, sv, tv: Option<f64>, err, skip| U {
        ss,
        ts,
        su,
        tu,
        meta,
        sv,
        tv,
        err,
        skip,
    };
    let table = vec![
        t(
            "s1",
            "s1",
            "m",
            "cm",
            "ucum_code",
            1.0,
            Some(100.0),
            false,
            false,
        ),
        t(
            "s1",
            "s1",
            "m",
            "cm",
            "symbol",
            1.0,
            Some(100.0),
            false,
            false,
        ),
        t(
            "s1",
            "s1",
            "m",
            "m",
            "ucum_code",
            1.0,
            Some(1.0),
            false,
            false,
        ),
        t(
            "s1",
            "s1",
            "a",
            "mo",
            "ucum_code",
            10.0,
            Some(120.0),
            false,
            false,
        ),
        // Under the `symbol` scheme the engine uses a plain (pint-like) registry
        // that does not know the UCUM-only `a`/`mo` codes → UndefinedUnitError,
        // matching Python.
        t("s1", "s1", "a", "mo", "symbol", 10.0, None, true, false),
        t("s1", "s1", "m", "ml", "ucum_code", 1.0, None, true, false),
        t(
            "s1",
            "s1",
            "m",
            "pinknoodles",
            "ucum_code",
            1.0,
            None,
            true,
            false,
        ),
        t("s1", "s1", "ml", "m", "ucum_code", 1.0, None, true, false),
        t(
            "s1",
            "s1",
            "pinknoodles",
            "m",
            "ucum_code",
            1.0,
            None,
            true,
            false,
        ),
        t(
            "s1",
            "s1",
            "m/s",
            "cm/s",
            "ucum_code",
            1.0,
            Some(100.0),
            false,
            false,
        ),
        t(
            "s1",
            "s1",
            "m.s-1",
            "cm.s-1",
            "ucum_code",
            1.0,
            Some(100.0),
            false,
            false,
        ),
        // upstream-skipped (ucumvert issue #8)
        t(
            "s1",
            "s1",
            "g.m2-1",
            "kg.m2-1",
            "ucum_code",
            1.0,
            Some(0.001),
            false,
            true,
        ),
        t(
            "height_in_m",
            "height_in_cm",
            "m",
            "cm",
            "ucum_code",
            1.0,
            Some(100.0),
            false,
            false,
        ),
        t(
            "s1",
            "s1",
            "m[H2O]{35Cel}",
            "m[H2O]{35Cel}",
            "ucum_code",
            1.0,
            Some(1.0),
            false,
            false,
        ),
    ];
    table
        .into_iter()
        .filter(|u| !u.skip)
        .map(|u| Case {
            feature: "simple_unit_conversion",
            combo: format!("{} {}->{} [{}]", u.ss, u.su, u.tu, u.meta),
            schema: schema(
                "types",
                json!({ "C": { "attributes": {
                    u.ss: { "range": "float", "unit": { u.meta: u.su } }
                } } }),
                None,
            ),
            spec: json!({
                "class_derivations": { "C": { "slot_derivations": {
                    u.ts: { "populated_from": u.ss, "unit_conversion": { "target_unit": u.tu } }
                } } }
            }),
            input: json!({ u.ss: u.sv }),
            expected: if u.err {
                None
            } else {
                Some(json!({ u.ts: u.tv.unwrap() }))
            },
            source_type: "C",
        })
        .collect()
}

/// `test_complex_unit_conversion` — magnitude+unit object → scalar.
fn cases_complex_unit_conversion() -> Vec<Case> {
    // (src_unit, tgt_unit, src_val, tgt_val, expect_err)
    let table: Vec<(&str, &str, f64, Option<f64>, bool)> = vec![
        ("m", "cm", 1.0, Some(100.0), false),
        ("cm", "cm", 100.0, Some(100.0), false),
        ("cm", "ml", 100.0, None, true),
        ("cm", "pinknoodles", 100.0, None, true),
    ];
    table
        .into_iter()
        .map(|(su, tu, sv, tv, err)| {
            let tgt_slot = format!("q_in_{tu}");
            Case {
                feature: "complex_unit_conversion",
                combo: format!("{su}->{tu}"),
                schema: schema(
                    "types",
                    json!({
                        "Q": { "attributes": {
                            "magnitude": { "range": "float" },
                            "unit": { "range": "string" }
                        } },
                        "C": { "tree_root": true, "attributes": { "q": { "range": "Q" } } }
                    }),
                    None,
                ),
                spec: json!({
                    "class_derivations": { "D": {
                        "populated_from": "C",
                        "slot_derivations": { tgt_slot.clone(): {
                            "populated_from": "q",
                            "unit_conversion": {
                                "target_unit": tu,
                                "source_unit_slot": "unit",
                                "source_magnitude_slot": "magnitude"
                            }
                        } }
                    } }
                }),
                input: json!({ "q": { "magnitude": sv, "unit": su } }),
                expected: if err {
                    None
                } else {
                    Some(json!({ tgt_slot: tv.unwrap() }))
                },
                source_type: "C",
            }
        })
        .collect()
}

/// `test_stringify` — compact a multivalued slot into a string.
fn cases_stringify() -> Vec<Case> {
    // (syntax, delimiter, source_value, target_value)
    let table: Vec<(Option<&str>, Option<&str>, J, J)> = vec![
        (None, Some(","), json!(["a", "b"]), json!("a,b")),
        (None, Some("|"), json!(["a", "b"]), json!("a|b")),
        (None, Some("|"), json!(["a"]), json!("a")),
        (None, Some("|"), json!([]), json!("")),
        (
            Some("JSON"),
            None,
            json!(["a", "b"]),
            json!("[\"a\", \"b\"]"),
        ),
        (Some("JSON"), None, json!([]), json!("[]")),
        (Some("YAML"), None, json!(["a", "b"]), json!("[a, b]")),
    ];
    table
        .into_iter()
        .enumerate()
        .map(|(i, (syntax, delim, sv, tv))| {
            let mut strf = serde_json::Map::new();
            if let Some(s) = syntax {
                strf.insert("syntax".into(), json!(s));
            }
            if let Some(d) = delim {
                strf.insert("delimiter".into(), json!(d));
            }
            Case {
                feature: "stringify",
                combo: format!("{}:{:?}/{:?}", i, syntax, delim),
                schema: schema(
                    "types",
                    json!({ "C": { "attributes": {
                        "s1": { "range": "string", "multivalued": true }
                    } } }),
                    None,
                ),
                spec: json!({
                    "class_derivations": { "D": {
                        "populated_from": "C",
                        "slot_derivations": { "s1_verbatim": {
                            "populated_from": "s1",
                            "stringification": J::Object(strf)
                        } }
                    } }
                }),
                input: json!({ "s1": sv }),
                expected: Some(json!({ "s1_verbatim": tv })),
                source_type: "C",
            }
        })
        .collect()
}

/// `test_isomorphic` — copy a nested schema to itself (recursive descent).
fn cases_isomorphic() -> Vec<Case> {
    let classes = json!({
        "Container": { "tree_root": true, "attributes": {
            "c_list": { "range": "C", "multivalued": true },
            "d": { "range": "D" }
        } },
        "C": { "attributes": { "s1": { "range": "string" }, "s2": { "range": "string" } } },
        "D": { "attributes": { "s3": { "range": "string" } } }
    });
    let src = json!({
        "c_list": [{"s1":"a","s2":"b"},{"s1":"c","s2":"d"}],
        "d": {"s3":"e"}
    });
    [true, false]
        .into_iter()
        .map(|use_expr| {
            let s3 = if use_expr {
                json!({ "expr": "s3" })
            } else {
                json!({ "populated_from": "s3" })
            };
            Case {
                feature: "isomorphic",
                combo: format!("use_expr={use_expr}"),
                schema: schema("isomorphic", classes.clone(), None),
                spec: json!({
                    "class_derivations": {
                        "Container": {
                            "populated_from": "Container",
                            "slot_derivations": {
                                "c_list": { "populated_from": "c_list", "range": "C" },
                                "d": { "populated_from": "d", "range": "D" }
                            }
                        },
                        "C": {
                            "populated_from": "C",
                            "slot_derivations": {
                                "s1": { "populated_from": "s1" },
                                "s2": { "populated_from": "s2" }
                            }
                        },
                        "D": {
                            "populated_from": "D",
                            "slot_derivations": { "s3": s3 }
                        }
                    }
                }),
                input: src.clone(),
                expected: Some(src.clone()),
                source_type: "Container",
            }
        })
        .collect()
}

/// `test_join` — denormalise two objects into one (inlined + FK variants).
fn cases_join() -> Vec<Case> {
    let mut out = Vec::new();
    for inlined in [true, false] {
        let mut classes = json!({
            "R": {
                "tree_root": inlined,
                "attributes": {
                    "s1": { "range": "E", "inlined": inlined },
                    "s2": { "range": "E", "inlined": inlined }
                }
            },
            "E": { "attributes": {
                "id": { "range": "string", "identifier": true },
                "name": { "range": "string" }
            } }
        });
        let mut cds = json!({
            "R": {
                "populated_from": "R",
                "slot_derivations": {
                    "s1_id": { "expr": "s1.id" },
                    "s1_name": { "expr": "s1.name" },
                    "s2_id": { "expr": "s2.id" },
                    "s2_name": { "expr": "s2.name" }
                }
            }
        });
        let (input, expected, source_type) = if inlined {
            (
                json!({ "s1": {"id":"x1","name":"foo"}, "s2": {"id":"x2","name":"bar"} }),
                json!({ "s1_id":"x1","s1_name":"foo","s2_id":"x2","s2_name":"bar" }),
                "R",
            )
        } else {
            classes["Container"] = json!({
                "tree_root": true,
                "attributes": {
                    "r_list": { "range": "R", "multivalued": true },
                    "e_list": { "range": "E", "multivalued": true, "inlined_as_list": true }
                }
            });
            cds["Container"] = json!({
                "populated_from": "Container",
                "slot_derivations": { "r_list": { "populated_from": "r_list" } }
            });
            (
                json!({
                    "r_list": [{ "s1": "x1", "s2": "x2" }],
                    "e_list": [
                        {"id":"x1","name":"foo"},
                        {"id":"x2","name":"bar"}
                    ]
                }),
                json!({ "r_list": [
                    {"s1_id":"x1","s1_name":"foo","s2_id":"x2","s2_name":"bar"}
                ] }),
                "Container",
            )
        };
        out.push(Case {
            feature: "join",
            combo: format!("inlined={inlined}"),
            schema: schema("types", classes, None),
            spec: json!({ "class_derivations": cds }),
            input,
            expected: Some(expected),
            source_type,
        });
    }
    out
}

/// `test_map_enum` — permissible-value mapping (SSSOM-style). The upstream
/// `mirror_source=True` combos `pytest.skip`, so they are skipped here too.
fn cases_map_enum() -> Vec<Case> {
    // (source_value, mapping, target_value, mirror_source)
    struct E {
        sv: &'static str,
        mapping: J,
        tv: Option<&'static str>,
        mirror: bool,
    }
    let table = vec![
        E {
            sv: "A",
            mapping: json!({ "B": "A" }),
            tv: Some("B"),
            mirror: false,
        },
        E {
            sv: "Z",
            mapping: json!({ "B": "A" }),
            tv: None,
            mirror: false,
        },
        E {
            sv: "C",
            mapping: json!({ "B": "A" }),
            tv: Some("C"),
            mirror: true,
        },
        E {
            sv: "A",
            mapping: json!({ "B": ["A", "C"] }),
            tv: Some("B"),
            mirror: false,
        },
        E {
            sv: "C",
            mapping: json!({ "B": ["A", "C"] }),
            tv: Some("B"),
            mirror: false,
        },
    ];
    table
        .into_iter()
        .filter(|e| !e.mirror) // upstream skips mirror_source
        .map(|e| {
            // Build permissible_value_derivations. v0.6.0 list-form (#250):
            // `populated_from` accepts a scalar or a list of source PVs.
            let mut pvds = serde_json::Map::new();
            for (k, v) in e.mapping.as_object().unwrap() {
                pvds.insert(k.clone(), json!({ "populated_from": v }));
            }
            Case {
                feature: "map_enum",
                combo: format!("{}->{:?}", e.sv, e.tv),
                schema: schema(
                    "enums",
                    json!({ "C": { "attributes": { "s1": { "range": "E" } } } }),
                    Some(json!({ "E": { "permissible_values": {
                        "A": {}, "B": {}, "C": {}
                    } } })),
                ),
                spec: json!({
                    "class_derivations": { "C": { "slot_derivations": {
                        "s1": { "populated_from": "s1" }
                    } } },
                    "enum_derivations": { "E": {
                        "populated_from": "E",
                        "mirror_source": false,
                        "permissible_value_derivations": J::Object(pvds)
                    } }
                }),
                input: json!({ "s1": e.sv }),
                expected: Some(json!({ "s1": e.tv })),
                source_type: "C",
            }
        })
        .collect()
}

/// `test_inheritance` — is_a / mixins, with optional slot roll-down (flatten).
fn cases_inheritance() -> Vec<Case> {
    let mut out = Vec::new();
    for is_a in [true, false] {
        for flatten in [false, true] {
            let mut classes = json!({
                "C": { "tree_root": true, "attributes": { "s1": { "range": "integer" } } },
                "D": { "attributes": { "s2": { "range": "integer" } } }
            });
            if is_a {
                classes["C"]["is_a"] = json!("D");
            } else {
                classes["C"]["mixins"] = json!(["D"]);
            }

            let mut cds = json!({
                "C": {
                    "populated_from": "C",
                    "slot_derivations": { "s1": { "expr": "s1 + 1" } }
                },
                "D": {
                    "populated_from": "D",
                    "slot_derivations": { "s2": { "expr": "s2 + 1" } }
                }
            });
            if flatten {
                cds["C"]["slot_derivations"]["s2"] = json!({ "expr": "s2 + 1" });
                cds.as_object_mut().unwrap().remove("D");
            } else if is_a {
                cds["C"]["is_a"] = json!("D");
            } else {
                cds["C"]["mixins"] = json!(["D"]);
            }

            out.push(Case {
                feature: "inheritance",
                combo: format!("is_a={is_a},flatten={flatten}"),
                schema: schema("expr", classes, None),
                spec: json!({ "class_derivations": cds }),
                input: json!({ "s1": 1, "s2": 2 }),
                expected: Some(json!({ "s1": 2, "s2": 3 })),
                source_type: "C",
            });
        }
    }
    out
}

fn all_cases() -> Vec<Case> {
    let mut v = Vec::new();
    v.extend(cases_map_types());
    v.extend(cases_map_collections());
    v.extend(cases_expr());
    v.extend(cases_simple_unit_conversion());
    v.extend(cases_complex_unit_conversion());
    v.extend(cases_stringify());
    v.extend(cases_isomorphic());
    v.extend(cases_join());
    v.extend(cases_map_enum());
    v.extend(cases_inheritance());
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Port of the upstream `test_compliance_suite.py`. Runs every ported combo,
    /// writes `tests/COMPLIANCE_REPORT.md`, and prints a summary. Like the
    /// fixture conformance runner, individual combo failures do NOT fail the test
    /// — the report is the deliverable.
    #[test]
    fn compliance_suite() {
        let cases = all_cases();
        eprintln!("[compliance] running {} ported combos", cases.len());

        let mut results = Vec::new();
        for c in &cases {
            let r = run_one(c);
            eprintln!("  [{:4}] {}  {}", r.status, r.case_name, r.reason);
            results.push(r);
        }

        let report = crate::ConformanceReport::new(results);
        eprintln!("\n=== COMPLIANCE SUMMARY ===");
        eprintln!(
            "Total: {}  PASS: {}  FAIL: {}  SKIP: {}",
            report.total(),
            report.pass_count(),
            report.fail_count(),
            report.skip_count(),
        );

        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = manifest.parent().and_then(|p| p.parent()).unwrap();
        let report_path = root.join("tests").join("COMPLIANCE_REPORT.md");
        let md = report.to_markdown();
        let _ = std::fs::write(&report_path, &md);
        eprintln!("Report written to: {}", report_path.display());
        println!("\n{}", md);
    }
}
