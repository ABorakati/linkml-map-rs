//! Shared helpers for the property-based tests (`proptest_*.rs`).
//!
//! Not every test binary uses every helper, hence the blanket `dead_code` allow.
#![allow(dead_code)]

use indexmap::IndexMap;
use linkml_map_core::decimal::Decimal;
use linkml_map_core::expr::Bindings;
use linkml_map_core::value::Value;
use proptest::prelude::*;

/// Proptest configuration with a modest default case count.
///
/// Set `PROPTEST_CASES=<n>` for a long run; the environment variable wins over
/// `default_cases` (plain `ProptestConfig::with_cases` would silently ignore it).
pub fn config(default_cases: u32) -> ProptestConfig {
    let mut config = ProptestConfig::default();
    if std::env::var_os("PROPTEST_CASES").is_none() {
        config.cases = default_cases;
    }
    // No `*.proptest-regressions` files: a stale seed for a known (ignored) bug would
    // fail the property for good. A failure prints its input and seed instead.
    config.failure_persistence = None;
    config
}

// ── Values ───────────────────────────────────────────────────────────────────

/// Strings that are interesting to the evaluator and the writers.
fn arb_text() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => "[a-zA-Z0-9 _.-]{0,12}",
        2 => any::<String>(),
        1 => "\\PC{0,12}",
        1 => Just(String::new()),
        3 => prop::sample::select(vec![
            "0", "1", "-1", "3.14", "1e5", " 7 ", "NaN", "inf", "-0", "true", "None", "null",
            "a,b", "a\tb", "line\nbreak", "quote\"d", "{x}", "x.y", "__class__", "\u{0}",
            "ß", "İ", "日本語", "🦀", "\r\n",
        ])
        .prop_map(str::to_string),
    ]
}

/// A Decimal built from generated text; any parse failure maps to `Decimal(0)`.
fn arb_decimal() -> impl Strategy<Value = Decimal> {
    "-?[0-9]{1,24}(\\.[0-9]{0,12})?([eE][+-]?[0-9]{1,3})?"
        .prop_map(|s| Decimal::parse_python(&s).unwrap_or_else(|_| Decimal::from_i64(0)))
}

fn arb_key() -> impl Strategy<Value = String> {
    prop_oneof![
        4 => prop::sample::select(vec!["id", "name", "x", "y", "value", "unit", "a", "b", "n"])
            .prop_map(str::to_string),
        1 => "[a-z_]{1,6}",
        1 => any::<String>(),
    ]
}

/// Scalars only. `finite_only` removes NaN and the infinities (JSON cannot hold them);
/// `small_ints` keeps integers within -8..=8 (see [`arb_bindings`]).
pub fn arb_scalar(finite_only: bool, small_ints: bool) -> BoxedStrategy<Value> {
    let int = if small_ints {
        (-8i64..=8).boxed()
    } else {
        prop_oneof![any::<i64>(), -5i64..5].boxed()
    };
    let float = if finite_only {
        prop_oneof![
            4 => any::<f64>().prop_filter("finite", |f| f.is_finite()),
            1 => Just(0.0),
            1 => Just(-0.0),
            1 => Just(1e22),
            1 => Just(5e-324),
        ]
        .boxed()
    } else {
        prop_oneof![
            4 => any::<f64>(),
            1 => Just(f64::NAN),
            1 => Just(f64::INFINITY),
            1 => Just(f64::NEG_INFINITY),
        ]
        .boxed()
    };
    prop_oneof![
        2 => Just(Value::Null),
        2 => any::<bool>().prop_map(Value::Bool),
        4 => int.prop_map(Value::Int),
        3 => float.prop_map(Value::Float),
        2 => arb_decimal().prop_map(Value::Decimal),
        5 => arb_text().prop_map(Value::Str),
    ]
    .boxed()
}

/// Arbitrary nested [`Value`], including NaN/inf floats and decimals.
pub fn arb_value() -> BoxedStrategy<Value> {
    arb_value_with(false, true, false)
}

/// Arbitrary nested [`Value`] that JSON/YAML/TSV can carry (finite floats; no decimals
/// unless `decimals`).
pub fn arb_value_with(finite_only: bool, decimals: bool, small_ints: bool) -> BoxedStrategy<Value> {
    arb_scalar(finite_only, small_ints)
        .prop_filter("decimal allowed", move |v| {
            decimals || !matches!(v, Value::Decimal(_))
        })
        .prop_recursive(4, 48, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..5).prop_map(Value::List),
                prop::collection::vec((arb_key(), inner), 0..5)
                    .prop_map(|pairs| Value::Map(pairs.into_iter().collect::<IndexMap<_, _>>())),
            ]
        })
        .boxed()
}

