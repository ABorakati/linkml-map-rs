//! PyO3 bindings for the linkml-map Rust transform engine.
//!
//! # Python API
//!
//! ```python
//! from linkml_map_rs import Transformer
//!
//! t = Transformer(
//!     source_schema="source.yaml",
//!     spec="transform.yaml",
//!     target_schema="target.yaml",   # optional
//!     source_class="Person",         # optional; inferred from tree_root if omitted
//! )
//! result = t.transform({"id": "P:1", "height": {"value": 172.0, "unit": "cm"}})
//! results = t.transform_many([obj1, obj2])
//!
//! # Cross-table joins: register the joined table's rows before transforming
//! # (mirrors upstream Transformer.lookup_index, but rows are supplied
//! # in-memory rather than loaded from a file).
//! t.register_join_table("demographics", [{"patient_id": "P:1", "age": 42}], "patient_id")
//! result = t.transform({"pid": "P:1"})   # spec's `joins:`/`{alias.field}` now resolves
//!
//! # Convenience free functions (schema/spec loaded on every call — use the
//! # class when transforming many objects from the same spec).
//! result = transform_object(obj, source_schema="source.yaml", spec="transform.yaml")
//! results = transform_objects(objs, source_schema="source.yaml", spec="transform.yaml")
//!
//! # Pre-flight validation: cross-reference a spec against its schema(s)
//! # WITHOUT running any transform (no implicit-join synthesis either — it
//! # checks the spec exactly as parsed, so it catches problems before that
//! # stage runs). Returns [] if neither schema is given.
//! from linkml_map_rs import validate_spec
//! messages = validate_spec(
//!     "transform.yaml",
//!     source_schema="source.yaml",
//!     target_schema="target.yaml",   # optional
//!     strict=False,                  # optional; True escalates expr warnings to errors
//! )
//! for msg in messages:
//!     print(msg.severity, msg.path, msg.message)   # or just: print(msg)
//! ```
//!
//! # dict <-> Value bridging
//!
//! Python dicts are converted via JSON round-trip:
//!   Python object  ---(Python json.dumps)--->  JSON string
//!   JSON string    ---(serde_json::from_str)-->  serde_json::Value
//!   serde_json::Value  ---(serde_json::from_value)-->  linkml_map_core::value::Value
//!
//! The reverse path uses serde_json serialisation + Python json.loads.
//! This approach avoids `pythonize` (not a dep) and the fragile manual
//! PyDict traversal, and is correct for all Value variants including nested
//! maps and lists.
//!
//! `json.dumps` is always called with `default=str`, both for a source
//! *object* (`py_to_value`) and for a dict/`TransformationSpecification`
//! passed as `specification=` (`spec_to_json_value`) — the `linkml_map`
//! shim's `create_transformer_specification(spec_dict)` path commonly carries
//! a `yaml.safe_load`-parsed `datetime.date`/`datetime.datetime` (e.g. a bare
//! `publication_date: 2025-08-14` spec metadata field), which `json.dumps`
//! otherwise rejects with `TypeError`. Note the resulting `Value` is always a
//! plain string on both bridge directions — the core `Value` enum
//! (`linkml_map_core::value::Value`) has no dedicated date/datetime variant,
//! so a value returned from `Transformer.transform`/`.map_object` that
//! originated from a date is a Python `str`, not a `datetime.date` — a
//! permanent (and correct, LinkML-stringification-matching) asymmetry with
//! upstream Python's in-memory `ObjectTransformer`, which never round-trips
//! through JSON and so preserves native `date`/`datetime` objects untouched.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use linkml_map_core::{
    datamodel::TransformationSpecification,
    engine::{LookupIndex, ObjectTransformer},
    validate::{validate_spec_semantics, ValidationMessage},
    value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

// ── Helpers: Python object <-> Value via JSON ─────────────────────────────────

