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
//! # Paths, strictness and spec loading
//!
//! Every schema/spec path argument takes a `str` or any `os.PathLike`
//! (`pathlib.Path`); a missing file raises `PathNotFoundError`, a
//! `FileNotFoundError` that is also a `ValueError`. `strict=True` is upstream's
//! `ObjectTransformer.strict` (the engine's `with_strict_exprs`).
//! `unrestricted_eval=True` is upstream's `ObjectTransformer.unrestricted_eval`
//! (the engine's `with_unrestricted_eval`); the default, as upstream, is the
//! restricted evaluator, so a spec that needs statement blocks, `src.` accessors
//! or comprehension attribute access must opt in.
//!
//! ```python
//! from pathlib import Path
//! from linkml_map_rs import Transformer, load_specification
//! t = Transformer(Path("source.yaml"), Path("transform.yaml"), strict=True)
//! spec = load_specification(Path("transform.yaml"))   # normalised dict
//! ```
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
//! Source objects and results cross the boundary by walking the Python object
//! (`py_to_value`) or the engine `Value` (`value_to_py`) directly; neither
//! direction goes through a JSON string:
//!
//! | Python            | `Value`          | back to Python     |
//! |-------------------|------------------|--------------------|
//! | `None`            | `Null`           | `None`             |
//! | `bool`            | `Bool`           | `bool`             |
//! | `int`             | `Int` (`i64`)    | `int`              |
//! | `float`           | `Float`          | `float`            |
//! | `decimal.Decimal` | `Decimal`        | `decimal.Decimal`  |
//! | `str`             | `Str`            | `str`              |
//! | `list`, `tuple`   | `List`           | `list`             |
//! | `dict`            | `Map` (ordered)  | `dict` (ordered)   |
//!
//! A `decimal.Decimal` is exact in both directions: `range: decimal` slot
//! derivations (upstream 72c0c28) return a real `decimal.Decimal` whose
//! `str()` keeps the source digits (`"52.30"` stays `Decimal("52.30")`), and a
//! `Decimal` in a source object keeps its digits through the engine. A
//! non-finite `Decimal` (`NaN`, `Infinity`) has no exact form and is carried as
//! its `str()`.
//!
//! The rules are those of the former `json.dumps(obj, default=str)` bridge:
//! an `int` beyond `i64` becomes a float; a non-finite `float` becomes `None`
//! on the way out and raises `ValueError` on the way in, as does nesting
//! beyond 128 levels (`serde_json`'s old parse limit); and dict keys
//! may be `str`/`int`/`float`/`bool`/`None` (`TypeError` otherwise).
//!
//! Any other scalar is stringified with `str()` (`default=str`), both for a
//! source *object* (`py_to_value`) and for a dict/`TransformationSpecification`
//! passed as `specification=` (`spec_to_json_value`, which still uses
//! `json.dumps(default=str)`) — the `linkml_map`
//! shim's `create_transformer_specification(spec_dict)` path commonly carries
//! a `yaml.safe_load`-parsed `datetime.date`/`datetime.datetime` (e.g. a bare
//! `publication_date: 2025-08-14` spec metadata field), which `json.dumps`
//! otherwise rejects with `TypeError`. Note the resulting `Value` is a plain
//! string for a date — the core `Value` enum
//! (`linkml_map_core::value::Value`) has no dedicated date/datetime variant —
//! so a value returned from `Transformer.transform`/`.map_object` that
//! originated from a date is a Python `str`, not a `datetime.date` — a
//! permanent (and correct, LinkML-stringification-matching) asymmetry with
//! upstream Python's in-memory `ObjectTransformer`, which preserves native
//! `date`/`datetime` objects untouched.

use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};

use linkml_map_core::{
    datamodel::TransformationSpecification,
    engine::{CompiledExprs, LookupIndex, ObjectTransformer},
    validate::{ValidationMessage, validate_spec_semantics},
    value::{Decimal, Value},
};
use linkml_map_schemaview::SchemaViewProvider;

// ── Helpers: Python object <-> Value ──────────────────────────────────────────

/// Deepest container nesting `py_to_value` accepts. This is `serde_json`'s own
/// parse limit, which the former JSON bridge hit (as `ValueError`); it also
/// keeps the engine's recursion over a `Value` shallow.
const MAX_NESTING: usize = 128;