/// A record: a map from keys to values, the shape of one source row.
pub fn arb_record() -> BoxedStrategy<Value> {
    prop::collection::vec((arb_key(), arb_value()), 0..6)
        .prop_map(|pairs| Value::Map(pairs.into_iter().collect::<IndexMap<_, _>>()))
        .boxed()
}

/// Expression bindings over the names the generators below use.
///
/// `wild` bindings hold any `i64`; the other kind holds only small integers. The
/// split exists because `list * int` allocates `len * n` items up front (see the
/// `bug:` tests in `proptest_expr.rs`), so an unrestricted `x * y` would abort the
/// whole test process instead of failing one case.
pub fn arb_bindings(wild: bool) -> BoxedStrategy<Bindings> {
    prop::collection::vec(
        (
            prop::sample::select(EXPR_NAMES.to_vec()).prop_map(str::to_string),
            arb_value_with(false, true, !wild),
        ),
        0..8,
    )
    .prop_map(|pairs| pairs.into_iter().collect::<Bindings>())
    .boxed()
}

/// An arbitrary `serde_json::Value` (YAML-shaped: no NaN/inf).
pub fn arb_json() -> BoxedStrategy<serde_json::Value> {
    arb_value_with(true, false, false)
        .prop_map(|v| serde_json::Value::from(&v))
        .boxed()
}

// ── Expression sources ───────────────────────────────────────────────────────

pub const EXPR_NAMES: &[&str] = &[
    "x", "y", "z", "n", "s", "src", "target", "name", "id", "value", "unit", "items",
];

const FUNCS: &[&str] = &[
    "max",
    "min",
    "len",
    "sum",
    "sorted",
    "any",
    "all",
    "reversed",
    "first",
    "last",
    "str",
    "int",
    "float",
    "bool",
    "abs",
    "round",
    "floor",
    "ceil",
    "strlen",
    "uuid5",
    "substr",
    "upper",
    "lower",
    "title",
    "capitalize",
    "strip",
    "lstrip",
    "rstrip",
    "replace",
    "startswith",
    "endswith",
    "contains",
    "split",
    "join",
    "slugify",
    "to_snake",
    "to_camel",
    "to_pascal",
    "is_str",
    "is_int",
    "is_float",
    "is_bool",
    "is_list",
    "case",
    "coalesce",
    "is_numeric",
    "list",
    "tuple",
    "slot",
    "dict",
    "set",
    "range",
    "units",
    "NULL",
    "nope",
];

/// Binary operators. The `wild` expression profile leaves out `*`: list repetition
/// with a huge count aborts the process (see the `bug:` tests in `proptest_expr.rs`).
const BINOPS_WILD: &[&str] = &["+", "-", "/", "//", "%", "**", "^", "&", "|", "<<", ">>"];
const BINOPS_TAME: &[&str] = &[
    "+", "-", "*", "/", "//", "%", "**", "^", "&", "|", "<<", ">>",
];
const CMPOPS: &[&str] = &[
    "==", "!=", "<", "<=", ">", ">=", "in", "not in", "is", "is not",
];

fn arb_literal(wild: bool) -> BoxedStrategy<String> {
    let ints = if wild {
        prop_oneof![
            3 => any::<i64>().prop_map(|i| i.to_string()),
            3 => (-20i64..20).prop_map(|i| i.to_string()),
            3 => prop::sample::select(vec![
                "9223372036854775807", "-9223372036854775808", "9223372036854775808",
                "4294967296", "2147483648", "2147483647", "65536", "255", "256", "63", "64",
            ])
            .prop_map(str::to_string),
        ]
        .boxed()
    } else {
        (-8i64..=8).prop_map(|i| i.to_string()).boxed()
    };
    prop_oneof![
        6 => ints,
        2 => any::<f64>().prop_map(|f| format!("{f:?}")),
        1 => Just("1e400".to_string()),
        1 => Just("9223372036854775808".to_string()),
        3 => "[a-zA-Z0-9 _-]{0,8}".prop_map(|s| format!("'{s}'")),
        1 => "[a-z]{0,5}".prop_map(|s| format!("\"{s}\\n\\u00e9\"")),
        1 => Just("''".to_string()),
        3 => prop::sample::select(vec!["None", "True", "False", "NULL"]).prop_map(str::to_string),
    ]
    .boxed()
}

