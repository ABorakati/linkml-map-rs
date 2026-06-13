# Schema Backend Spike: `SchemaProvider` for linkml-map-rs

_Investigator: Claude Sonnet 4.6 — June 2026_

---

## 1. Schema operations the engine needs (from Part 1 audit)

The following table was derived by reading
`linkml_map/transformer/object_transformer.py` and
`linkml_map/transformer/transformer.py` in the Python linkml-map package.
Every `sv.*` call is listed with the context where it appears.

| Method called on `SchemaView` (Python) | Call site / purpose |
|---|---|
| `sv.all_classes()` | Resolve tree-root class when `source_type` is not given; iterate all classes for type/enum dispatch |
| `sv.all_enums()` | Check if `source_type` is an enum before dispatching `transform_enum` |
| `sv.all_types()` | Check if `source_type` is a scalar type (string/int/float/…); coerce at type-range boundary |
| `sv.induced_slot(slot_name, class_name)` | The _most-called_ method; used for every slot that has a `populated_from` or `sources` binding to read range, multivalued, unit annotation |
| `sv.class_induced_slots(class_name)` | FK-path resolution (`resolve_fk_path`); join resolution to discover `field_path` belongs to `joined_class` |
| `sv.get_identifier_slot(class_name, use_key=True)` | `_reshape_collection`: list → compact-dict keying; determines the identifier/key slot of the range class |
| `slot.range` | Range of source slot, used to recursively `map_object` nested values |
| `slot.multivalued` | Cardinality coercion in `_coerce_cardinality` and `_is_coerce_to_multivalued/singlevalued` |
| `slot.unit` (+ `.ucum_code`, `.iec61360code`, `.symbol`, `.abbreviation`, `.descriptive_name`) | `_perform_unit_conversion`: resolve source unit from schema annotation |
| `slot.any_of` / `any_of[*].range` | `_get_any_of_enum_names`: detect enum via any_of when range is None/Any |
| `target_schemaview.induced_slot(name, class_name)` | `_is_coerce_to_multivalued/singlevalued`: target-side cardinality check; `_derive_nested_objects`: multivalued check for nested objects |
| `target_schemaview.class_induced_slots(class_name)` | `_perform_unmelt`: valid slot names for the unmelt target class |

**Summary of distinct operations needed:**

1. **`get_class(name)`** — class metadata (tree_root, is_a, mixins).
2. **`all_class_names()`** — iterate all classes; tree-root discovery.
3. **`all_enum_names()`** — dispatch: is this source_type an enum?
4. **`all_type_names()`** — dispatch: is this source_type a scalar?
5. **`induced_slots(class_name)`** — all effective slots for a class (FK, join, unmelt).
6. **`induced_slot(slot_name, class_name)`** — single slot lookup (range, multivalued, unit, any_of enums).
7. **`identifier_slot(class_name)`** — key/identifier slot for compact-dict reshape and FK indexing.
8. **`get_enum(name)`** — permissible values for enum transform dispatch.

These are exactly the eight methods defined on `SchemaProvider` in
`crates/linkml-map-core/src/schema/mod.rs`.

---

## 2. Is linkml-runtime-rust usable as a native Cargo dependency?

### 2a. What "linkml-runtime-rust" actually is