static DECIMAL_TYPE: GILOnceCell<Py<PyType>> = GILOnceCell::new();

/// The `decimal.Decimal` class, imported once per process.
fn decimal_type<'py>(py: Python<'py>) -> PyResult<&'py Bound<'py, PyType>> {
    DECIMAL_TYPE
        .get_or_try_init(py, || -> PyResult<Py<PyType>> {
            Ok(py
                .import("decimal")?
                .getattr("Decimal")?
                .downcast_into::<PyType>()?
                .unbind())
        })
        .map(|t| t.bind(py))
}

/// Convert a Python object (dict, list, scalar) to a Rust `Value` by walking it
/// directly. The caller must hold the GIL (`py`).
///
/// Accepts exactly what the former `json.dumps(obj, default=str)` bridge did:
/// `bool` is tested before `int`; `tuple` is a list; dict keys may be `str`,
/// `int`, `float`, `bool` or `None` (stringified as `json.dumps` does, else
/// `TypeError`); an `int` beyond `i64` becomes a float; a non-finite float
/// raises `ValueError`; a cyclic structure or nesting beyond 128 levels raises
/// `ValueError`. Anything else is stringified with `str()` (`datetime.date`, `pathlib.Path`, ...), except
/// `decimal.Decimal`, which stays exact as `Value::Decimal`.
fn py_to_value(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<Value> {
    py_to_value_at(py, obj, &mut Vec::new())
}

/// `py_to_value`, with `open` holding the addresses of the containers
/// currently being walked (cycle and depth guard).
fn py_to_value_at(
    py: Python<'_>,
    obj: &Bound<'_, PyAny>,
    open: &mut Vec<usize>,
) -> PyResult<Value> {
    if obj.is_none() {
        return Ok(Value::Null);
    }
    // `bool` is a subclass of `int`, so it must be tested first.
    if let Ok(b) = obj.downcast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }
    if obj.is_instance_of::<PyInt>() {
        return Ok(match obj.extract::<i64>() {
            Ok(i) => Value::Int(i),
            // Beyond `i64`: the JSON bridge parsed these as floats.
            Err(_) => Value::Float(
                obj.extract::<f64>()
                    .map_err(|e| PyValueError::new_err(format!("Value conversion error: {e}")))?,
            ),
        });
    }
    if obj.is_instance_of::<PyFloat>() {
        let f: f64 = obj.extract()?;
        if !f.is_finite() {
            return Err(PyValueError::new_err(format!(
                "Value conversion error: cannot convert non-finite float {f} to a value"
            )));
        }
        return Ok(Value::Float(f));
    }
    if obj.is_instance_of::<PyString>() {
        return Ok(Value::Str(obj.extract()?));
    }

    let is_list = obj.is_instance_of::<PyList>() || obj.is_instance_of::<PyTuple>();
    let is_dict = obj.is_instance_of::<PyDict>();
    if is_list || is_dict {
        let id = obj.as_ptr() as usize;
        if open.contains(&id) {
            return Err(PyValueError::new_err("Circular reference detected"));
        }
        if open.len() >= MAX_NESTING {
            return Err(PyValueError::new_err(format!(
                "Value conversion error: nesting deeper than {MAX_NESTING} levels"
            )));
        }
        open.push(id);
        let value = if is_list {
            obj.try_iter()?
                .map(|item| py_to_value_at(py, &item?, open))
                .collect::<PyResult<Vec<_>>>()
                .map(Value::List)
        } else {
            let dict = obj.downcast::<PyDict>()?;
            let mut map = IndexMap::with_capacity(dict.len());
            dict.iter()
                .try_for_each(|(k, v)| -> PyResult<()> {
                    map.insert(py_key_to_string(&k)?, py_to_value_at(py, &v, open)?);
                    Ok(())
                })
                .map(|()| Value::Map(map))
        };
        open.pop();
        return value;
    }

    let text: String = obj.str()?.extract()?;
    if obj.is_instance(decimal_type(py)?)? {
        // A NaN/sNaN/Infinity `Decimal` has no finite exact form: it stays the
        // string `str()` gave, as the JSON bridge did.
        if let Ok(d) = Decimal::parse_python(&text) {
            return Ok(Value::Decimal(d));
        }
    }
    // `default=str`: dates, paths and other non-JSON scalars become their text.
    Ok(Value::Str(text))
}