fn arb_atom(wild: bool) -> BoxedStrategy<String> {
    prop_oneof![
        4 => arb_literal(wild),
        4 => prop::sample::select(EXPR_NAMES.to_vec()).prop_map(str::to_string),
        2 => prop::sample::select(EXPR_NAMES.to_vec()).prop_map(|n| format!("{{{n}}}")),
        1 => (
            prop::sample::select(EXPR_NAMES.to_vec()),
            prop::sample::select(EXPR_NAMES.to_vec()),
        )
            .prop_map(|(a, b)| format!("{{{a}.{b}}}")),
    ]
    .boxed()
}

/// A syntactically plausible expression, built from the grammar (so the evaluator
/// is reached far more often than with raw text).
///
/// `wild == true`: boundary integers, but no `*` operator. `wild == false`: `*`, but
/// integers only in -8..=8. Together they cover everything without letting a
/// generated `[..] * 4611686018427387904` take the process down.
pub fn arb_expr_tree(wild: bool) -> BoxedStrategy<String> {
    let binops = if wild { BINOPS_WILD } else { BINOPS_TAME };
    arb_atom(wild)
        .prop_recursive(5, 48, 4, move |inner| {
            let name = prop::sample::select(EXPR_NAMES.to_vec());
            prop_oneof![
                3 => (inner.clone(), prop::sample::select(binops.to_vec()), inner.clone())
                    .prop_map(|(a, op, b)| format!("({a} {op} {b})")),
                1 => (inner.clone(), prop::sample::select(binops.to_vec()), inner.clone())
                    .prop_map(|(a, op, b)| format!("{a}{op}{b}")),
                2 => (inner.clone(), prop::sample::select(CMPOPS.to_vec()), inner.clone())
                    .prop_map(|(a, op, b)| format!("({a} {op} {b})")),
                1 => (
                    inner.clone(),
                    prop::sample::select(CMPOPS.to_vec()),
                    inner.clone(),
                    prop::sample::select(CMPOPS.to_vec()),
                    inner.clone(),
                )
                    .prop_map(|(a, o1, b, o2, c)| format!("({a} {o1} {b} {o2} {c})")),
                2 => (inner.clone(), prop::sample::select(vec!["and", "or"]), inner.clone())
                    .prop_map(|(a, op, b)| format!("({a} {op} {b})")),
                1 => inner.clone().prop_map(|a| format!("not {a}")),
                1 => (prop::sample::select(vec!["-", "+", "~"]), inner.clone())
                    .prop_map(|(op, a)| format!("{op}({a})")),
                4 => (
                    prop::sample::select(FUNCS.to_vec()),
                    prop::collection::vec(inner.clone(), 0..4),
                )
                    .prop_map(|(f, args)| format!("{f}({})", args.join(", "))),
                2 => prop::collection::vec(inner.clone(), 0..4)
                    .prop_map(|items| format!("[{}]", items.join(", "))),
                1 => prop::collection::vec(inner.clone(), 0..4)
                    .prop_map(|items| format!("({},)", items.join(", "))),
                1 => prop::collection::vec((inner.clone(), inner.clone()), 0..3).prop_map(
                    |pairs| {
                        let body: Vec<String> =
                            pairs.into_iter().map(|(k, v)| format!("{k}: {v}")).collect();
                        format!("{{{}}}", body.join(", "))
                    }
                ),
                2 => (inner.clone(), inner.clone()).prop_map(|(a, i)| format!("{a}[{i}]")),
                1 => (inner.clone(), inner.clone(), inner.clone())
                    .prop_map(|(a, i, j)| format!("{a}[{i}:{j}]")),
                2 => (inner.clone(), name.clone()).prop_map(|(a, n)| format!("{a}.{n}")),
                1 => (inner.clone(), inner.clone(), inner.clone())
                    .prop_map(|(a, c, b)| format!("({a} if {c} else {b})")),
                2 => (
                    inner.clone(),
                    name,
                    inner.clone(),
                    proptest::option::of(inner.clone()),
                )
                    .prop_map(|(e, v, it, cond)| match cond {
                        Some(c) => format!("[{e} for {v} in {it} if {c}]"),
                        None => format!("[{e} for {v} in {it}]"),
                    }),
                1 => prop::collection::vec(inner.clone(), 0..4)
                    .prop_map(|items| format!("{{{}}}", items.join(", "))),
                1 => (inner.clone(), inner.clone(), inner.clone(), inner.clone()).prop_map(
                    |(k, v, it, c)| format!("{{{k}: {v} for q in {it} if {c}}}")
                ),
                1 => (inner.clone(), inner.clone())
                    .prop_map(|(e, it)| format!("{{{e} for q in {it}}}")),
                1 => (inner.clone(), inner.clone(), inner.clone(), inner.clone()).prop_map(
                    |(e, a, b, c)| format!("({e} for q in {a} for r in {b} if {c})")
                ),
                2 => (
                    prop::sample::select(FUNCS.to_vec()),
                    prop::collection::vec(inner.clone(), 0..3),
                    prop::collection::vec(
                        (prop::sample::select(EXPR_NAMES.to_vec()), inner.clone()),
                        0..3,
                    ),
                )
                    .prop_map(|(f, args, kw)| {
                        let mut all = args;
                        all.extend(kw.into_iter().map(|(k, v)| format!("{k}={v}")));
                        format!("{f}({})", all.join(", "))
                    }),
                1 => inner.prop_map(|a| format!("(lambda q: {a})(1)")),
            ]
        })
        .boxed()
}

