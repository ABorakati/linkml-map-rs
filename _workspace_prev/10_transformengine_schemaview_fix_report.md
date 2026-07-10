# transform-engine report — schemaview `default_curi_maps` / builtin-import prefix resolution

**Task:** task_f8b31863 (flagged by pyo3-binding-engineer). Fix the pre-existing
`CurieError(NotFound("schema"))` schema-loading limitation in `linkml-map-schemaview` that
blocked the PyO3 binding's flattening golden cases, and remove the duplicated tolerant-loader
workarounds it forced into pipeline and conformance.

**Outcome:** Option A landed (root-cause fix in `linkml-map-schemaview`). Both duplicated
in-memory-YAML fallbacks (pipeline + conformance) deleted. No public API signature changed;
harness gate clean.

---

## Root cause

The vendored `schemaview` crate (`Kapernikov/rust-linkml-core`, rev `df9d18e`) builds its CURIE
converter in `converter_from_schemas` (`src/schemaview/src/identifier.rs:199`). That function:

1. Registers only the prefixes declared **inline** under `schema.prefixes`.
2. Adds a hardcoded fallback trio: `rdfs`, `rdf`, `dcterms` (`identifier.rs:224-230`).
3. **Ignores `default_curi_maps` entirely** — the field is parsed into
   `SchemaDefinition.default_curi_maps` but never read by the converter.
4. **Does not fetch or merge the prefixes of builtin `linkml:` imports** (`linkml:types` etc.);
   it silently skips unresolvable imports.

`add_schema` (`schemaview.rs:759`) then indexes every class/slot, calling `Identifier::to_uri`
on each `slot_uri`. The first unresolvable CURIE prefix aborts the whole load.

### Why `tests/golden/flattening/source/normalized.yaml` failed but `personinfo` didn't

- `normalized.yaml` declares only `mappings:` and `linkml:` under `prefixes:`, relying on
  `default_curi_maps: [semweb_context]` + `imports: [linkml:types]` to make `schema:` and
  `rdfs:` resolvable. Its slots use `slot_uri: schema:identifier` and `slot_uri: rdfs:label`.
  `rdfs:` happened to survive (hardcoded trio), but `schema:` had no source → `add_schema`
  failed with exactly `CurieError(NotFound("schema"))` (reproduced directly against the
  fixture via the strict loader before the fix).
- `personinfo.yaml` **explicitly declares** `schema:` and `rdfs:` under `prefixes:`, so it
  never depends on `default_curi_maps` resolution — it papered over the gap by listing the
  prefixes inline. That is why the conformance/personinfo path never surfaced the bug.

### What real Python does (the parity target)

`linkml_runtime.SchemaView` resolves prefixes via `SchemaLoader.load()`
(`linkml/utils/schemaloader.py:130-137`):

```python
for prefix in self.schema.prefixes.values():
    self.namespaces[prefix.prefix_prefix] = prefix.prefix_reference
for cmap in self.schema.default_curi_maps:
    self.namespaces.add_prefixmap(cmap, include_defaults=False)
```

- `Namespaces.add_prefixmap` (`linkml_runtime/utils/namespaces.py:277-305`) adds each prefix
  **only if not already defined** (`elif k not in self:`), filtered to valid NCNames. So
  explicit `prefixes:` always win; the curie map fills gaps.
- `semweb_context` ∈ `BIOCONTEXT_CONTEXTS`, so it is read by `curie_util.read_biocontext`
  from `prefixcommons/registry/semweb_context.jsonld` (fetched and transcribed verbatim into
  the fix). That file contains `rdf`/`rdfs`/`owl`/`xsd`/`prov`/`dcterms`/… but **not**
  `schema`.