/// Convert a Python object (dict, list, scalar) to a Rust `Value` by going
/// through JSON.  The caller must hold the GIL (`py`).
fn py_to_value(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    use pyo3::types::PyDict;
    let json_mod = py.import("json")?;
    // `default=str` so non-JSON scalars (datetime.date / Decimal / etc., as
    // produced by linkml_runtime loaders) serialise to their string form rather
    // than raising — matching how LinkML treats date/datetime ranges.
    let kwargs = PyDict::new(py);
    kwargs.set_item("default", py.import("builtins")?.getattr("str")?)?;
    let json_str: String = json_mod
        .call_method("dumps", (obj,), Some(&kwargs))?
        .extract()?;

    let serde_val: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| PyValueError::new_err(format!("JSON parse error: {e}")))?;

    serde_json::from_value(serde_val)
        .map_err(|e| PyValueError::new_err(format!("Value conversion error: {e}")))
}

/// Convert a Rust `Value` back to a Python object (dict/list/scalar) via JSON.
fn value_to_py(py: Python<'_>, val: &Value) -> PyResult<PyObject> {
    let serde_val = serde_json::to_value(val)
        .map_err(|e| PyValueError::new_err(format!("Value serialisation error: {e}")))?;
    let json_str = serde_json::to_string(&serde_val)
        .map_err(|e| PyValueError::new_err(format!("JSON serialisation error: {e}")))?;

    let json_mod = py.import("json")?;
    let py_obj: PyObject = json_mod.call_method1("loads", (json_str,))?.extract()?;

    Ok(py_obj)
}

/// Coerce a Python SchemaView (or string/path) argument to a JSON string.
fn schema_to_json_str(py: Python<'_>, sv: &Bound<'_, PyAny>) -> PyResult<String> {
    use pyo3::types::PyString;
    if sv.is_instance_of::<PyString>() {
        let s: String = sv.extract()?;
        let path = std::path::Path::new(&s);
        if path.exists() && path.is_file() {
            let content = std::fs::read_to_string(path)
                .map_err(|e| PyValueError::new_err(format!("Cannot read schema file '{s}': {e}")))?;
            return Ok(content);
        }
        return Ok(s);
    }

    let schema = if sv.hasattr("schema")? {
        sv.getattr("schema")?
    } else {
        sv.clone()
    };

    let json_dumper = py.import("linkml_runtime.dumpers.json_dumper")?;
    let json_str: String = json_dumper.call_method1("dumps", (schema,))?.extract()?;
    Ok(json_str)
}

/// Coerce a Python specification (dict, dataclass, string/path) argument to a `serde_json::Value`.
fn spec_to_json_value(py: Python<'_>, spec: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    use pyo3::types::{PyDict, PyString};

    if spec.is_instance_of::<PyString>() {
        let s: String = spec.extract()?;
        let path = std::path::Path::new(&s);
        let content = if path.exists() && path.is_file() {
            std::fs::read_to_string(path)
                .map_err(|e| PyValueError::new_err(format!("Cannot read spec file '{s}': {e}")))?
        } else {
            s
        };
        let val: serde_json::Value = serde_yaml_ng::from_str(&content)
            .map_err(|e| PyValueError::new_err(format!("Failed to parse spec: {e}")))?;
        return Ok(val);
    }

    if spec.is_instance_of::<PyDict>() {
        // `default=str` mirrors `py_to_value`: a dict spec loaded via
        // `yaml.safe_load` can carry non-JSON scalars (e.g. a bare
        // `publication_date: 2025-08-14` becomes `datetime.date`), which
        // `json.dumps` otherwise rejects with `TypeError`.
        let json_mod = py.import("json")?;
        let kwargs = PyDict::new(py);
        kwargs.set_item("default", py.import("builtins")?.getattr("str")?)?;
        let json_str: String = json_mod
            .call_method("dumps", (spec,), Some(&kwargs))?
            .extract()?;
        let val: serde_json::Value = serde_json::from_str(&json_str)
            .map_err(|e| PyValueError::new_err(format!("JSON parse error: {e}")))?;
        return Ok(val);
    }

    // Otherwise assume it is a dataclass object.
    let json_dumper = py.import("linkml_runtime.dumpers.json_dumper")?;
    let json_str: String = json_dumper.call_method1("dumps", (spec,))?.extract()?;
    let val: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| PyValueError::new_err(format!("JSON parse error: {e}")))?;
    Ok(val)
}

// ── Schema / spec loading ─────────────────────────────────────────────────────

fn load_spec(spec_path: &str) -> PyResult<TransformationSpecification> {
    let yaml_str = std::fs::read_to_string(spec_path)
        .map_err(|e| PyValueError::new_err(format!("Cannot read spec file '{spec_path}': {e}")))?;
    parse_spec_yaml(&yaml_str)
}