const TOKENS: &[&str] = &[
    "(",
    ")",
    "[",
    "]",
    "{",
    "}",
    ",",
    ".",
    ":",
    ";",
    "=",
    "==",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "+",
    "-",
    "*",
    "/",
    "//",
    "%",
    "**",
    "^",
    "&",
    "|",
    "~",
    "<<",
    ">>",
    "and",
    "or",
    "not",
    "in",
    "is",
    "if",
    "else",
    "elif",
    "for",
    "while",
    "def",
    "lambda",
    "return",
    "import",
    "None",
    "True",
    "False",
    "NULL",
    "target",
    "src",
    "x",
    "y",
    "len",
    "case",
    "slot",
    "uuid5",
    "0",
    "1",
    "-1",
    "3.14",
    "1e999",
    "9999999999999999999",
    "0x1f",
    "1_000",
    "1j",
    "'a'",
    "\"b\"",
    "'",
    "\"",
    "'''",
    "\\",
    "\n",
    "\n    ",
    "\n\t",
    "#",
    " ",
    "  ",
    "\t",
    "\r",
    "\u{0}",
    "é",
    "🦀",
    "__",
    "__class__",
    ".__dict__",
    "*args",
    "**kw",
    "@",
    "->",
    ":=",
    "...",
    "!",
    "$",
    "`",
    "?",
];

/// Token soup: random concatenations of lexical fragments, valid or not.
pub fn arb_token_soup() -> BoxedStrategy<String> {
    (
        prop::collection::vec(prop::sample::select(TOKENS.to_vec()), 0..40),
        any::<bool>(),
    )
        .prop_map(|(toks, spaced)| toks.join(if spaced { " " } else { "" }))
        .boxed()
}

/// A small multi-statement program (the `asteval`-style block path).
pub fn arb_program(wild: bool) -> BoxedStrategy<String> {
    let t = move || arb_expr_tree(wild);
    let stmt = prop_oneof![
        (prop::sample::select(EXPR_NAMES.to_vec()), t()).prop_map(|(n, e)| format!("{n} = {e}")),
        (t(), prop::sample::select(EXPR_NAMES.to_vec()), t())
            .prop_map(|(c, n, e)| format!("if {c}:\n    {n} = {e}")),
        (t(), t(), t())
            .prop_map(|(c, a, b)| format!("if {c}:\n    target = {a}\nelse:\n    target = {b}")),
        t(),
        (t(), t()).prop_map(|(it, e)| format!("target = [q for q in {it} if {e}]")),
    ];
    prop::collection::vec(stmt, 1..5)
        .prop_map(|stmts| {
            let mut s = stmts.join("\n");
            s.push_str("\ntarget = target\n");
            s
        })
        .boxed()
}

/// True when `src` has no single-`*` operator (`**` is fine). Conservative: it only
/// has to rule out `list * int`.
pub fn no_repetition(src: &str) -> bool {
    !src.replace("**", "").contains('*')
}

/// Any of the expression-source generators.
///
/// `wild == true` (no `*` operator, boundary integers) is for sources that meet any
/// bindings; `wild == false` (small integers, `*` allowed) for sources that meet
/// small-integer bindings.
pub fn arb_expr_source(wild: bool) -> BoxedStrategy<String> {
    let src = prop_oneof![
        4 => arb_expr_tree(wild),
        3 => arb_token_soup(),
        2 => any::<String>(),
        1 => "\\PC{0,60}",
        2 => arb_program(wild),
    ];
    if wild {
        src.prop_filter("no list repetition", |s| no_repetition(s))
            .boxed()
    } else {
        src.boxed()
    }
}