/// A dict key as `json.dumps` writes it (`TypeError` for any other type).
fn py_key_to_string(key: &Bound<'_, PyAny>) -> PyResult<String> {
    if key.is_instance_of::<PyString>() {
        return key.extract();
    }
    if key.is_none() {
        return Ok("null".to_owned());
    }
    if let Ok(b) = key.downcast::<PyBool>() {
        return Ok(if b.is_true() { "true" } else { "false" }.to_owned());
    }
    if key.is_instance_of::<PyInt>() {
        return key.str()?.extract();
    }
    if key.is_instance_of::<PyFloat>() {
        let f: f64 = key.extract()?;
        return Ok(if f.is_nan() {
            "NaN".to_owned()
        } else if f.is_infinite() {
            if f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
        } else {
            key.repr()?.extract()?
        });
    }
    Err(PyTypeError::new_err(format!(
        "keys must be str, int, float, bool or None, not {}",
        key.get_type().name()?
    )))
}

/// Convert a Rust `Value` back to a Python object (dict/list/scalar) by
/// walking it directly. `Value::Decimal` becomes an exact `decimal.Decimal`; a
/// non-finite float becomes `None`, as `serde_json` serialises it.
fn value_to_py(py: Python<'_>, val: &Value) -> PyResult<PyObject> {
    Ok(value_to_bound(py, val)?.unbind())
}

fn value_to_bound<'py>(py: Python<'py>, val: &Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match val {
        Value::Null => py.None().into_bound(py),
        Value::Bool(b) => PyBool::new(py, *b).to_owned().into_any(),
        Value::Int(i) => i.into_pyobject(py)?.into_any(),
        Value::Float(f) if f.is_finite() => PyFloat::new(py, *f).into_any(),
        Value::Float(_) => py.None().into_bound(py),
        // `to_sci_string` is Python's `str(Decimal)`, so this rebuilds the
        // value with its exact digits and exponent (`52.30` stays `52.30`).
        Value::Decimal(d) => decimal_type(py)?.call1((d.to_sci_string(),))?,
        Value::Str(s) => PyString::new(py, s).into_any(),
        Value::List(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(value_to_bound(py, item)?)?;
            }
            list.into_any()
        }
        Value::Map(map) => {
            let dict = PyDict::new(py);
            for (k, v) in map {
                dict.set_item(k, value_to_bound(py, v)?)?;
            }
            dict.into_any()
        }
    })
}

// ── Helpers: path arguments ───────────────────────────────────────────────────

static PATH_NOT_FOUND: GILOnceCell<Py<PyType>> = GILOnceCell::new();

/// `linkml_map_rs.PathNotFoundError`: raised when a schema/spec path does not
/// exist. It subclasses BOTH `FileNotFoundError` (what upstream `open(path)` /
/// `SchemaView(path)` raise) and `ValueError` (what this binding has always
/// raised for a bad path), so callers written against either keep working.
fn path_not_found_type<'py>(py: Python<'py>) -> PyResult<&'py Bound<'py, PyType>> {
    PATH_NOT_FOUND
        .get_or_try_init(py, || -> PyResult<Py<PyType>> {
            let builtins = py.import("builtins")?;
            let bases = (
                builtins.getattr("FileNotFoundError")?,
                builtins.getattr("ValueError")?,
            );
            let namespace = PyDict::new(py);
            namespace.set_item("__module__", "linkml_map_rs")?;
            namespace.set_item(
                "__doc__",
                "A schema or specification path does not exist \
                 (both a FileNotFoundError and a ValueError).",
            )?;
            let ty = builtins
                .getattr("type")?
                .call1(("PathNotFoundError", bases, namespace))?;
            Ok(ty.downcast_into::<PyType>()?.unbind())
        })
        .map(|ty| ty.bind(py))
}