/// Parse a transform spec from a YAML string (the in-memory entry point used by
/// the `linkml_map`-compatible shim, which dumps spec objects to YAML).
fn parse_spec_yaml(yaml_str: &str) -> PyResult<TransformationSpecification> {
    let mut obj: serde_json::Value = serde_yaml_ng::from_str(yaml_str)
        .map_err(|e| PyValueError::new_err(format!("Failed to parse YAML spec: {e}")))?;
    linkml_map_core::datamodel::normalise_spec_json(&mut obj);
    serde_json::from_value(obj)
        .map_err(|e| PyValueError::new_err(format!("Failed to parse spec: {e}")))
}

fn load_schema_provider(schema_path: &str) -> PyResult<SchemaViewProvider> {
    let path = std::path::Path::new(schema_path);
    SchemaViewProvider::load_from_path(path)
        .map_err(|e| PyValueError::new_err(format!("Failed to load schema '{schema_path}': {e}")))
}

/// Load the SOURCE schema, applying the spec's `source_schema_patches` (if any)
/// before indexing. Patches only ever augment the source schema (e.g. adding FK
/// `range`s missing from auto-generated schemas); the target schema is untouched.
fn load_source_provider(
    schema_path: &str,
    spec: &TransformationSpecification,
) -> PyResult<SchemaViewProvider> {
    let path = std::path::Path::new(schema_path);
    SchemaViewProvider::load_from_path_with_patch(path, spec.source_schema_patches.as_ref())
        .map_err(|e| {
            PyValueError::new_err(format!("Failed to load source schema '{schema_path}': {e}"))
        })
}

/// Resolve implicit cross-table joins on a freshly loaded spec, once the source
/// schema is available. Mirrors upstream `Transformer.derived_specification`:
/// implicit `{Table.col}` references are rewritten into explicit joins before
/// any object is transformed (synthesis walks the source schema's classes, so
/// it cannot run at raw spec-parse time). Called once at each build-once entry
/// — the reusable `Transformer` constructors and the one-shot free functions —
/// so `.transform` / `.transform_many` reuse the derived spec instead of paying
/// synthesis per object. Raises `ValueError` on an un-keyable / un-hostable
/// reference, matching the `ValueError` Python's `_synthesize_implicit_joins`
/// raises.
fn derive_spec(
    mut spec: TransformationSpecification,
    source: &SchemaViewProvider,
) -> PyResult<TransformationSpecification> {
    linkml_map_core::normalize::synthesize_implicit_joins(
        &mut spec,
        source as &dyn linkml_map_core::schema::SchemaProvider,
    )
    .map_err(|e| PyValueError::new_err(e.to_string()))?;
    Ok(spec)
}

// ── Core transform logic ──────────────────────────────────────────────────────

/// Inner implementation shared by `Transformer.transform` and the free function.
///
/// `source_class` is `None` → engine infers from `tree_root` or first derivation.
/// `lookup_index` is `None` unless the caller registered joined-table rows via
/// `Transformer.register_join_table` — see that method for the resolution path.
fn run_transform(
    py: Python<'_>,
    obj: &Bound<'_, PyAny>,
    spec: &TransformationSpecification,
    source_provider: &SchemaViewProvider,
    target_provider: Option<&SchemaViewProvider>,
    source_class: Option<&str>,
    lookup_index: Option<&Arc<LookupIndex>>,
) -> PyResult<PyObject> {
    let input_value = py_to_value(py, obj)?;

    let mut engine = ObjectTransformer::new(
        spec.clone(),
        Some(source_provider as &dyn linkml_map_core::schema::SchemaProvider),
        target_provider.map(|p| p as &dyn linkml_map_core::schema::SchemaProvider),
    );
    if let Some(li) = lookup_index {
        engine = engine.with_lookup_index(Arc::clone(li));
    }

    // `map_container` builds a foreign-key ObjectIndex from the whole input
    // first, then maps — handling cross-row FK joins. For non-FK input the index
    // is empty and this is identical to `map_object`.
    let result = engine
        .map_container(&input_value, source_class)
        .map_err(|e| PyValueError::new_err(format!("Transform error: {e}")))?;

    value_to_py(py, &result)
}