// ── Crash isolation ──────────────────────────────────────────────────────────

/// Run `body` in a child copy of this test binary, so that a stack overflow or
/// out-of-memory abort fails one test instead of killing the whole test process.
///
/// `name` must be the exact name of the calling top-level `#[test]` function. The
/// child runs on a 2 MiB thread, the stack size of a tokio/rayon worker.
pub fn crash_isolated(name: &str, body: impl FnOnce() + Send + 'static) {
    const ENV: &str = "LINKML_MAP_CRASH_CHILD";
    if std::env::var(ENV).as_deref() == Ok(name) {
        std::thread::Builder::new()
            .stack_size(2 * 1024 * 1024)
            .spawn(body)
            .expect("spawn child thread")
            .join()
            .expect("body panicked");
        return;
    }
    let exe = std::env::current_exe().expect("current_exe");
    let out = std::process::Command::new(exe)
        .args(["--exact", name, "--include-ignored", "--test-threads=1"])
        .env(ENV, name)
        .output()
        .expect("spawn child test process");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(600)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        panic!(
            "child process for `{name}` failed ({:?}): ...{tail}",
            out.status
        );
    }
}

// ── Specifications ───────────────────────────────────────────────────────────

use linkml_map_core::schema::{
    ClassDef, EnumDef, InMemorySchema, InMemorySchemaBuilder, PermissibleValue, RangeKind, SlotDef,
};
use serde_json::{Value as Json, json};

/// A small schema whose names match the generated specs (`Person`, `Row`, ...).
pub fn tiny_schema() -> InMemorySchema {
    fn slot(name: &str, range: RangeKind) -> SlotDef {
        SlotDef {
            name: name.into(),
            range,
            multivalued: false,
            inlined: false,
            inlined_as_list: false,
            required: false,
            identifier: false,
            key: false,
            unit: None,
            any_of_enums: vec![],
        }
    }
    let class = |name: &str| ClassDef {
        name: name.into(),
        tree_root: name == "Person",
        is_a: None,
        mixins: vec![],
    };
    let mut b = InMemorySchemaBuilder::new()
        .with_schema_id("https://example.org/tiny")
        .add_type("string")
        .add_type("integer")
        .add_type("float")
        .add_type("decimal")
        .add_type("boolean")
        .add_enum(EnumDef {
            name: "Kind".into(),
            permissible_values: ["A", "B"]
                .iter()
                .map(|t| PermissibleValue {
                    text: (*t).into(),
                    ..Default::default()
                })
                .collect(),
            is_dynamic: false,
        });
    for c in ["Person", "Row", "Entity", "Address", "Agent"] {
        b = b.add_class(class(c));
        let mut id = slot("id", RangeKind::Type("string".into()));
        id.identifier = true;
        let mut tags = slot("tags", RangeKind::Type("string".into()));
        tags.multivalued = true;
        let mut address = slot("address", RangeKind::Class("Address".into()));
        address.inlined = true;
        let mut friends = slot("friends", RangeKind::Class("Person".into()));
        friends.multivalued = true;
        for s in [
            id,
            slot("name", RangeKind::Type("string".into())),
            slot("x", RangeKind::Type("integer".into())),
            slot("y", RangeKind::Type("float".into())),
            slot("value", RangeKind::Type("decimal".into())),
            slot("kind", RangeKind::Enum("Kind".into())),
            tags,
            address,
            friends,
        ] {
            b = b.add_slot(c, s);
        }
    }
    b.build()
}

const CLASS_NAMES: &[&str] = &["Person", "Row", "Entity", "Address", "Agent", "Nope"];
const SLOT_NAMES: &[&str] = &[
    "id", "name", "x", "y", "value", "kind", "tags", "address", "friends", "extra",
];