- `schema:` actually comes from the **`linkml:types` import**: `linkml_runtime`'s
  `linkml_model/model/schema/types.yaml` `prefixes:` block declares
  `linkml`/`xsd`/`shex`/`schema`, and Python's converter is built from *all* loaded schemas
  (main + every import), so imported prefixes merge in (again without clobbering the
  importer's). The Rust backend can't fetch that import, so `schema:` was simply missing.

Net: the correct fix must reproduce **two** prefix sources the vendored crate drops —
`default_curi_maps` maps *and* builtin `linkml:` import prefixes — with Python's
"explicit-wins, never override" precedence.

---

## The fix (Option A) — `crates/linkml-map-schemaview/src/lib.rs`

Added `resolve_default_curi_maps(&mut SchemaDefinition)`, called from
`load_schema_definition_from_str` (the single choke point both the path and the string
loaders — and therefore all four public entry points — flow through), **before** any patch
and before `add_schema`. It:

- Collects prefix contributions from `default_curi_maps` (via `builtin_curi_map`) and from
  builtin `linkml:` imports (via `builtin_import_prefixes`).
- Merges them into `schema.prefixes` with `Entry::or_insert_with`, so an already-present
  prefix (explicit, or an earlier contributor) is never overwritten.
  A `// PARITY:` comment cites `Namespaces.add_prefixmap`'s `k not in self` rule.

Two verbatim static tables back it:

- `SEMWEB_CONTEXT` — the 14 entries of `prefixcommons/registry/semweb_context.jsonld`
  (`dc`, `dcat`, `dcterms`, `faldo`, `foaf`, `idot`, `oa`, `owl`, `prov`, `rdf`, `rdfs`,
  `void`, `xsd`, `oboInOwl`).
- `LINKML_TYPES_PREFIXES` — the `prefixes:` of `linkml_model` `types.yaml`
  (`linkml`, `xsd`, `shex`, `schema`) — the only builtin import the repo's fixtures use.

Only maps/imports we bundle are expanded; anything unknown is left for the backend exactly as
before (non-fatal). This is deliberately the *prefix side* only — we do **not** load import
content, because that is all `add_schema` needs and the backend already tolerates unresolved
builtin imports for everything else.

Added two crate unit tests: `default_curi_maps_and_builtin_import_resolve_prefixes` (a schema
that omits `schema:`/`xsd:` inline now loads) and `explicit_prefix_wins_over_curi_map` (guards
the non-override precedence).

**This is a pure schema-loading behaviour change with no signature change** — the public
`SchemaViewProvider::{load_from_path, load_from_path_with_patch, from_yaml_str,
from_yaml_str_with_patch}` surface is byte-for-byte identical; they just now accept fixtures
that previously errored.

---

## What I removed / simplified (dead-code cleanup, Option A side effect)

With the strict loader now resolving these prefixes, both hand-rolled tolerant fallbacks
became dead and were deleted:

### `crates/linkml-map-pipeline/src/lib.rs` (−208 net)
- Deleted the `TolerantSchema` enum, `load_schema_tolerant`, and the private
  `build_inmemory_schema_from_yaml` (~145-line minimal YAML parser).
- Replaced with a thin `load_schema(path, patch) -> Result<SchemaViewProvider>` that calls
  `SchemaViewProvider::load_from_path_with_patch` directly.
- Simplified the two call sites in `CompiledPlan::load` to box the `SchemaViewProvider`
  directly (no more `match ts { Real | InMemory }`).

### `crates/linkml-map-conformance/src/lib.rs` (−~170 net)
- Deleted the `SchemaSource` enum (`Real`/`InMemory` + `as_provider`) and the second copy of
  `build_inmemory_schema_from_yaml`.
- `load_source_schema` now returns `SchemaViewProvider` and loads every candidate through the
  strict backend. **The "most-classes" candidate scan is preserved** — it solves an
  orthogonal problem (fixtures like `flattening/` ship a truncated 0-class
  `cmdr_normalized.yaml` beside the real `normalized.yaml`) and is deliberately mirrored on
  the Python side by `_best_source_schema` in `test_compat_api.py`. The biolink-metamodel
  "documented SKIP" still works: a candidate that legitimately fails to load yields `Err`,
  which `run_case_inner` turns into a SKIP exactly as before.
- Trimmed the now-unused imports (`ClassDef`, `InMemorySchema`, `InMemorySchemaBuilder`,
  `RangeKind`, `SlotDef`) down to `SchemaProvider`.

I did **not** touch `crates/linkml-map-py` (owned by pyo3-binding-engineer — see handoff
below), nor redesign any pipeline/conformance internals beyond the mechanical call-site swaps.

---

## Verification (real output)

**1. Build + clippy (`linkml-map-schemaview`) clean:**
```
$ cargo build -p linkml-map-schemaview
compiled 2 crates (14.22s)

$ cargo clippy -p linkml-map-schemaview --all-targets
warning: `linkml-map-core` (lib) generated 7 warnings   # pre-existing, in a dependency
    Finished `dev` profile [...] in 0.42s
# schemaview crate itself: 0 warnings (verified — no `schemaview/src` lines in clippy output)
```

**2. `cargo test --workspace --exclude linkml-map-py`:**
```
TOTAL PASS=241 FAIL=0
```
(includes schemaview 22 passed, pipeline suite, conformance 4 passed.)

**3. Conformance / compliance counts — no regression:**
```
$ cargo test -p linkml-map-conformance
test compliance::tests::compliance_suite ... ok
test tests::conformance_suite ... ok
test result: ok. 4 passed; 0 failed; ...

tests/CONFORMANCE_REPORT.md  -> **Total**: 14  **PASS**: 14  **FAIL**: 0  **SKIP**: 0
tests/COMPLIANCE_REPORT.md   -> **Total**: 55  **PASS**: 55  **FAIL**: 0  **SKIP**: 0
| `golden/flattening/MappingSet-001` | **PASS** |

$ git status --short tests/CONFORMANCE_REPORT.md tests/COMPLIANCE_REPORT.md
   (empty)   # regenerated reports are byte-identical to the committed baseline
```
Both suites regenerate the report tables on every run; the empty git status proves byte
parity was preserved (14/14 and 55/55, unchanged).

**4. Rust-level proof the PyO3-visible symptom is fixed (no `linkml-map-py` edit):**
Before the fix, a throwaway test loading the real fixture failed with
`LOAD FAILED: SchemaView::add_schema failed: CurieError(NotFound("schema"))`. After the fix,
`SchemaViewProvider::load_from_path("tests/golden/flattening/source/normalized.yaml")` returns
`OK classes=["Entity", "MappingSet", "Mapping"]`. This is now permanently covered by the
crate unit test `default_curi_maps_and_builtin_import_resolve_prefixes`, and — decisively —
by the two flattening golden cases (`golden/flattening/MappingSet-001`,
`examples/flattening/MappingSet-001`) passing 14/14 **after the conformance in-memory fallback
was deleted**, i.e. they can now only have loaded through the strict `SchemaViewProvider`.

**5. Harness gate:**
```
$ cargo xtask harness-gate
EXIT=0
```
Clean — no three-surface sync violation. The fix adds no new public symbol that
pipeline/conformance couldn't already reach (they call the same unchanged
`load_from_path*` entry points), and changes no signature, so there is nothing for the
Python `.pyi`/docs surface to mirror.