// ── PyO3 class: Transformer ───────────────────────────────────────────────────

/// A reusable transformer that loads schema(s) and spec once in `__init__`,
/// then exposes `.transform(obj)` and `.transform_many(objs)`.
///
/// Parameters
/// ----------
/// source_schema : str
///     Path to the source LinkML schema YAML.
/// spec : str
///     Path to the transformation specification YAML.
/// target_schema : str or None, optional
///     Path to the target LinkML schema YAML.  When omitted, the source
///     schema is used for both source and target lookups.
/// source_class : str or None, optional
///     Name of the source class.  When omitted the engine infers it from
///     the `tree_root: true` class in the source schema or from the first
///     class derivation in the spec.
/// `unsendable`: holds a `SchemaViewProvider` (the wrapped `SchemaView` is not
/// `Send`). Pinned to the creating thread, which is the normal single-threaded
/// Python usage.
#[pyclass(name = "Transformer", unsendable)]
struct PyTransformer {
    spec: TransformationSpecification,
    /// Loaded ONCE at construction — not reloaded per `transform` call.
    source_provider: SchemaViewProvider,
    target_provider: Option<SchemaViewProvider>,
    source_class: Option<String>,
    /// Cross-table join data, populated via `register_join_table`. `None`
    /// until the first registration; attached to the engine on every
    /// `transform` / `transform_many` / `map_object` call once present.
    lookup_index: Option<Arc<LookupIndex>>,
}

#[pymethods]
impl PyTransformer {
    #[new]
    #[pyo3(signature = (source_schema, spec, target_schema=None, source_class=None))]
    fn new(
        source_schema: String,
        spec: String,
        target_schema: Option<String>,
        source_class: Option<String>,
    ) -> PyResult<Self> {
        let spec_parsed = load_spec(&spec)?;
        let source_provider = load_source_provider(&source_schema, &spec_parsed)?;
        let spec_parsed = derive_spec(spec_parsed, &source_provider)?;
        let target_provider = target_schema
            .as_deref()
            .map(load_schema_provider)
            .transpose()?;
        Ok(Self {
            spec: spec_parsed,
            source_provider,
            target_provider,
            source_class,
            lookup_index: None,
        })
    }

    /// Build from in-memory YAML strings (schema + spec), instead of file paths.
    /// Used by the `linkml_map`-compatible shim, which dumps SchemaView / spec
    /// objects to YAML. Schemas/spec are parsed once here.
    #[staticmethod]
    #[pyo3(signature = (source_schema_yaml, spec_yaml, target_schema_yaml=None, source_class=None))]
    fn from_yaml(
        source_schema_yaml: String,
        spec_yaml: String,
        target_schema_yaml: Option<String>,
        source_class: Option<String>,
    ) -> PyResult<Self> {
        let spec_parsed = parse_spec_yaml(&spec_yaml)?;
        let source_provider = SchemaViewProvider::from_yaml_str_with_patch(
            &source_schema_yaml,
            spec_parsed.source_schema_patches.as_ref(),
        )
        .map_err(|e| PyValueError::new_err(format!("Failed to parse source schema YAML: {e}")))?;
        let spec_parsed = derive_spec(spec_parsed, &source_provider)?;
        let target_provider = target_schema_yaml
            .as_deref()
            .map(|y| {
                SchemaViewProvider::from_yaml_str(y).map_err(|e| {
                    PyValueError::new_err(format!("Failed to parse target schema YAML: {e}"))
                })
            })
            .transpose()?;
        Ok(Self {
            spec: spec_parsed,
            source_provider,
            target_provider,
            source_class,
            lookup_index: None,
        })
    }