const SPEC_KEYS: &[&str] = &[
    "add",
    "aggregation_operation",
    "alias",
    "author",
    "cast_collection_as",
    "class_defaults",
    "class_derivations",
    "class_name",
    "comments",
    "content_url",
    "copy_all",
    "copy_directives",
    "creator",
    "delimiter",
    "description",
    "dictionary_key",
    "direction",
    "documentation",
    "element_name",
    "enum_derivations",
    "exclude",
    "exclude_all",
    "expr",
    "expression_mappings",
    "expression_to_expression_mappings",
    "expression_to_value_mappings",
    "hide",
    "id",
    "id_slots",
    "implements",
    "include",
    "invalid_value_handling",
    "inverse_of",
    "is_a",
    "join_on",
    "joins",
    "key",
    "license",
    "local_path",
    "lookup_key",
    "mapping_method",
    "mint_ids",
    "mirror_source",
    "mixins",
    "molecular_weight",
    "name",
    "none_if_non_numeric",
    "null_handling",
    "offset",
    "operator",
    "over_slots",
    "overrides",
    "permissible_value_derivations",
    "pivot_operation",
    "populated_from",
    "prefixes",
    "publication_date",
    "range",
    "reversed",
    "reviewer",
    "slot_derivations",
    "slot_name",
    "slot_name_template",
    "source_key",
    "source_magnitude_slot",
    "source_schema",
    "source_schema_patches",
    "source_slots",
    "source_unit",
    "source_unit_scheme",
    "source_unit_slot",
    "stringification",
    "syntax",
    "target_definition",
    "target_magnitude_slot",
    "target_schema",
    "target_unit",
    "target_unit_scheme",
    "target_unit_slot",
    "title",
    "type_designator",
    "unit_conversion",
    "unit_slot",
    "url",
    "valence",
    "value",
    "value_mappings",
    "value_slot",
    "variable_slot",
    "version",
];

const ENUMISH: &[&str] = &[
    "SingleValued",
    "MultiValued",
    "MultiValuedList",
    "MultiValuedDict",
    "SimpleDict",
    "JSON",
    "YAML",
    "TURTLE",
    "SUM",
    "AVG",
    "COUNT",
    "MIN",
    "MAX",
    "ARRAY_AGG",
    "STRING_AGG",
    "DISTINCT",
    "string",
    "integer",
    "float",
    "decimal",
    "boolean",
    "Kind",
    "UCUM",
    "ucum",
    "cm",
    "m",
    "kg",
    "[in_i]",
    "mg/dL",
    "NULL",
    "None",
    "true",
    "false",
];

fn arb_name() -> BoxedStrategy<String> {
    prop_oneof![
        8 => prop::sample::select(CLASS_NAMES.to_vec()).prop_map(str::to_string),
        8 => prop::sample::select(SLOT_NAMES.to_vec()).prop_map(str::to_string),
        1 => "[A-Za-z_.{}]{0,8}",
        1 => any::<String>(),
    ]
    .boxed()
}

/// Leaf values of a spec: names, enum-ish words, expressions, numbers, junk.
fn arb_spec_leaf() -> BoxedStrategy<Json> {
    prop_oneof![
        6 => arb_name().prop_map(Json::String),
        3 => prop::sample::select(ENUMISH.to_vec()).prop_map(|s| json!(s)),
        4 => arb_expr_tree(true).prop_map(Json::String),
        1 => any::<String>().prop_filter("no repetition", |s| no_repetition(s)).prop_map(Json::String),
        1 => Just(Json::Null),
        1 => any::<bool>().prop_map(Json::Bool),
        1 => any::<i64>().prop_map(|i| json!(i)),
        1 => any::<f64>().prop_filter("finite", |f| f.is_finite()).prop_map(|f| json!(f)),
    ]
    .boxed()
}

/// Arbitrary YAML-shaped JSON whose object keys are drawn from the spec vocabulary.
pub fn arb_spec_junk() -> BoxedStrategy<Json> {
    arb_spec_leaf()
        .prop_recursive(5, 80, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..4).prop_map(Json::Array),
                prop::collection::vec(
                    (
                        prop_oneof![
                            5 => prop::sample::select(SPEC_KEYS.to_vec()).prop_map(str::to_string),
                            3 => arb_name(),
                        ],
                        inner,
                    ),
                    0..5,
                )
                .prop_map(|pairs| Json::Object(pairs.into_iter().collect())),
            ]
        })
        .boxed()
}