---

## Handoff to pyo3-binding-engineer (task_f8b31863)

**No new or different entrypoint is needed. `linkml-map-py` should keep calling the strict
loader it already calls — it just works now.**

- The public `SchemaViewProvider` API is unchanged (same method names/signatures); the binding
  needs **zero code change** to consume the fix — the fixture that used to raise
  `CurieError(NotFound("schema"))` now loads successfully through the exact call the binding
  already makes.
- The 3 previously-failing compat tests
  (`test_golden_native_transformer_matches_expected[golden/flattening/MappingSet-001]` and its
  2 shim-variant siblings) should now pass with no binding-layer patch. Please re-run
  `crates/linkml-map-py/tests/test_compat_api.py` in the clean compat venv to confirm — I did
  not run the Python pytest suite (out of my crate's scope).
- Do **not** add any YAML-fallback shim to `linkml-map-py`; the whole point of Option A is that
  no consumer needs one anymore.

## Suggested upstream issues (not auto-filed)

Two behaviours in the vendored `Kapernikov/rust-linkml-core` @ `df9d18e` diverge from
`linkml_runtime` and are worth reporting upstream (we work around them here rather than
"fix" them, per the parity doctrine):

1. `converter_from_schemas` (`identifier.rs`) ignores `SchemaDefinition.default_curi_maps`
   entirely, so schemas relying on `default_curi_maps: [semweb_context]` fail to resolve
   builtin prefixes — whereas `linkml_runtime.SchemaView` resolves them via
   `Namespaces.add_prefixmap`.
2. Builtin `linkml:` imports (`linkml:types`, …) contribute no prefixes to the converter,
   so `schema:`/`xsd:` from `types.yaml` are unresolvable unless re-declared inline —
   whereas Python builds the converter from all loaded (imported) schemas.