    /// Build from direct Python SchemaView and specification objects (or strings).
    /// Bypasses Python-side YAML dumping/parsing via JSON coercion.
    #[staticmethod]
    #[pyo3(signature = (source_schemaview, spec, target_schemaview=None, source_class=None))]
    fn from_python(
        py: Python<'_>,
        source_schemaview: Bound<'_, PyAny>,
        spec: Bound<'_, PyAny>,
        target_schemaview: Option<Bound<'_, PyAny>>,
        source_class: Option<String>,
    ) -> PyResult<Self> {
        let mut spec_val = spec_to_json_value(py, &spec)?;
        linkml_map_core::datamodel::normalise_spec_json(&mut spec_val);
        let spec_parsed: TransformationSpecification = serde_json::from_value(spec_val)
            .map_err(|e| PyValueError::new_err(format!("Failed to parse spec: {e}")))?;

        let source_json = schema_to_json_str(py, &source_schemaview)?;
        let source_provider = SchemaViewProvider::from_yaml_str_with_patch(
            &source_json,
            spec_parsed.source_schema_patches.as_ref(),
        )
        .map_err(|e| PyValueError::new_err(format!("Failed to parse source schema JSON: {e}")))?;
        let spec_parsed = derive_spec(spec_parsed, &source_provider)?;

        let target_provider = target_schemaview
            .map(|t| {
                let target_json = schema_to_json_str(py, &t)?;
                SchemaViewProvider::from_yaml_str(&target_json).map_err(|e| {
                    PyValueError::new_err(format!("Failed to parse target schema JSON: {e}"))
                })
            })
            .transpose()?;

        Ok(Self {
            spec: spec_parsed,
            source_provider,
            target_provider,
            source_class,
            lookup_index: None,
        })
    }

    /// Transform a single Python dict and return the result as a dict.
    fn transform(&self, py: Python<'_>, obj: Bound<'_, PyAny>) -> PyResult<PyObject> {
        run_transform(
            py,
            &obj,
            &self.spec,
            &self.source_provider,
            self.target_provider.as_ref(),
            self.source_class.as_deref(),
            self.lookup_index.as_ref(),
        )
    }

    /// Alias of [`transform`] matching the Python `ObjectTransformer.map_object`
    /// method name (lets the `linkml_map` shim forward calls 1:1).
    #[pyo3(signature = (obj, source_type=None))]
    fn map_object(
        &self,
        py: Python<'_>,
        obj: Bound<'_, PyAny>,
        source_type: Option<String>,
    ) -> PyResult<PyObject> {
        let source_class = source_type.as_deref().or(self.source_class.as_deref());
        run_transform(
            py,
            &obj,
            &self.spec,
            &self.source_provider,
            self.target_provider.as_ref(),
            source_class,
            self.lookup_index.as_ref(),
        )
    }

    /// Transform a list of Python dicts and return a list of result dicts.
    fn transform_many(
        &self,
        py: Python<'_>,
        objs: Vec<Bound<'_, PyAny>>,
    ) -> PyResult<Vec<PyObject>> {
        objs.iter()
            .map(|obj| {
                run_transform(
                    py,
                    obj,
                    &self.spec,
                    &self.source_provider,
                    self.target_provider.as_ref(),
                    self.source_class.as_deref(),
                    self.lookup_index.as_ref(),
                )
            })
            .collect()
    }

    /// Register (or replace) a joined table's rows for cross-table join
    /// resolution.
    ///
    /// Mirrors upstream `Transformer.lookup_index`
    /// (`utils/lookup_index.py::LookupIndex.register_table`), except tables
    /// are supplied directly as in-memory row dicts rather than loaded from a
    /// CSV/TSV/JSON file — the Python caller owns getting the joined data
    /// into memory (however it likes), and only hands the rows to the Rust
    /// engine. This lets a `joins:` block (explicit, or synthesized from an
    /// implicit `{Table.col}` reference — see `Transformer.__init__`) resolve
    /// `populated_from: "alias.field"` / `expr: "{alias.field}"` bindings
    /// against real data instead of always yielding `null`.
    ///
    /// `rows` is a list of dicts, converted to the engine's `Value` via the
    /// same JSON bridge `.transform()` uses. `key_column` names the column in
    /// `rows` that join lookups match against (the spec's `joins:` entry
    /// supplies the *source*-side key via `source_key`/`join_on`).
    ///
    /// Multiple tables accumulate across calls; calling again with the same
    /// `name` replaces that table's rows (mirrors `LookupIndex::register_table`).
    /// Call this before `.transform()` / `.map_object()` / `.transform_many()`
    /// — each call snapshots the currently-registered tables into a fresh
    /// index, so later registrations are not retroactively visible to a
    /// transform that already ran.
    #[pyo3(signature = (name, rows, key_column))]
    fn register_join_table(
        &mut self,
        py: Python<'_>,
        name: String,
        rows: Vec<Bound<'_, PyAny>>,
        key_column: String,
    ) -> PyResult<()> {
        let values: Vec<Value> = rows
            .iter()
            .map(|r| py_to_value(py, r))
            .collect::<PyResult<Vec<_>>>()?;
        let mut index = self.lookup_index.as_deref().cloned().unwrap_or_default();
        index.register_table(&name, &values, &key_column);
        self.lookup_index = Some(Arc::new(index));
        Ok(())
    }