/// Build the `OSError(ENOENT, ..., path)`-shaped error for a missing path.
fn path_not_found(path: &str) -> PyErr {
    Python::with_gil(|py| match path_not_found_type(py) {
        Ok(ty) => PyErr::from_type(
            ty.clone(),
            (2, "No such file or directory", path.to_owned()),
        ),
        Err(e) => e,
    })
}

/// A schema/spec path argument: a `str` or any `os.PathLike` (`pathlib.Path`).
///
/// Mirrors upstream `173121b`, which accepts `str | Path` wherever a file is
/// named. `bytes` paths are decoded with the filesystem encoding.
struct PathArg(String);

impl<'py> FromPyObject<'py> for PathArg {
    fn extract_bound(ob: &Bound<'py, PyAny>) -> PyResult<Self> {
        if let Ok(s) = ob.downcast::<PyString>() {
            return Ok(Self(s.to_string()));
        }
        let decoded = ob.py().import("os")?.call_method1("fsdecode", (ob,))?;
        Ok(Self(decoded.extract()?))
    }
}

/// The filesystem path of an `os.PathLike` object, or `None` for anything else
/// (including `str`, whose in-memory-YAML-text superset is handled by callers).
fn fspath(ob: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    if ob.is_instance_of::<PyString>() || !ob.hasattr("__fspath__")? {
        return Ok(None);
    }
    let decoded = ob.py().import("os")?.call_method1("fsdecode", (ob,))?;
    Ok(Some(decoded.extract()?))
}

/// Read a schema/spec file; a missing file raises [`path_not_found`].
fn read_text(path: &str, what: &str) -> PyResult<String> {
    if !std::path::Path::new(path).exists() {
        return Err(path_not_found(path));
    }
    std::fs::read_to_string(path)
        .map_err(|e| PyValueError::new_err(format!("Cannot read {what} file '{path}': {e}")))
}

/// Serialise a Python dict (or other `json.dumps`-able value) to a `serde_json::Value`.
///
/// `default=str` mirrors `py_to_value`: a dict loaded via `yaml.safe_load` can
/// carry non-JSON scalars (e.g. a bare `publication_date: 2025-08-14` becomes
/// `datetime.date`), which `json.dumps` otherwise rejects with `TypeError`.
fn dict_to_json_value(py: Python<'_>, obj: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    let kwargs = PyDict::new(py);
    kwargs.set_item("default", py.import("builtins")?.getattr("str")?)?;
    let json_str: String = py
        .import("json")?
        .call_method("dumps", (obj,), Some(&kwargs))?
        .extract()?;
    serde_json::from_str(&json_str)
        .map_err(|e| PyValueError::new_err(format!("JSON parse error: {e}")))
}

/// The `linkml_runtime` JSON dumper *instance*.
///
/// Fetched as the `linkml_runtime.dumpers.json_dumper` attribute of the package:
/// importing the dotted name returns the *module* (`sys.modules`), which has no
/// `dumps` in current `linkml-runtime` releases; the package attribute is the
/// `JSONDumper()` instance everywhere.
fn json_dumper<'py>(py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
    py.import("linkml_runtime.dumpers")?.getattr("json_dumper")
}

/// Coerce a Python SchemaView / SchemaDefinition / path / YAML-text argument to
/// schema text (YAML or JSON) the Rust schema loader accepts.
///
/// A path-like is always a file path (missing file: `PathNotFoundError`). A
/// `str` is a file path when such a file exists, otherwise YAML text.
fn schema_to_json_str(py: Python<'_>, sv: &Bound<'_, PyAny>) -> PyResult<String> {
    if let Some(path) = fspath(sv)? {
        return read_text(&path, "schema");
    }
    if sv.is_instance_of::<PyString>() {
        let s: String = sv.extract()?;
        let path = std::path::Path::new(&s);
        if path.exists() && path.is_file() {
            let content = std::fs::read_to_string(path).map_err(|e| {
                PyValueError::new_err(format!("Cannot read schema file '{s}': {e}"))
            })?;
            return Ok(content);
        }
        return Ok(s);
    }

    let schema = if sv.hasattr("schema")? {
        sv.getattr("schema")?
    } else {
        sv.clone()
    };

    let json_dumper = json_dumper(py)?;
    let json_str: String = json_dumper.call_method1("dumps", (schema,))?.extract()?;
    Ok(json_str)
}

