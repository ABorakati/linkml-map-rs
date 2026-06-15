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
//! # Convenience free functions (schema/spec loaded on every call — use the
//! # class when transforming many objects from the same spec).
//! result = transform_object(obj, source_schema="source.yaml", spec="transform.yaml")
//! results = transform_objects(objs, source_schema="source.yaml", spec="transform.yaml")
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

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use linkml_map_core::{
    datamodel::TransformationSpecification, engine::ObjectTransformer, value::Value,
};
use linkml_map_schemaview::SchemaViewProvider;

// ── Helpers: Python object <-> Value via JSON ─────────────────────────────────

/// Convert a Python object (dict, list, scalar) to a Rust `Value` by going
/// through JSON.  The caller must hold the GIL (`py`).
fn py_to_value(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    let json_mod = py.import("json")?;
    let json_str: String = json_mod.call_method1("dumps", (obj,))?.extract()?;

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

// ── Schema / spec loading ─────────────────────────────────────────────────────

fn load_spec(spec_path: &str) -> PyResult<TransformationSpecification> {
    let yaml_str = std::fs::read_to_string(spec_path)
        .map_err(|e| PyValueError::new_err(format!("Cannot read spec file '{spec_path}': {e}")))?;

    // linkml-map transform specs write class_derivations as a YAML mapping
    // (ClassName -> body) rather than as a list.  Normalise before deserialising.
    let normalised = normalise_transform_yaml(&yaml_str).map_err(|e| {
        PyValueError::new_err(format!("Failed to normalise spec '{spec_path}': {e}"))
    })?;

    let spec: TransformationSpecification = serde_yaml_ng::from_str(&normalised)
        .map_err(|e| PyValueError::new_err(format!("Failed to parse spec '{spec_path}': {e}")))?;

    Ok(spec)
}

/// Normalise a linkml-map transform YAML into the canonical JSON shape that
/// `TransformationSpecification` expects for serde deserialisation.
///
/// Handles:
/// - `class_derivations`: YAML mapping → `Vec<ClassDerivation>` with injected `name`
/// - `slot_derivations` within each class: inject `name` into each slot value
/// - null-shorthand slots (`id:` with no body) → `{"name": "id"}`
/// - `enum_derivations` and nested `permissible_value_derivations`: same name injection
fn normalise_transform_yaml(text: &str) -> anyhow::Result<String> {
    let mut root: serde_json::Value =
        serde_yaml_ng::from_str(text).map_err(|e| anyhow::anyhow!("YAML parse error: {e}"))?;

    if let Some(obj) = root.as_object_mut() {
        // ── class_derivations: mapping → Vec ──────────────────────────────────
        if let Some(cd) = obj.get_mut("class_derivations") {
            if cd.is_object() {
                let mapping = std::mem::replace(cd, serde_json::Value::Null);
                let mut list = Vec::new();
                if let serde_json::Value::Object(m) = mapping {
                    for (class_name, mut val) in m {
                        if val.is_null() {
                            val = serde_json::json!({});
                        }
                        if let Some(o) = val.as_object_mut() {
                            o.insert("name".into(), serde_json::Value::String(class_name.clone()));
                            // slot_derivations: inject `name` into each slot value
                            if let Some(sd) = o.get_mut("slot_derivations") {
                                if sd.is_object() {
                                    let sd_owned = std::mem::replace(sd, serde_json::Value::Null);
                                    if let serde_json::Value::Object(mut sdm) = sd_owned {
                                        for (slot_name, slot_val) in sdm.iter_mut() {
                                            if slot_val.is_null() {
                                                *slot_val = serde_json::json!({});
                                            }
                                            if let Some(so) = slot_val.as_object_mut() {
                                                if !so.contains_key("name") {
                                                    so.insert(
                                                        "name".into(),
                                                        serde_json::Value::String(
                                                            slot_name.clone(),
                                                        ),
                                                    );
                                                }
                                            }
                                        }
                                        *sd = serde_json::Value::Object(sdm);
                                    }
                                }
                            }
                        }
                        list.push(val);
                    }
                }
                *cd = serde_json::Value::Array(list);
            }
        }

        // ── enum_derivations: inject `name` ───────────────────────────────────
        if let Some(ed) = obj.get_mut("enum_derivations") {
            if let Some(edm) = ed.as_object_mut() {
                for (enum_name, enum_val) in edm.iter_mut() {
                    if enum_val.is_null() {
                        *enum_val = serde_json::json!({});
                    }
                    if let Some(eo) = enum_val.as_object_mut() {
                        if !eo.contains_key("name") {
                            eo.insert("name".into(), serde_json::Value::String(enum_name.clone()));
                        }
                        if let Some(pvds) = eo.get_mut("permissible_value_derivations") {
                            if let Some(pvm) = pvds.as_object_mut() {
                                for (pv_name, pv_val) in pvm.iter_mut() {
                                    if pv_val.is_null() {
                                        *pv_val = serde_json::json!({});
                                    }
                                    if let Some(po) = pv_val.as_object_mut() {
                                        if !po.contains_key("name") {
                                            po.insert(
                                                "name".into(),
                                                serde_json::Value::String(pv_name.clone()),
                                            );
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    serde_json::to_string(&root).map_err(|e| anyhow::anyhow!("JSON re-serialise error: {e}"))
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

// ── Core transform logic ──────────────────────────────────────────────────────

/// Inner implementation shared by `Transformer.transform` and the free function.
///
/// `source_class` is `None` → engine infers from `tree_root` or first derivation.
fn run_transform(
    py: Python<'_>,
    obj: &Bound<'_, PyAny>,
    spec: &TransformationSpecification,
    source_provider: &SchemaViewProvider,
    target_provider: Option<&SchemaViewProvider>,
    source_class: Option<&str>,
) -> PyResult<PyObject> {
    let input_value = py_to_value(py, obj)?;

    let engine = ObjectTransformer::new(
        spec.clone(),
        Some(source_provider as &dyn linkml_map_core::schema::SchemaProvider),
        target_provider.map(|p| p as &dyn linkml_map_core::schema::SchemaProvider),
    );

    let result = engine
        .map_object(&input_value, source_class)
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
#[pyclass(name = "Transformer")]
struct PyTransformer {
    spec: TransformationSpecification,
    source_schema_path: String,
    target_schema_path: Option<String>,
    source_class: Option<String>,
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
        // Eagerly validate files exist and parse; fail fast in __init__.
        let spec_parsed = load_spec(&spec)?;
        let _source = load_source_provider(&source_schema, &spec_parsed)?;
        if let Some(ref ts) = target_schema {
            let _target = load_schema_provider(ts)?;
        }

        Ok(Self {
            spec: spec_parsed,
            source_schema_path: source_schema,
            target_schema_path: target_schema,
            source_class,
        })
    }

    /// Transform a single Python dict and return the result as a dict.
    fn transform(&self, py: Python<'_>, obj: Bound<'_, PyAny>) -> PyResult<PyObject> {
        let source = load_source_provider(&self.source_schema_path, &self.spec)?;
        let target_opt: Option<SchemaViewProvider> = self
            .target_schema_path
            .as_deref()
            .map(load_schema_provider)
            .transpose()?;

        run_transform(
            py,
            &obj,
            &self.spec,
            &source,
            target_opt.as_ref(),
            self.source_class.as_deref(),
        )
    }

    /// Transform a list of Python dicts and return a list of result dicts.
    fn transform_many(
        &self,
        py: Python<'_>,
        objs: Vec<Bound<'_, PyAny>>,
    ) -> PyResult<Vec<PyObject>> {
        // Load schemas once for the batch.
        let source = load_source_provider(&self.source_schema_path, &self.spec)?;
        let target_opt: Option<SchemaViewProvider> = self
            .target_schema_path
            .as_deref()
            .map(load_schema_provider)
            .transpose()?;

        objs.iter()
            .map(|obj| {
                run_transform(
                    py,
                    obj,
                    &self.spec,
                    &source,
                    target_opt.as_ref(),
                    self.source_class.as_deref(),
                )
            })
            .collect()
    }

    fn __repr__(&self) -> String {
        format!(
            "Transformer(source_schema={:?}, target_schema={:?}, source_class={:?})",
            self.source_schema_path, self.target_schema_path, self.source_class,
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
            )
        })
        .collect()
}

// ── Module ────────────────────────────────────────────────────────────────────

/// linkml-map Rust transform engine — Python bindings.
#[pymodule]
fn linkml_map_rs(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyTransformer>()?;
    m.add_function(wrap_pyfunction!(transform_object, m)?)?;
    m.add_function(wrap_pyfunction!(transform_objects, m)?)?;
    Ok(())
}