    fn __repr__(&self) -> String {
        format!(
            "Transformer(classes={}, has_target={}, source_class={:?})",
            self.spec
                .class_derivations
                .as_ref()
                .map(|c| c.len())
                .unwrap_or(0),
            self.target_provider.is_some(),
            self.source_class,
        )
    }
}

// ── Free functions ────────────────────────────────────────────────────────────

/// Transform a single object.  Schema(s) and spec are loaded on every call;
/// use `Transformer` when transforming many objects from the same spec.
///
/// Parameters
/// ----------
/// source_obj : dict
///     The source object as a Python dict.
/// source_schema : str
///     Path to the source LinkML schema YAML.
/// spec : str
///     Path to the transformation specification YAML.
/// target_schema : str or None, optional
///     Path to the target LinkML schema YAML.
/// source_class : str or None, optional
///     Name of the source class (inferred when omitted).
///
/// Returns
/// -------
/// dict
///     The transformed object.
#[pyfunction]
#[pyo3(signature = (source_obj, *, source_schema, spec, target_schema=None, source_class=None))]
fn transform_object(
    py: Python<'_>,
    source_obj: Bound<'_, PyAny>,
    source_schema: String,
    spec: String,
    target_schema: Option<String>,
    source_class: Option<String>,
) -> PyResult<PyObject> {
    let spec_parsed = load_spec(&spec)?;
    let source = load_source_provider(&source_schema, &spec_parsed)?;
    let spec_parsed = derive_spec(spec_parsed, &source)?;
    let target_opt: Option<SchemaViewProvider> = target_schema
        .as_deref()
        .map(load_schema_provider)
        .transpose()?;

    run_transform(
        py,
        &source_obj,
        &spec_parsed,
        &source,
        target_opt.as_ref(),
        source_class.as_deref(),
        None,
    )
}

/// Transform a list of objects.  Schema(s) and spec are loaded once for the batch.
///
/// Parameters
/// ----------
/// source_objs : list[dict]
///     List of source objects.
/// source_schema : str
///     Path to the source LinkML schema YAML.
/// spec : str
///     Path to the transformation specification YAML.
/// target_schema : str or None, optional
///     Path to the target LinkML schema YAML.
/// source_class : str or None, optional
///     Name of the source class (inferred when omitted).
///
/// Returns
/// -------
/// list[dict]
///     List of transformed objects.
#[pyfunction]
#[pyo3(signature = (source_objs, *, source_schema, spec, target_schema=None, source_class=None))]
fn transform_objects(
    py: Python<'_>,
    source_objs: Vec<Bound<'_, PyAny>>,
    source_schema: String,
    spec: String,
    target_schema: Option<String>,
    source_class: Option<String>,
) -> PyResult<Vec<PyObject>> {
    let spec_parsed = load_spec(&spec)?;
    let source = load_source_provider(&source_schema, &spec_parsed)?;
    let spec_parsed = derive_spec(spec_parsed, &source)?;
    let target_opt: Option<SchemaViewProvider> = target_schema
        .as_deref()
        .map(load_schema_provider)
        .transpose()?;

    source_objs
        .iter()
        .map(|obj| {
            run_transform(
                py,
                obj,
                &spec_parsed,
                &source,
                target_opt.as_ref(),
                source_class.as_deref(),
                None,
            )
        })
        .collect()
}

// ── PyO3 class: ValidationMessage ─────────────────────────────────────────────

/// A single finding from [`validate_spec`].
///
/// Mirrors `linkml_map_core::validate::ValidationMessage` over the PyO3
/// boundary. `severity` is exposed as the lowercased string form (`"error"` /
/// `"warning"` / `"info"`, via the Rust `Severity`'s `Display` impl) rather
/// than a separate enum type — a plain string is the simplest faithful
/// mirror for a read-only field consumers will mostly print or compare.
/// `__str__`/`__repr__` reproduce the Rust `Display` impl:
/// `"{path}: [{severity}] {message}"`.
#[pyclass(name = "ValidationMessage")]
#[derive(Clone)]
struct PyValidationMessage {
    severity: String,
    path: String,
    message: String,
    category: Option<String>,
}