/// Coerce a Python specification (dict, dataclass, path, YAML text) argument to a `serde_json::Value`.
///
/// A path-like is always a file path (missing file: `PathNotFoundError`). A
/// `str` is a file path when such a file exists, otherwise YAML text (a
/// superset of upstream, where a `str` is always a path).
fn spec_to_json_value(py: Python<'_>, spec: &Bound<'_, PyAny>) -> PyResult<serde_json::Value> {
    if let Some(path) = fspath(spec)? {
        let content = read_text(&path, "spec")?;
        return serde_yaml_ng::from_str(&content)
            .map_err(|e| PyValueError::new_err(format!("Failed to parse spec: {e}")));
    }

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
        return dict_to_json_value(py, spec);
    }

    // Otherwise assume it is a dataclass object.
    let json_dumper = json_dumper(py)?;
    let json_str: String = json_dumper.call_method1("dumps", (spec,))?.extract()?;
    let val: serde_json::Value = serde_json::from_str(&json_str)
        .map_err(|e| PyValueError::new_err(format!("JSON parse error: {e}")))?;
    Ok(val)
}

// ── Schema / spec loading ─────────────────────────────────────────────────────

fn load_spec(spec_path: &str) -> PyResult<TransformationSpecification> {
    parse_spec_yaml(&read_text(spec_path, "spec")?)
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
    if !path.exists() {
        return Err(path_not_found(schema_path));
    }
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
    if !path.exists() {
        return Err(path_not_found(schema_path));
    }
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
#[allow(clippy::too_many_arguments)]
fn run_transform(
    py: Python<'_>,
    obj: &Bound<'_, PyAny>,
    spec: &TransformationSpecification,
    source_provider: &SchemaViewProvider,
    target_provider: Option<&SchemaViewProvider>,
    source_class: Option<&str>,
    lookup_index: Option<&Arc<LookupIndex>>,
    compiled_exprs: Option<&CompiledExprs>,
    strict: bool,
    unrestricted_eval: bool,
) -> PyResult<PyObject> {
    let input_value = py_to_value(py, obj)?;

    let mut engine = ObjectTransformer::new(
        spec.clone(),
        Some(source_provider as &dyn linkml_map_core::schema::SchemaProvider),
        target_provider.map(|p| p as &dyn linkml_map_core::schema::SchemaProvider),
    );
    // Upstream `ObjectTransformer(strict=...)`: raise on unresolved expression
    // references instead of warning and yielding null.
    engine = engine.with_strict_exprs(strict);
    // Upstream `ObjectTransformer(unrestricted_eval=...)`: statement blocks with
    // `src` and the asteval fallback. Off by default, as upstream.
    engine = engine.with_unrestricted_eval(unrestricted_eval);
    if let Some(ce) = compiled_exprs {
        engine = engine.with_compiled_exprs(ce);
    }
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
    compiled_exprs: CompiledExprs,
    /// Upstream `ObjectTransformer.strict`; forwarded to the engine on every call.
    strict: bool,
    /// Upstream `ObjectTransformer.unrestricted_eval`; forwarded to the engine on every call.
    unrestricted_eval: bool,
    /// Cross-table join data, populated via `register_join_table`. `None`
    /// until the first registration; attached to the engine on every
    /// `transform` / `transform_many` / `map_object` call once present.
    lookup_index: Option<Arc<LookupIndex>>,
}

#[pymethods]
impl PyTransformer {
    #[new]
    #[pyo3(signature = (source_schema, spec, target_schema=None, source_class=None, *, strict=false, unrestricted_eval=false))]
    fn new(
        source_schema: PathArg,
        spec: PathArg,
        target_schema: Option<PathArg>,
        source_class: Option<String>,
        strict: bool,
        unrestricted_eval: bool,
    ) -> PyResult<Self> {
        let spec_parsed = load_spec(&spec.0)?;
        let source_provider = load_source_provider(&source_schema.0, &spec_parsed)?;
        let spec_parsed = derive_spec(spec_parsed, &source_provider)?;
        let compiled_exprs = CompiledExprs::build(&spec_parsed).map_err(|e| {
            PyValueError::new_err(format!("Failed to compile expression ASTs: {e}"))
        })?;
        let target_provider = target_schema
            .as_ref()
            .map(|p| load_schema_provider(&p.0))
            .transpose()?;
        Ok(Self {
            spec: spec_parsed,
            source_provider,
            target_provider,
            source_class,
            compiled_exprs,
            strict,
            unrestricted_eval,
            lookup_index: None,
        })
    }

    /// Build from in-memory YAML strings (schema + spec), instead of file paths.
    /// Used by the `linkml_map`-compatible shim, which dumps SchemaView / spec
    /// objects to YAML. Schemas/spec are parsed once here.
    #[staticmethod]
    #[pyo3(signature = (source_schema_yaml, spec_yaml, target_schema_yaml=None, source_class=None, *, strict=false, unrestricted_eval=false))]
    fn from_yaml(
        source_schema_yaml: String,
        spec_yaml: String,
        target_schema_yaml: Option<String>,
        source_class: Option<String>,
        strict: bool,
        unrestricted_eval: bool,
    ) -> PyResult<Self> {
        let spec_parsed = parse_spec_yaml(&spec_yaml)?;
        let source_provider = SchemaViewProvider::from_yaml_str_with_patch(
            &source_schema_yaml,
            spec_parsed.source_schema_patches.as_ref(),
        )
        .map_err(|e| PyValueError::new_err(format!("Failed to parse source schema YAML: {e}")))?;
        let spec_parsed = derive_spec(spec_parsed, &source_provider)?;
        let compiled_exprs = CompiledExprs::build(&spec_parsed).map_err(|e| {
            PyValueError::new_err(format!("Failed to compile expression ASTs: {e}"))
        })?;
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
            compiled_exprs,
            strict,
            unrestricted_eval,
            lookup_index: None,
        })
    }

    /// Build from direct Python SchemaView and specification objects (or strings).
    /// Bypasses Python-side YAML dumping/parsing via JSON coercion.
    #[staticmethod]
    #[pyo3(signature = (source_schemaview, spec, target_schemaview=None, source_class=None, *, strict=false, unrestricted_eval=false))]
    fn from_python(
        py: Python<'_>,
        source_schemaview: Bound<'_, PyAny>,
        spec: Bound<'_, PyAny>,
        target_schemaview: Option<Bound<'_, PyAny>>,
        source_class: Option<String>,
        strict: bool,
        unrestricted_eval: bool,
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
        let compiled_exprs = CompiledExprs::build(&spec_parsed).map_err(|e| {
            PyValueError::new_err(format!("Failed to compile expression ASTs: {e}"))
        })?;

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
            compiled_exprs,
            strict,
            unrestricted_eval,
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
            Some(&self.compiled_exprs),
            self.strict,
            self.unrestricted_eval,
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
            Some(&self.compiled_exprs),
            self.strict,
            self.unrestricted_eval,
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
                    Some(&self.compiled_exprs),
                    self.strict,
                    self.unrestricted_eval,
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
#[pyo3(signature = (source_obj, *, source_schema, spec, target_schema=None, source_class=None, strict=false, unrestricted_eval=false))]
fn transform_object(
    py: Python<'_>,
    source_obj: Bound<'_, PyAny>,
    source_schema: PathArg,
    spec: PathArg,
    target_schema: Option<PathArg>,
    source_class: Option<String>,
    strict: bool,
    unrestricted_eval: bool,
) -> PyResult<PyObject> {
    let spec_parsed = load_spec(&spec.0)?;
    let source = load_source_provider(&source_schema.0, &spec_parsed)?;
    let spec_parsed = derive_spec(spec_parsed, &source)?;
    let compiled_exprs = CompiledExprs::build(&spec_parsed)
        .map_err(|e| PyValueError::new_err(format!("Failed to compile expression ASTs: {e}")))?;
    let target_opt: Option<SchemaViewProvider> = target_schema
        .as_ref()
        .map(|p| load_schema_provider(&p.0))
        .transpose()?;

    run_transform(
        py,
        &source_obj,
        &spec_parsed,
        &source,
        target_opt.as_ref(),
        source_class.as_deref(),
        None,
        Some(&compiled_exprs),
        strict,
        unrestricted_eval,
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
#[pyo3(signature = (source_objs, *, source_schema, spec, target_schema=None, source_class=None, strict=false, unrestricted_eval=false))]
fn transform_objects(
    py: Python<'_>,
    source_objs: Vec<Bound<'_, PyAny>>,
    source_schema: PathArg,
    spec: PathArg,
    target_schema: Option<PathArg>,
    source_class: Option<String>,
    strict: bool,
    unrestricted_eval: bool,
) -> PyResult<Vec<PyObject>> {
    let spec_parsed = load_spec(&spec.0)?;
    let source = load_source_provider(&source_schema.0, &spec_parsed)?;
    let spec_parsed = derive_spec(spec_parsed, &source)?;
    let compiled_exprs = CompiledExprs::build(&spec_parsed)
        .map_err(|e| PyValueError::new_err(format!("Failed to compile expression ASTs: {e}")))?;
    let target_opt: Option<SchemaViewProvider> = target_schema
        .as_ref()
        .map(|p| load_schema_provider(&p.0))
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
                Some(&compiled_exprs),
                strict,
                unrestricted_eval,
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
    spec: PathArg,
    source_schema: Option<PathArg>,
    target_schema: Option<PathArg>,
    strict: bool,
) -> PyResult<Vec<PyValidationMessage>> {
    let spec_parsed = load_spec(&spec.0)?;

    let source_provider: Option<SchemaViewProvider> = source_schema
        .as_ref()
        .map(|path| load_source_provider(&path.0, &spec_parsed))
        .transpose()?;
    let target_provider: Option<SchemaViewProvider> = target_schema
        .as_ref()
        .map(|p| load_schema_provider(&p.0))
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

    Ok(messages
        .into_iter()
        .map(PyValidationMessage::from)
        .collect())
}

// ── load_specification ────────────────────────────────────────────────────────

/// Load a transformation specification from a YAML file or a dict.
///
/// Native counterpart of upstream `linkml_map.utils.loaders.load_specification`
/// (`173121b`): a `str` / `os.PathLike` is a FILE PATH (never YAML text), a dict
/// is normalised like a file's contents (`class_derivations` mapping to a named
/// list, injected agent types, ...). The spec is also validated against the
/// datamodel, as upstream's `TransformationSpecification(**spec)` does.
///
/// Parameters
/// ----------
/// spec : dict or str or os.PathLike
///     A parsed specification, or the path of a YAML specification file.
///
/// Returns
/// -------
/// dict
///     The normalised specification, as a NEW dict.
///
/// Notes
/// -----
/// Upstream returns a pydantic `TransformationSpecification` and normalises a
/// dict argument IN PLACE; the Rust datamodel is not a Python class, so this
/// returns the normalised plain dict and leaves the caller's dict untouched.
/// The `linkml_map` shim wraps the result in a dict-backed
/// `TransformationSpecification` and mirrors the in-place normalisation.
///
/// Raises
/// ------
/// PathNotFoundError
///     (a `FileNotFoundError` and a `ValueError`) the path does not exist.
/// ValueError
///     The YAML is malformed or does not match the specification datamodel.
#[pyfunction]
fn load_specification(py: Python<'_>, spec: &Bound<'_, PyAny>) -> PyResult<PyObject> {
    let mut value = if spec.is_instance_of::<PyDict>() {
        dict_to_json_value(py, spec)?
    } else {
        let path = PathArg::extract_bound(spec)?.0;
        serde_yaml_ng::from_str(&read_text(&path, "spec")?)
            .map_err(|e| PyValueError::new_err(format!("Failed to parse spec: {e}")))?
    };
    linkml_map_core::datamodel::normalise_spec_json(&mut value);
    serde_json::from_value::<TransformationSpecification>(value.clone())
        .map_err(|e| PyValueError::new_err(format!("Failed to parse spec: {e}")))?;
    let json_str = serde_json::to_string(&value)
        .map_err(|e| PyValueError::new_err(format!("JSON serialisation error: {e}")))?;
    Ok(py
        .import("json")?
        .call_method1("loads", (json_str,))?
        .unbind())
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
    m.add_function(wrap_pyfunction!(load_specification, m)?)?;
    m.add("PathNotFoundError", path_not_found_type(m.py())?)?;
    Ok(())
}