fn arb_slot_derivation() -> BoxedStrategy<Json> {
    let field = prop_oneof![
        prop::sample::select(SLOT_NAMES.to_vec()).prop_map(|n| ("populated_from", json!(n))),
        arb_expr_tree(true).prop_map(|e| ("expr", json!(e))),
        prop::sample::select(ENUMISH.to_vec()).prop_map(|r| ("range", json!(r))),
        arb_spec_leaf().prop_map(|v| ("value", v)),
        any::<bool>().prop_map(|b| ("hide", json!(b))),
        prop::sample::select(ENUMISH.to_vec()).prop_map(|r| ("cast_collection_as", json!(r))),
        prop::sample::select(ENUMISH.to_vec()).prop_map(|r| (
            "unit_conversion",
            json!({"target_unit": r, "source_unit": "cm"})
        )),
        arb_spec_junk().prop_map(|v| ("value_mappings", v)),
        arb_spec_junk().prop_map(|v| ("stringification", v)),
        arb_spec_junk().prop_map(|v| ("inverse_of", v)),
        arb_spec_junk().prop_map(|v| ("class_derivations", v)),
        arb_spec_junk().prop_map(|v| ("overrides", v)),
    ];
    prop_oneof![
        1 => Just(Json::Null),
        6 => prop::collection::vec(field, 0..5)
            .prop_map(|fields| Json::Object(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())),
    ]
    .boxed()
}

fn arb_class_derivation() -> BoxedStrategy<Json> {
    let slots = prop::collection::vec(
        (
            prop::sample::select(SLOT_NAMES.to_vec()).prop_map(str::to_string),
            arb_slot_derivation(),
        ),
        0..5,
    )
    .prop_map(|pairs| Json::Object(pairs.into_iter().collect()));
    let field = prop_oneof![
        3 => prop::sample::select(CLASS_NAMES.to_vec()).prop_map(|n| ("populated_from", json!(n))),
        1 => prop::sample::select(CLASS_NAMES.to_vec()).prop_map(|n| ("is_a", json!(n))),
        1 => prop::collection::vec(prop::sample::select(CLASS_NAMES.to_vec()), 0..3)
            .prop_map(|n| ("mixins", json!(n))),
        6 => slots.prop_map(|s| ("slot_derivations", s)),
        1 => arb_spec_junk().prop_map(|v| ("joins", v)),
        1 => arb_spec_junk().prop_map(|v| ("copy_directives", v)),
        1 => any::<bool>().prop_map(|b| ("mint_ids", json!(b))),
        1 => arb_expr_tree(true).prop_map(|e| ("expr", json!(e))),
        1 => arb_spec_junk().prop_map(|v| ("overrides", v)),
    ];
    prop_oneof![
        1 => Just(Json::Null),
        8 => prop::collection::vec(field, 0..6)
            .prop_map(|fields| Json::Object(fields.into_iter().map(|(k, v)| (k.to_string(), v)).collect())),
    ]
    .boxed()
}

/// A spec that mostly has the right shape (so loading, join synthesis and the engine
/// are reached), with junk in places.
pub fn arb_structured_spec() -> BoxedStrategy<Json> {
    let class_derivations = prop::collection::vec(
        (
            prop::sample::select(CLASS_NAMES.to_vec()).prop_map(str::to_string),
            arb_class_derivation(),
        ),
        1..4,
    )
    .prop_map(|pairs| Json::Object(pairs.into_iter().collect()));
    let enum_derivations = prop::collection::vec(
        (
            prop::sample::select(vec!["Kind", "Other"]).prop_map(str::to_string),
            arb_spec_junk(),
        ),
        0..2,
    )
    .prop_map(|pairs| Json::Object(pairs.into_iter().collect()));
    let field = prop_oneof![
        2 => enum_derivations.prop_map(|c| ("enum_derivations", c)),
        1 => any::<bool>().prop_map(|b| ("mint_ids", json!(b))),
        1 => arb_spec_junk().prop_map(|v| ("class_defaults", v)),
        1 => arb_spec_junk().prop_map(|v| ("prefixes", v)),
        1 => arb_spec_junk().prop_map(|v| ("source_schema", v)),
        1 => arb_spec_junk().prop_map(|v| ("target_schema", v)),
        1 => arb_spec_junk().prop_map(|v| ("slot_derivations", v)),
        1 => arb_spec_junk().prop_map(|v| ("creator", v)),
        1 => arb_spec_junk().prop_map(|v| ("copy_directives", v)),
        1 => arb_spec_junk().prop_map(|v| ("version", v)),
        1 => arb_spec_junk().prop_map(|v| ("source_schema_patches", v)),
    ];
    (class_derivations, prop::collection::vec(field, 0..5))
        .prop_map(|(cds, fields)| {
            let mut obj = serde_json::Map::new();
            obj.insert("class_derivations".into(), cds);
            obj.extend(fields.into_iter().map(|(k, v)| (k.to_string(), v)));
            Json::Object(obj)
        })
        .boxed()
}