#[pymethods]
impl PyValidationMessage {
    #[getter]
    fn severity(&self) -> String {
        self.severity.clone()
    }

    #[getter]
    fn path(&self) -> String {
        self.path.clone()
    }

    #[getter]
    fn message(&self) -> String {
        self.message.clone()
    }

    #[getter]
    fn category(&self) -> Option<String> {
        self.category.clone()
    }

    fn __repr__(&self) -> String {
        format!(
            "ValidationMessage(severity={:?}, path={:?}, message={:?}, category={:?})",
            self.severity, self.path, self.message, self.category
        )
    }

    fn __str__(&self) -> String {
        format!("{}: [{}] {}", self.path, self.severity, self.message)
    }
}

impl From<ValidationMessage> for PyValidationMessage {
    fn from(m: ValidationMessage) -> Self {
        Self {
            severity: m.severity.to_string(),
            path: m.path,
            message: m.message,
            category: m.category,
        }
    }
}

/// Cross-reference a spec against its declared source/target schema(s)
/// without running any transform.
///
/// Native binding for `linkml_map_core::validate::validate_spec_semantics`.
/// This is a read-only pre-flight check: it catches an unresolved
/// class/slot/enum name, an unresolvable expression reference, a
/// misconfigured join, an unresolved `is_a`/`mixins`, or a required target
/// slot with no derivation — the kinds of mistakes that would otherwise
/// surface as a silent `null` or a runtime error at transform time.
///
/// Parameters
/// ----------
/// spec : str
///     Path to the transformation specification YAML.
/// source_schema : str or None, optional
///     Path to the source LinkML schema YAML.
/// target_schema : str or None, optional
///     Path to the target LinkML schema YAML.
/// strict : bool, optional
///     When True, unresolved expression references are reported at
///     ``error`` severity instead of ``warning``. Defaults to False.
///
/// Returns
/// -------
/// list[ValidationMessage]
///     Findings from checking the spec against whichever schema(s) were
///     supplied. Returns an empty list when neither `source_schema` nor
///     `target_schema` is given — matching the Rust function, there is
///     nothing to check the spec against. Validation runs on the spec
///     exactly as parsed (no implicit-join synthesis / `derive_spec` first):
///     the whole point of this check is to catch spec problems *before*
///     that stage, so it must see the spec un-derived.
#[pyfunction]
#[pyo3(signature = (spec, *, source_schema=None, target_schema=None, strict=false))]
fn validate_spec(
    spec: String,
    source_schema: Option<String>,
    target_schema: Option<String>,
    strict: bool,
) -> PyResult<Vec<PyValidationMessage>> {
    let spec_parsed = load_spec(&spec)?;

    let source_provider: Option<SchemaViewProvider> = source_schema
        .as_deref()
        .map(|path| load_source_provider(path, &spec_parsed))
        .transpose()?;
    let target_provider: Option<SchemaViewProvider> = target_schema
        .as_deref()
        .map(load_schema_provider)
        .transpose()?;

    let messages = validate_spec_semantics(
        &spec_parsed,
        source_provider
            .as_ref()
            .map(|p| p as &dyn linkml_map_core::schema::SchemaProvider),
        target_provider
            .as_ref()
            .map(|p| p as &dyn linkml_map_core::schema::SchemaProvider),
        strict,
    );

    Ok(messages.into_iter().map(PyValidationMessage::from).collect())
}

// ── Module ────────────────────────────────────────────────────────────────────

/// linkml-map Rust transform engine — compiled core, re-exported by the
/// `linkml_map_rs` Python package (`python/linkml_map_rs/__init__.py`).
#[pymodule]
fn _native(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTransformer>()?;
    m.add_class::<PyValidationMessage>()?;
    m.add_function(wrap_pyfunction!(transform_object, m)?)?;
    m.add_function(wrap_pyfunction!(transform_objects, m)?)?;
    m.add_function(wrap_pyfunction!(validate_spec, m)?)?;
    Ok(())
}
