# Upstream sync review — 2026-08-21 (Slice C: engine internals — caching and dead-code deletion)

Point-in-time snapshot. This review covers two upstream commits in the engine internals category:
1. `429379ce71846a2042d3ddda24a035939b40af8b` — "Cache ReferenceValidator object in Transformer._shape_normalize" (2026-08-21)
2. `abeeb1b1aa8fd2c3d0b75e2804134047dc8b340a` — "Delete unreferenced modules and symbols" (2026-08-03, #297)

## Commit-by-commit

| Commit | Date | Summary | Status |
|---|---|---|---|
| `429379ce71` | 2026-08-21 | Cache `ReferenceValidator` instance in `Transformer._shape_normalize` via `@lru_cache(maxsize=1)` helper `_shape_normalizer()` to avoid re-materializing the metamodel `SchemaView` on every spec load | **N/A to Rust.** Purely a Python-side performance optimization with zero observable behavioral drift. Rust uses static serde deserializers (`crates/linkml-map-core/src/datamodel/mod.rs`) and compiles expression ASTs once per spec (`crates/linkml-map-core/src/engine/mod.rs:77`), so no dynamic metamodel schema reflection or per-call validator construction exists. |
| `abeeb1b1aa` | 2026-08-03 | Delete unreferenced dead-code modules and symbols: unported compiler stubs (`r2rml_compiler.py`, `sparql_compiler.py`, `sssom_compiler.py`, `sssom.map.yaml`), unported importer module (`importer/`), unused exception (`QuantitySyntaxError`), unreferenced `SchemaMapper.copy_attributes`, and redundant experimental datamodel (`transformer_model_new.py`) | **Already matching.** None of the deleted symbols or modules were ever ported to Rust. The Rust port never exposed these symbols on its public Rust, PyO3, or Python shim surfaces. |

---

## 1. Commit `429379ce71846a2042d3ddda24a035939b40af8b` (2026-08-21)

**Upstream title:** `Cache ReferenceValidator object in Transformer._shape_normalize`

### Upstream change analysis

In `src/linkml_map/transformer/transformer.py`, upstream added a module-level cached helper:

```python
@lru_cache(maxsize=1)
def _shape_normalizer() -> ReferenceValidator:
    """Return the shared ReferenceValidator used by ``_shape_normalize``.

    Constructing a ReferenceValidator materializes the derived transformer_model
    schema. That process is expensive, so instead, a single instance is built
    lazily and reused. The schema is a packaged constant and the validator does
    not hold any per-call state, so caching is safe.
    """
    normalizer = ReferenceValidator(package_schemaview("linkml_map.datamodel.transformer_model"))
    normalizer.expand_all = True
    return normalizer
```

In `Transformer._shape_normalize(cls, obj: dict[str, Any])`, the per-call construction `ReferenceValidator(package_schemaview(...))` was replaced with `_shape_normalizer()`.

### Performance vs. observable behavior evaluation

1. **Statefulness check:** `ReferenceValidator.normalize()` in `linkml-runtime` performs a structural normalization on raw dictionaries according to the schema definitions (expanding list vs dict representations, injecting element names). It does not record or retain validation error state across invocations.
2. **Schema mutability check:** The underlying `SchemaView` wraps the static packaged `transformer_model` schema, which is constant and not mutated at runtime.
3. **Verdict:** This change is **purely a performance optimization** for spec loading in Python. A cached `ReferenceValidator` carries no state across rows, batches, or transformer instances. There is zero change in observable transformation output or error behavior.

### Rust counterpart investigation

1. **Spec deserialization and shape normalization:**
   - In Python, `_shape_normalize` canonicalizes loose YAML dictionary structures (such as dict-keyed vs list-keyed derivations) using LinkML's dynamic `ReferenceValidator`.
   - In Rust (`crates/linkml-map-core/src/datamodel/mod.rs`), shape normalization is handled statically by Serde during typed deserialization into `TransformationSpecification`. Custom deserializers handle polymorphic input formats without dynamic schema reflection:
     - `deserialize_stringish_option` (`crates/linkml-map-core/src/datamodel/mod.rs:10`): coerces strings, booleans, and numbers to `Option<String>`.
     - `deserialize_string_or_seq_option` (`crates/linkml-map-core/src/datamodel/mod.rs:28`): handles scalar vs list `populated_from` on permissible-value derivations.
     - `deserialize_schema_reference_option` (`crates/linkml-map-core/src/datamodel/mod.rs:68`): handles bare-string vs mapping `source_schema`/`target_schema`.
2. **Validator instantiation lifecycle:**
   - Semantic validation in Rust lives in `crates/linkml-map-core/src/validate/mod.rs` (`validate_spec_semantics`). It is an opt-in pre-flight function taking `&TransformationSpecification` and `&dyn SchemaProvider` (`crates/linkml-map-core/src/validate/mod.rs:1-45`).
   - The transformation engine (`crates/linkml-map-core/src/engine/mod.rs`) takes an immutable borrow or Cow of `TransformationSpecification`. Expression ASTs are parsed once via `CompiledExprs::build(spec)` (`crates/linkml-map-core/src/engine/mod.rs:77`).
   - In the PyO3 layer (`crates/linkml-map-py/src/lib.rs`), `PyTransformer` parses the spec once during construction, performs join synthesis once via `derive_spec` (`crates/linkml-map-py/src/lib.rs:235-245`), builds `CompiledExprs` once, and reuses them across all `.transform()` / `.transform_many()` calls.
   - Rust has no counterpart to `ReferenceValidator` and does not construct any validator per call.

### Final status: **N/A to Rust**

---

## 2. Commit `abeeb1b1aa8fd2c3d0b75e2804134047dc8b340a` (2026-08-03)

**Upstream title:** `Delete unreferenced modules and symbols` (Closes #297)

### Deleted symbols and modules breakdown

The upstream commit removed dead, unreferenced, or placeholder code across compilers, datamodels, importers, and inference helpers.

| Upstream symbol / path | Upstream kind | Rust counterpart | Still called here? | Action | Classification |
|---|---|---|---|---|---|
| `mkdocs.yml` (nav entry for importer) | Docs configuration comment | None | No | None | (a) Never ported |
| `linkml_map.compiler.r2rml_compiler.R2RMLCompiler` | Class (stub raising `NotImplementedError`) | None | No | None | (a) Never ported |
| `linkml_map.compiler.sparql_compiler.SPARQLCompiler` | Class (stub raising `NotImplementedError`) | None | No | None | (a) Never ported |
| `linkml_map.compiler.sssom_compiler.SSSOMCompiler` | Class (stub raising `NotImplementedError`) | None | No | None | (a) Never ported |
| `src/linkml_map/datamodel/sssom.map.yaml` | Transformation spec fixture/stub | None | No | None | (a) Never ported |
| `src/linkml_map/datamodel/transformer_model_new.py` (all symbols, 649 lines) | Redundant experimental Pydantic v2 datamodel file | `crates/linkml-map-core/src/datamodel/mod.rs` (mirrors canonical `transformer_model.py`) | No | None | (a) Never ported |
| `linkml_map.functions.unit_conversion.QuantitySyntaxError` | Exception class (unused upstream) | None (`crates/linkml-map-core/src/engine/units.rs` defines `ConvError::Undefined` and `ConvError::Dimensionality`) | No | None | (a) Never ported |
| `linkml_map.importer.importer.Importer` | Abstract base class (`import_specification`) | None | No | None | (a) Never ported |
| `src/linkml_map/importer/__init__.py` | Module | None | No | None | (a) Never ported |
| `linkml_map.inference.schema_mapper.SchemaMapper.copy_attributes` | Method (unreferenced in Python; operated on `linkml_runtime` `Element` ASTs) | None (`crates/linkml-map-core/src/inference/schema_mapper.rs` uses `apply_directives` on `serde_json::Map`) | No | None | (a) Never ported |

### Public API / three-surface impact

Under `harness-ownership.toml` and the three-surface API synchronization doctrine:
- **Rust API:** `crates/linkml-map-core/src/lib.rs`, `crates/linkml-map-core/src/datamodel/mod.rs`, `crates/linkml-map-core/src/inference/mod.rs`, `crates/linkml-map-core/src/engine/mod.rs`. None of these ever exported R2RML/SPARQL/SSSOM compilers, Importer traits, `QuantitySyntaxError`, or `copy_attributes`.
- **PyO3 Binding:** `crates/linkml-map-py/src/lib.rs` exports `Transformer`, `ValidationMessage`, `transform_object`, `transform_objects`, and `validate_spec`. None of the deleted upstream symbols were ever bound or exposed.
- **Python-facing stubs/shims:**
  - `crates/linkml-map-py/python/linkml_map/__init__.py`: exports `ObjectTransformer`.
  - `crates/linkml-map-py/python/linkml_map_rs/__init__.py`: exports `Transformer`, `ValidationMessage`, `transform_object`, `transform_objects`, `validate_spec`.
  - `crates/linkml-map-py/python/linkml_map_rs/_native.pyi`: type stubs for the 5 native symbols.
  - None of the deleted upstream symbols were present in `__all__` or `_native.pyi`.

### Final status: **Already matching**

---

## Verification and notes

- **Verified via Codedb and file inspection:**
  - `crates/linkml-map-core/src/datamodel/mod.rs`: verified static Serde shape normalization and absence of `transformer_model_new` constructs.
  - `crates/linkml-map-core/src/validate/mod.rs`: verified typed `validate_spec_semantics` architecture.
  - `crates/linkml-map-core/src/engine/units.rs`: verified unit conversion error enum definitions (`Undefined`, `Dimensionality`) and absence of syntax error exceptions.
  - `crates/linkml-map-core/src/inference/schema_mapper.rs`: verified that `SchemaMapper` uses `apply_directives` on `serde_json::Map` and never defined `copy_attributes`.
  - `crates/linkml-map-py/src/lib.rs` & `crates/linkml-map-py/python/`: verified PyO3 exports, `.pyi` stubs, and `linkml_map` shim compatibility.
- **Limitations:** No `cargo build` or `cargo test` runs were executed in this session to prevent lock contention with concurrent agents. Verification was performed via structural and static inspection.