/// Structured specs and pure junk, 3:1.
pub fn arb_spec_json() -> BoxedStrategy<Json> {
    prop_oneof![3 => arb_structured_spec(), 1 => arb_spec_junk()].boxed()
}

// ── YAML-safe strings ─────────────────────────────────────────────────────────

/// Strings and keys of every awkward kind: YAML-1.1 booleans/nulls/numbers/timestamps,
/// indicators, whitespace edge cases, long lines that fold, non-ASCII, control characters.
pub fn arb_tricky_string() -> impl Strategy<Value = String> {
    arb_any_tricky_string().prop_filter("1.1/1.2 ambiguity", |s| !ambiguous_between_1_1_and_1_2(s))
}

pub fn arb_any_tricky_string() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => any::<String>(),
        2 => "\\PC{0,200}",
        2 => "[ -~]{0,40}",
        2 => prop::sample::select(vec![
            "", " ", "  ", "yes", "No", "ON", "off", "y", "N", "true", "False", "null", "Null", "~",
            "NULL", "0", "-1", "+1", "1_000", "0o17", "017", "0x1f", "1:30", "1:30:45", "1e3", "1.5e+3",
            ".5", "5.", ".inf", "-.INF", ".NaN", "2001-12-14", "2001-12-14t21:59:43.10-05:00",
            "2001-12-14 21:59:43.10 -5", "=", "<<", "-", "- a", "? a", ": a", "a: b", "a #b", "#a",
            "&a", "*a", "!a", "|", ">", "%a", "@a", "`a", "{a}", "[a]", "a,b", "'", "\"", "a'b\"c",
            "tab\there", "line1\nline2", "trailing\n", "\nleading", "a\n\nb", "  lead", "trail  ",
            "a  b", "\u{0}", "\u{7}", "\u{85}", "\u{2028}", "\u{2029}", "\u{feff}", "\u{a0}",
            "é", "日本語", "🦀", "\u{10ffff}", "\r", "\r\n", "a\rb",
        ]).prop_map(str::to_string),
        1 => "[a-z ]{60,260}",
    ]
}

/// Whether a YAML 1.2 core-schema parser would read the plain scalar `s` as something
/// other than a string although a YAML 1.1 resolver (PyYAML) reads it as a string.
/// PyYAML writes such strings plain, e.g. `1e3` (a 1.1 float needs a `.`), so a round trip
/// through `serde_yaml_ng` is only lossless for the rest.
pub fn ambiguous_between_1_1_and_1_2(s: &str) -> bool {
    // U+0085 (NEL) is a YAML 1.1 line break; PyYAML itself writes it inside a quoted scalar
    // and reads it back as a space, so no parser can round-trip it (the port is faithful).
    if s.contains('\u{85}') {
        return true;
    }
    let t = s.trim();
    let unsigned = t.trim_start_matches(['+', '-']);
    t.parse::<f64>().is_ok()
        || unsigned.starts_with("0o")
        || unsigned.starts_with("0x")
        || unsigned.starts_with("0b")
}

/// Replace every float by a multiple of 0.001 within +-1e9: at most 13 significant
/// digits and a small exponent, which every JSON float parser reads back exactly
/// (serde_json without `float_roundtrip` does not for longer ones).
pub fn short_floats(v: &Value) -> Value {
    match v {
        Value::Float(f) => Value::Float((f.clamp(-1e9, 1e9) * 1000.0).round() / 1000.0),
        Value::List(items) => Value::List(items.iter().map(short_floats).collect()),
        Value::Map(m) => Value::Map(
            m.iter()
                .map(|(k, v)| (k.clone(), short_floats(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

/// A list of 0..6 records with 1..5 safe keys (`[a-z_]{1,6}`) each and the given value strategy.
pub fn arb_records(value: BoxedStrategy<Value>) -> BoxedStrategy<Vec<Value>> {
    prop::collection::vec(
        prop::collection::vec(("[a-z_]{1,6}", value), 1..5)
            .prop_map(|pairs| Value::Map(pairs.into_iter().collect::<IndexMap<_, _>>())),
        0..6,
    )
    .boxed()
}

/// A name from the spec vocabulary (for renaming keys).
pub fn arb_name_for_mutation() -> BoxedStrategy<String> {
    prop_oneof![
        3 => prop::sample::select(SPEC_KEYS.to_vec()).prop_map(str::to_string),
        1 => arb_name(),
    ]
    .boxed()
}