The PyPI wheel `linkml_runtime_rust-0.2.1-cp312-cp312-win_amd64.whl` (vendored
at `C:\Users\abora\LinkML-MCP\vendor\`) and PyPI `linkml-runtime-rust` are the
Python-facing build product of the **p2p-ld/linkml-core** monorepo on GitHub
(`https://github.com/p2p-ld/linkml-core`).

That monorepo is a pure-Rust workspace with the following crates:

| Crate (internal name) | Package name | What it provides |
|---|---|---|
| `linkml_meta` | `linkml-meta` | Auto-generated serde structs for the LinkML metamodel (`ClassDefinition`, `SlotDefinition`, `EnumDefinition`, …) |
| `linkml_schemaview` | `schemaview` | `SchemaView`, `ClassView`, `SlotView`, `EnumView` — full SchemaView implementation in Rust |
| `linkml_runtime` | `linkml-runtime` | YAML/JSON parsing, diff/patch, Turtle serialisation, basic validation |
| `linkml_tools` | (CLI tools) | CLI wrappers |
| `linkml_python` | (Python bindings) | PyO3 glue; this is what the wheel exposes |

The key fact is that **`linkml_meta` and `linkml_schemaview` are pure-Rust
libraries**. PyO3 is an optional feature on `linkml_meta`. The Python wheel
activates that feature, but the underlying Rust API is fully usable without it.

### 2b. Crates.io availability

As of June 2026, **neither `linkml-meta` nor `schemaview` (nor any
`linkml-runtime-rust` umbrella crate) is published to crates.io**. The crate
package names inside the workspace (`linkml_meta`, `schemaview`) do not have
corresponding crates.io entries. A crates.io search for `linkml`, `linkml-meta`,
`linkml-schemaview`, or `linkml-runtime` returns unrelated crates.

Web search confirms the project lives only on GitHub; no crates.io publication
announcement was found in any 2024/2025 release notes or issue tracker.

### 2c. The local copy

The full source is already checked out at
`C:\Users\abora\LinkML-MCP\rust\` (the `linkml-runtime-rust` monorepo).
It was confirmed readable by inspecting:
- `rust/src/schemaview/src/schemaview.rs` — ~1600 lines, full `SchemaView` implementation
- `rust/src/schemaview/src/classview.rs` — `ClassView` with `slots()`, `identifier_slot()`, `key_or_identifier_slot()`
- `rust/src/schemaview/src/slotview.rs` — `SlotView` with `definition()`, `get_range_info()`, `determine_slot_container_mode()`
- `rust/src/schemaview/src/enumview.rs` — `EnumView` with `permissible_value_keys()`
- `rust/src/metamodel/Cargo.toml` — `linkml_meta 0.0.0`, depends on `serde`, `merge`, `chrono`; PyO3 is optional

The `SchemaView` API matches what `SchemaProvider` needs: `get_classdefinition`,
`get_slot`, `class_canonical_ids`, type ancestry, enum lookup.

---

## 3. Options for backing `SchemaProvider`

### Option A — Native Cargo path dependency on `p2p-ld/linkml-core`

**How:** Add a path or git dependency on the local `linkml-runtime-rust` workspace crates:

```toml
# In linkml-map-core/Cargo.toml
[dependencies]
linkml_meta     = { path = "../../linkml-mcp-rs/rust/src/metamodel" }
linkml_schemaview = { path = "../../linkml-mcp-rs/rust/src/schemaview" }
```

Or, once published or pinned via git:

```toml
linkml_schemaview = { git = "https://github.com/p2p-ld/linkml-core", rev = "..." }
```

**Adapter sketch:**

```rust
use linkml_schemaview::SchemaView;
pub struct SchemaViewAdapter(SchemaView);

impl SchemaProvider for SchemaViewAdapter {
    fn get_class(&self, name: &str) -> SchemaResult<ClassDef> {
        let conv = self.0.converter();
        let cv = self.0.get_class(&Identifier::new(name), &conv)
            .map_err(|e| SchemaError::Other(e.to_string()))?
            .ok_or_else(|| SchemaError::ClassNotFound(name.to_owned()))?;
        Ok(ClassDef {
            name: cv.name().to_owned(),
            tree_root: cv.def().tree_root.unwrap_or(false),
            is_a: cv.def().is_a.clone(),
            mixins: cv.def().mixins.clone().unwrap_or_default(),
        })
    }
    // … induced_slot maps SlotView -> SlotDef, etc.
}
```

**Pros:**
- Zero Python overhead; purely native Rust.
- `linkml_schemaview` already implements `induced_slot` (MRO resolution, slot_usage merging, inheritance) — no reimplementation.
- Schema parsing is saphyr/YAML-1.2 (faster than Python's libyaml on large schemas per `cache_speedtest.py`).
- Satisfies `Send + Sync`; safe for async/rayon use.

**Cons:**
- Hard dependency on an unpublished, fast-moving crate; no semver guarantees.
- The `schemaview` crate currently depends on `serde_yaml 0.9` (deprecated) and a `reqwest` feature for remote schema resolution. Pull those in transitively.
- `linkml_meta 0.0.0` — the version field is literally `0.0.0`; upstream makes no stability promise.
- Mapping `SchemaView`'s rich error types and optional `Converter` argument into `SchemaProvider`'s simple API requires a thin adapter crate (a few hundred lines).
- The metamodel crate uses `merge = 0.2` and a fork of serde-value; build times increase.

**Recommendation:** This is the **right long-term option** once `p2p-ld/linkml-core` publishes to crates.io or the workspace version is pinned. Use a separate `linkml-map-schemaview` crate for the adapter so `linkml-map-core` stays dependency-free.

---

### Option B — Parse LinkML schema YAML directly with serde

**How:** Deserialise a LinkML schema YAML file into hand-rolled serde structs,
walk `is_a`/`mixins` chains manually to resolve induced slots.

```toml
[dependencies]
serde_yaml_ng = "0.10"
serde = { version = "1", features = ["derive"] }
```

```rust
#[derive(Debug, Deserialize)]
struct LinkmlSchema {
    classes: HashMap<String, ClassRaw>,
    enums: HashMap<String, EnumRaw>,
    types: HashMap<String, TypeRaw>,
    // …
}
```

**Pros:**
- Completely self-contained; no external git/path dependency.
- Fast iteration: no upstream API churn to track.
- Could share `serde_yaml_ng` already used by `linkml-map-io`.

**Cons:**
- Must reimplement MRO resolution (is_a + mixins, slot_usage merging, inlined attributes) — this is non-trivial. The Python `induced_slot` is ~300 lines; the Rust `classview.rs` gather function is another ~120 lines. Getting it right (especially slot_usage precedence) requires conformance tests.
- Imports: LinkML schemas can `import` other schemas via URI or path. Handling transitive imports and cross-schema slot references requires a loader and import cache (another ~200 lines).
- Type inference (what is the `range` of a slot whose range is inherited from a type chain?) is subtle.
- Effectively reinvents `linkml_schemaview`. Any future LinkML metamodel change must be tracked manually.

**Recommendation:** Acceptable for a minimal MVP that only handles schemas with no cross-file imports and explicit `induced_slot` specifications in the transform spec. **Not recommended** for general use — the correctness burden is too high.

---

### Option C — Call Python SchemaView via PyO3

**How:** Link against the Python interpreter; call `SchemaView.induced_slot` etc.
from Rust via `pyo3::Python::with_gil`.

**Pros:**
- Zero reimplementation: reuses battle-tested Python SchemaView exactly.
- No schema parsing or MRO logic to maintain.

**Cons:**
- Requires a Python interpreter at runtime — breaks the "pure Rust binary" use case.
- GIL serialises all schema calls; negates async/rayon parallelism.
- Makes `linkml-map-core` depend on `pyo3`, inflating the dependency graph for every downstream user.
- Impossible to use in WASM targets.
- FFI marshalling overhead (Python dicts → Rust structs) on every slot lookup.

**Recommendation:** Only viable as a transitional shim for the linkml-map-py crate, not for `linkml-map-core`. Explicitly **not recommended** for the core engine.

---

## 4. Recommendation

**Immediate (now):** Keep `SchemaProvider` + `InMemorySchema` in
`linkml-map-core` with no external schema dependency. This is sufficient for:
- Unit tests of the engine (which supply `InMemorySchema` directly).
- Integration tests that pre-build `InMemorySchema` from a parsed YAML file
  in the test harness.

**Short term:** Add a separate `linkml-map-schemaview` crate (NOT in
`linkml-map-core`) that wraps the `linkml_schemaview` crate from
`p2p-ld/linkml-core` via a path/git dependency and implements
`SchemaProvider` for `SchemaView`. This keeps `linkml-map-core`
dependency-free (the stated constraint) while enabling full schema-driven
transforms in the CLI and Python bindings.

**Long term:** When `p2p-ld/linkml-core` publishes stable crates to crates.io
(which would also give the wheel a published source), switch the git/path
dependency to a versioned crates.io dep.

**Avoid Option B** except as a last resort for environments where neither a
Python interpreter nor the upstream source is available — the MRO
reimplementation risk is too high.

---

## 5. Trait surface summary

The `SchemaProvider` trait defined in
`crates/linkml-map-core/src/schema/mod.rs` exposes 8 required methods and 1
default method:

```text
Required:
  get_class(class_name)              → ClassDef
  all_class_names()                  → Vec<String>
  induced_slots(class_name)          → Vec<SlotDef>
  identifier_slot(class_name)        → Option<SlotDef>
  induced_slot(slot_name, class_name)→ SlotDef
  get_enum(enum_name)                → EnumDef
  all_enum_names()                   → Vec<String>
  all_type_names()                   → Vec<String>

Default (provided):
  tree_root_class()                  → Option<String>
    (scans all_class_names() for the first tree_root=true class)
```

Data structs carried: `ClassDef`, `SlotDef` (with `RangeKind` enum covering
Class/Type/Enum/None and helper methods `range_class()`, `range_enum()`,
`range_type()`, `is_object_range()`), `EnumDef`, `PermissibleValue`.

All 23 unit tests pass (`cargo test -p linkml-map-core`).

---

## Sources

- [GitHub — p2p-ld/linkml-core](https://github.com/p2p-ld/linkml-core)
- [linkml-runtime releases (Python)](https://github.com/linkml/linkml-runtime/releases)
- [SchemaView — linkml documentation](https://linkml.io/linkml/developers/schemaview.html)
- [Porting LinkML tools to other languages](https://linkml.io/linkml/howtos/port-linkml.html)
- Local source: `C:\Users\abora\LinkML-MCP\rust\src\schemaview\src\` (schemaview crate)
- Local source: `C:\Users\abora\LinkML-MCP\rust\src\metamodel\Cargo.toml` (linkml_meta crate)
- Local wheel: `C:\Users\abora\LinkML-MCP\vendor\linkml_runtime_rust-0.2.1-cp312-cp312-win_amd64.whl`
