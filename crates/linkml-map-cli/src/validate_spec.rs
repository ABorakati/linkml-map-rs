//! `linkml-tr-rs validate-spec`: a port of upstream `cli.py::validate_spec_cmd` and
//! `linkml_map.validator.validate_spec_file` / `validate_spec`.
//!
//! The command validates spec files individually (`_validate_spec_individual`: a header
//! and the findings on **stderr**, `<path>: ok` on **stdout**), or merged into one spec
//! (`--merge`, `_validate_spec_merged`), or lists the class-derivation names of the
//! merged spec (`--list-entities`). Exit status is 1 when any file has an error, or any
//! warning under `--strict`.
//!
//! Layers: the structural check (upstream validates the normalised dict against the
//! JSON Schema generated from `transformer_model.yaml`; here the typed datamodel's
//! deserialisation plays that part, so a structural message carries serde's text, not
//! jsonschema's), then the deprecated-field scan, then the semantic checks against the
//! source / target schemas (explicit, or auto-detected from the spec).

use std::io::Write;
use std::path::{Component, Path, PathBuf};

use linkml_map_core::datamodel::{
    SchemaReference, TransformationSpecification, normalise_spec_json,
};
use linkml_map_core::schema::SchemaProvider;
use linkml_map_core::validate::{
    Severity, ValidationMessage, check_deprecated_fields, validate_spec_semantics,
};
use linkml_map_schemaview::SchemaViewProvider;
use serde_json::{Map, Value as Json};

/// The `validate-spec` options (upstream's click options).
#[derive(Debug, Clone, Default)]
pub struct ValidateSpecOptions {
    /// Spec files and/or directories (`SPEC_FILES...`).
    pub spec_files: Vec<String>,
    /// `--source-schema`: overrides auto-detection from the spec.
    pub source_schema: Option<String>,
    /// `--target-schema`: overrides auto-detection from the spec.
    pub target_schema: Option<String>,
    /// `--strict`: warnings are errors; unresolved expression references too.
    pub strict: bool,
    /// `--no-warnings`: do not print warnings.
    pub no_warnings: bool,
    /// `--merge`: merge all spec files into one before validating.
    pub merge: bool,
    /// `--entity`: with `--merge`, validate only this class derivation.
    pub entity: Option<String>,
    /// `--emit-spec`: with `--merge`, write the resolved spec here (`-` for stdout).
    pub emit_spec: Option<String>,
    /// `--list-entities`: print the merged spec's class-derivation names and exit.
    pub list_entities: bool,
}

/// Run `validate-spec`, writing to `out` (stdout) and `err` (stderr); returns the exit code.
pub fn run_validate_spec(
    opts: &ValidateSpecOptions,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let paths: Vec<&str> = opts.spec_files.iter().map(String::as_str).collect();
    if opts.list_entities {
        return match merge_or_exit(&paths, err) {
            Ok(merged) => {
                let mut names: Vec<String> = class_derivation_names(&merged);
                names.sort();
                names.dedup();
                for name in names {
                    let _ = writeln!(out, "{name}");
                }
                0
            }
            Err(code) => code,
        };
    }
    if opts.merge {
        validate_spec_merged(opts, &paths, out, err)
    } else {
        if opts.entity.is_some() || opts.emit_spec.is_some() {
            let _ = writeln!(err, "--entity and --emit-spec require --merge");
            return 1;
        }
        validate_spec_individual(opts, &paths, out, err)
    }
}

// ── individual files ─────────────────────────────────────────────────────────

/// `_validate_spec_individual`.
fn validate_spec_individual(
    opts: &ValidateSpecOptions,
    paths: &[&str],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let resolved = match resolve_spec_paths(paths) {
        Ok(r) => r,
        Err(e) => {
            let _ = writeln!(err, "{e}");
            return 1;
        }
    };
    if resolved.is_empty() {
        let _ = writeln!(err, "No YAML files found in the provided paths");
        return 1;
    }

    let (mut has_errors, mut has_warnings) = (false, false);
    for path in &resolved {
        let shown = py_path_str(path);
        let messages = match validate_spec_file(
            path,
            opts.source_schema.as_deref(),
            opts.target_schema.as_deref(),
            opts.strict,
        ) {
            Ok(m) => m,
            Err(e) => {
                // Upstream lets the exception (unreadable file, YAML error) escape.
                let _ = writeln!(err, "Error: {e:#}");
                return 1;
            }
        };
        let errors: Vec<_> = messages
            .iter()
            .filter(|m| m.severity == Severity::Error)
            .collect();
        let warnings: Vec<_> = messages
            .iter()
            .filter(|m| m.severity == Severity::Warning)
            .collect();

        if !errors.is_empty() {
            has_errors = true;
            let _ = writeln!(err, "{shown}:");
            for msg in &errors {
                let _ = writeln!(err, "  {msg}");
            }
        }
        if !warnings.is_empty() {
            has_warnings = true;
        }
        if !warnings.is_empty() && !opts.no_warnings {
            if errors.is_empty() {
                let _ = writeln!(err, "{shown}:");
            }
            for msg in &warnings {
                let _ = writeln!(err, "  {msg}");
            }
        }
        if errors.is_empty() && warnings.is_empty() {
            let _ = writeln!(out, "{shown}: ok");
        } else if errors.is_empty() {
            let _ = writeln!(out, "{shown}: ok (with warnings)");
        }
    }
    i32::from(has_errors || (opts.strict && has_warnings))
}

/// `validator.validate_spec_file`: read a spec file and validate it.
///
/// # Errors
/// The file cannot be read or is not valid YAML (upstream lets those exceptions escape).
pub fn validate_spec_file(
    path: &Path,
    source_schema: Option<&str>,
    target_schema: Option<&str>,
    strict: bool,
) -> anyhow::Result<Vec<ValidationMessage>> {
    use anyhow::Context as _;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading spec {}", path.display()))?;
    let data = load_yaml(&text).with_context(|| format!("parsing spec {}", path.display()))?;
    if !data.is_object() {
        return Ok(vec![ValidationMessage::new(
            Severity::Error,
            "$",
            format!(
                "Expected a YAML mapping at top level, got {}",
                py_type_name(&data)
            ),
        )]);
    }
    Ok(validate_spec_value(
        &data,
        source_schema,
        target_schema,
        strict,
        path.parent(),
    ))
}

/// `validator.validate_spec`: the structural check, then (only when it finds nothing)
/// the deprecated-field scan and the semantic checks.
///
/// `base_path` is the directory relative schema paths in the spec resolve against.
pub fn validate_spec_value(
    data: &Json,
    source_schema: Option<&str>,
    target_schema: Option<&str>,
    strict: bool,
    base_path: Option<&Path>,
) -> Vec<ValidationMessage> {
    // The scan reads the raw shape (it handles compact keys itself); the typed
    // datamodel needs the normalised one.
    let scan_messages = check_deprecated_fields(data);
    let mut normalised = data.clone();
    normalise_spec_json(&mut normalised);
    let spec: TransformationSpecification = match serde_json::from_value(normalised) {
        Ok(spec) => spec,
        Err(e) => return vec![ValidationMessage::new(Severity::Error, "$", e.to_string())],
    };

    let mut messages = scan_messages;
    messages.extend(semantic_messages(
        &spec,
        source_schema,
        target_schema,
        strict,
        base_path,
    ));
    messages
}

/// `validator.validate_spec_semantics`'s schema resolution around the typed check.
fn semantic_messages(
    spec: &TransformationSpecification,
    source_schema: Option<&str>,
    target_schema: Option<&str>,
    strict: bool,
    base_path: Option<&Path>,
) -> Vec<ValidationMessage> {
    let mut messages = Vec::new();
    let source = resolve_schemaview(
        spec.source_schema.as_ref(),
        source_schema,
        base_path,
        "source_schema",
        |p| SchemaViewProvider::load_from_path_with_patch(p, spec.source_schema_patches.as_ref()),
        &mut messages,
    );
    let target = resolve_schemaview(
        spec.target_schema.as_ref(),
        target_schema,
        base_path,
        "target_schema",
        SchemaViewProvider::load_from_path,
        &mut messages,
    );
    messages.extend(validate_spec_semantics(
        spec,
        source.as_ref().map(|p| p as &dyn SchemaProvider),
        target.as_ref().map(|p| p as &dyn SchemaProvider),
        strict,
    ));
    messages
}

/// `validator._resolve_schema_path`: an explicit path wins; else the spec's
/// `source_file` (or `name`) as a path relative to the spec's directory, as given, or a
/// URL. Identifier-style names are not auto-resolved.
fn resolve_schema_path(
    spec_value: Option<&SchemaReference>,
    explicit: Option<&str>,
    base_path: Option<&Path>,
) -> Option<(String, bool)> {
    if let Some(explicit) = explicit {
        return Some((explicit.to_owned(), true));
    }
    let locator = spec_value?.path().filter(|l| !l.is_empty())?;
    if let Some(base) = base_path {
        let candidate = base.join(locator);
        if candidate.exists() {
            return Some((candidate.to_string_lossy().into_owned(), false));
        }
    }
    if Path::new(locator).exists() || locator.contains("://") {
        return Some((locator.to_owned(), false));
    }
    None
}

/// `validator._resolve_schemaview`: load the schema, recording a failure as an error
/// (explicit path) or a warning (auto-detected).
fn resolve_schemaview<E: std::fmt::Display>(
    spec_value: Option<&SchemaReference>,
    explicit: Option<&str>,
    base_path: Option<&Path>,
    label: &str,
    load: impl FnOnce(&Path) -> Result<SchemaViewProvider, E>,
    messages: &mut Vec<ValidationMessage>,
) -> Option<SchemaViewProvider> {
    let (path, explicit_provided) = resolve_schema_path(spec_value, explicit, base_path)?;
    match load(Path::new(&path)) {
        Ok(sv) => Some(sv),
        Err(exc) => {
            messages.push(ValidationMessage::new(
                if explicit_provided {
                    Severity::Error
                } else {
                    Severity::Warning
                },
                label,
                format!("Could not load {} '{path}': {exc}", label.replace('_', " ")),
            ));
            None
        }
    }
}

// ── merged spec ──────────────────────────────────────────────────────────────

/// `_validate_spec_merged`.
fn validate_spec_merged(
    opts: &ValidateSpecOptions,
    paths: &[&str],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let mut merged = match merge_or_exit(paths, err) {
        Ok(m) => m,
        Err(code) => return code,
    };

    // Filter before validating, so only what `map-data --entity` would run is checked.
    if let Some(entity) = &opts.entity
        && let Some(cd) = merged.get_mut("class_derivations")
    {
        match cd {
            Json::Array(items) => items.retain(|item| {
                item.as_object().is_some_and(|o| {
                    o.get("name").and_then(Json::as_str) == Some(entity.as_str())
                        || (o.len() == 1 && o.contains_key(entity))
                })
            }),
            Json::Object(map) => map.retain(|k, _| k == entity),
            _ => {}
        }
    }

    let messages = validate_spec_value(
        &Json::Object(merged.clone()),
        opts.source_schema.as_deref(),
        opts.target_schema.as_deref(),
        opts.strict,
        None,
    );
    let errors: Vec<_> = messages
        .iter()
        .filter(|m| m.severity == Severity::Error)
        .collect();
    let warnings: Vec<_> = messages
        .iter()
        .filter(|m| m.severity == Severity::Warning)
        .collect();

    if !errors.is_empty() {
        let _ = writeln!(err, "Merged spec validation errors:");
        for msg in &errors {
            let _ = writeln!(err, "  {msg}");
        }
    }
    if !warnings.is_empty() && !opts.no_warnings {
        let _ = writeln!(err, "Merged spec validation warnings:");
        for msg in &warnings {
            let _ = writeln!(err, "  {msg}");
        }
    }
    if !errors.is_empty() {
        return 1;
    }
    if warnings.is_empty() {
        let _ = writeln!(out, "Merged spec: ok");
    } else {
        let _ = writeln!(out, "Merged spec: ok (with warnings)");
    }
    if opts.strict && !warnings.is_empty() {
        return 1;
    }

    if let Some(emit_spec) = &opts.emit_spec {
        let mut normalised = Json::Object(merged);
        normalise_spec_json(&mut normalised);
        let spec: TransformationSpecification = match serde_json::from_value(normalised) {
            Ok(s) => s,
            Err(e) => {
                let _ = writeln!(err, "Error: {e}");
                return 1;
            }
        };
        let text = dump_resolved_spec(&spec);
        if emit_spec == "-" {
            let _ = writeln!(out, "{text}");
        } else if let Err(e) = std::fs::write(emit_spec, text) {
            let _ = writeln!(err, "Error: writing {emit_spec}: {e}");
            return 1;
        } else {
            let _ = writeln!(out, "Wrote resolved spec to {emit_spec}");
        }
    }
    0
}

/// `yaml_dumper.dumps(spec)`: block YAML in field order with empty values removed.
///
/// PARITY gap: the keys follow the Rust datamodel's field order (with the
/// `SpecificationComponent` fields moved first, as pydantic has them), which is not
/// otherwise pydantic's, so the text can differ from upstream's for some specs.
fn dump_resolved_spec(spec: &TransformationSpecification) -> String {
    fn strip(v: Json) -> Option<Json> {
        match v {
            Json::Null => None,
            Json::Array(items) => {
                let items: Vec<Json> = items.into_iter().filter_map(strip).collect();
                (!items.is_empty()).then_some(Json::Array(items))
            }
            Json::Object(map) => {
                let map: Map<String, Json> = map
                    .into_iter()
                    .filter_map(|(k, v)| strip(v).map(|v| (k, v)))
                    .collect();
                (!map.is_empty()).then_some(Json::Object(map))
            }
            other => Some(other),
        }
    }
    /// pydantic lists the `SpecificationComponent` fields (`description`, `implements`,
    /// `comments`) before everything else, in every derivation and in the spec itself.
    fn components_first(v: Json) -> Json {
        match v {
            Json::Array(items) => Json::Array(items.into_iter().map(components_first).collect()),
            Json::Object(mut map) => {
                let mut out = Map::new();
                for key in ["description", "implements", "comments"] {
                    if let Some(value) = map.shift_remove(key) {
                        out.insert(key.to_owned(), components_first(value));
                    }
                }
                out.extend(map.into_iter().map(|(k, v)| (k, components_first(v))));
                Json::Object(out)
            }
            other => other,
        }
    }
    let json = serde_json::to_value(spec).unwrap_or(Json::Null);
    let json = strip(json).unwrap_or_else(|| Json::Object(Map::new()));
    let json = components_first(json);
    linkml_map_io::pyyaml::dump_yaml(
        &linkml_map_core::value::Value::from(&json),
        linkml_map_io::pyyaml::DumpOptions {
            sort_keys: false,
            ..Default::default()
        },
    )
}

/// `_merge_or_exit`: merge, reporting the expected failures cleanly (exit code 1).
fn merge_or_exit(paths: &[&str], err: &mut dyn Write) -> Result<Map<String, Json>, i32> {
    load_and_merge_specs(paths).map_err(|e| {
        let _ = writeln!(err, "{e}");
        1
    })
}

// ── spec_merge ───────────────────────────────────────────────────────────────

/// `spec_merge.resolve_spec_paths`: files as given, directories expanded (recursively)
/// to their sorted `*.yaml` / `*.yml` files.
fn resolve_spec_paths(paths: &[&str]) -> Result<Vec<PathBuf>, String> {
    let mut resolved = Vec::new();
    for p in paths {
        let path = PathBuf::from(p);
        if !path.exists() {
            return Err(format!("Spec path does not exist: {}", py_path_str(&path)));
        }
        if path.is_dir() {
            let mut found = Vec::new();
            collect_yaml_files(&path, &mut found);
            found.sort_by_key(|f| {
                f.components()
                    .filter(|c| !matches!(c, Component::CurDir))
                    .map(|c| c.as_os_str().to_owned())
                    .collect::<Vec<_>>()
            });
            resolved.extend(found);
        } else {
            resolved.push(path);
        }
    }
    Ok(resolved)
}

fn collect_yaml_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_yaml = path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "yaml" || e == "yml");
        // `Path.rglob` does not follow symlinked directories.
        let is_real_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        if is_real_dir {
            collect_yaml_files(&path, out);
        } else if is_yaml {
            out.push(path);
        }
    }
}

/// `spec_merge.load_spec_file`: a spec dict, or the dict items of a list of partial specs.
fn load_spec_file(path: &Path) -> anyhow::Result<Vec<Map<String, Json>>> {
    use anyhow::Context as _;
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("reading spec {}", path.display()))?;
    let data = load_yaml(&text).with_context(|| format!("parsing spec {}", path.display()))?;
    Ok(match data {
        Json::Object(map) => vec![map],
        Json::Array(items) => items
            .into_iter()
            .filter_map(|i| match i {
                Json::Object(m) => Some(m),
                _ => None,
            })
            .collect(),
        other => {
            log::warn!(
                "Skipping {}: expected dict or list, got {}",
                path.display(),
                py_type_name(&other)
            );
            Vec::new()
        }
    })
}

/// `spec_merge.load_and_merge_specs`.
fn load_and_merge_specs(paths: &[&str]) -> Result<Map<String, Json>, String> {
    let files = resolve_spec_paths(paths)?;
    if files.is_empty() {
        return Err("No YAML files found in the provided paths".to_owned());
    }
    let mut all = Vec::new();
    for file in &files {
        all.extend(load_spec_file(file).map_err(|e| format!("{e:#}"))?);
    }
    if all.is_empty() {
        return Err("No valid spec dicts found in the provided files".to_owned());
    }
    merge_spec_dicts(all)
}

/// Fields whose value changes what is emitted, so fragments must agree on them.
const MUST_AGREE_KEYS: [&str; 1] = ["mint_ids"];
const COLLECTION_KEYS: [&str; 4] = [
    "class_derivations",
    "class_defaults",
    "enum_derivations",
    "slot_derivations",
];

/// `spec_merge.merge_spec_dicts`: class derivations are appended, the other collections
/// are united by name (a conflicting duplicate is an error), scalar fields take the first
/// non-null value (`mint_ids` must agree).
fn merge_spec_dicts(specs: Vec<Map<String, Json>>) -> Result<Map<String, Json>, String> {
    let mut specs = specs;
    if specs.len() == 1 {
        return Ok(specs.remove(0));
    }
    let mut merged = Map::new();
    let mut class_derivations: Vec<Json> = Vec::new();
    let mut enum_derivations = Map::new();
    let mut class_defaults = Map::new();
    let mut slot_derivations = Map::new();

    fn union(into: &mut Map<String, Json>, from: &Json, what: &str) -> Result<(), String> {
        if let Json::Object(map) = from {
            for (name, body) in map {
                if into.get(name).is_some_and(|existing| existing != body) {
                    return Err(format!("Conflicting {what} for '{name}'"));
                }
                into.insert(name.clone(), body.clone());
            }
        }
        Ok(())
    }

    for spec in &specs {
        match spec.get("class_derivations") {
            Some(Json::Array(items)) => class_derivations.extend(items.iter().cloned()),
            Some(Json::Object(map)) => {
                for (name, body) in map {
                    let body = if body.is_null() {
                        Json::Object(Map::new())
                    } else {
                        body.clone()
                    };
                    class_derivations.push(Json::Object(Map::from_iter([(name.clone(), body)])));
                }
            }
            _ => {}
        }
        if let Some(v) = spec.get("class_defaults") {
            union(&mut class_defaults, v, "class_defaults")?;
        }
        if let Some(v) = spec.get("enum_derivations") {
            union(&mut enum_derivations, v, "enum_derivations")?;
        }
        if let Some(v) = spec.get("slot_derivations") {
            union(&mut slot_derivations, v, "slot_derivations")?;
        }
        for (key, value) in spec {
            if MUST_AGREE_KEYS.contains(&key.as_str())
                && !value.is_null()
                && merged.get(key).is_some_and(|m| m != value)
            {
                return Err(format!(
                    "Conflicting values for '{key}' across spec files: {} and {}",
                    py_repr(&merged[key.as_str()]),
                    py_repr(value)
                ));
            }
            if !COLLECTION_KEYS.contains(&key.as_str())
                && !merged.contains_key(key)
                && !value.is_null()
            {
                merged.insert(key.clone(), value.clone());
            }
        }
    }

    if !class_derivations.is_empty() {
        merged.insert("class_derivations".into(), Json::Array(class_derivations));
    }
    if !class_defaults.is_empty() {
        merged.insert("class_defaults".into(), Json::Object(class_defaults));
    }
    if !enum_derivations.is_empty() {
        merged.insert("enum_derivations".into(), Json::Object(enum_derivations));
    }
    if !slot_derivations.is_empty() {
        merged.insert("slot_derivations".into(), Json::Object(slot_derivations));
    }
    Ok(merged)
}

/// `spec_merge.class_derivation_names`, in encounter order.
fn class_derivation_names(spec: &Map<String, Json>) -> Vec<String> {
    match spec.get("class_derivations") {
        Some(Json::Array(items)) => items
            .iter()
            .filter_map(|item| {
                let o = item.as_object()?;
                if let Some(name) = o.get("name") {
                    Some(
                        name.as_str()
                            .map_or_else(|| name.to_string(), str::to_owned),
                    )
                } else if o.len() == 1 {
                    o.keys().next().cloned()
                } else {
                    None
                }
            })
            .collect(),
        Some(Json::Object(map)) => map.keys().cloned().collect(),
        _ => Vec::new(),
    }
}

// ── helpers ──────────────────────────────────────────────────────────────────

/// `yaml.safe_load` of a single document, as JSON-shaped data.
fn load_yaml(text: &str) -> anyhow::Result<Json> {
    Ok(
        match linkml_map_io::yaml_load::safe_load_all(text)?.as_slice() {
            [] => Json::Null,
            [doc] => Json::from(doc),
            _ => anyhow::bail!("expected a single document in the stream"),
        },
    )
}

/// Python `type(x).__name__` of a loaded YAML value.
fn py_type_name(v: &Json) -> &'static str {
    match v {
        Json::Null => "NoneType",
        Json::Bool(_) => "bool",
        Json::Number(n) if n.is_f64() => "float",
        Json::Number(_) => "int",
        Json::String(_) => "str",
        Json::Array(_) => "list",
        Json::Object(_) => "dict",
    }
}

/// A short Python `repr` of a scalar, for messages.
fn py_repr(v: &Json) -> String {
    match v {
        Json::Null => "None".to_owned(),
        Json::Bool(true) => "True".to_owned(),
        Json::Bool(false) => "False".to_owned(),
        Json::String(s) => format!("'{s}'"),
        other => other.to_string(),
    }
}

/// `str(pathlib.Path(p))`: no leading `./`, no repeated or trailing separators.
fn py_path_str(path: &Path) -> String {
    let parts: Vec<String> = path
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| match c {
            Component::RootDir => String::new(),
            other => other.as_os_str().to_string_lossy().into_owned(),
        })
        .collect();
    match parts.as_slice() {
        [] => ".".to_owned(),
        [root] if root.is_empty() => "/".to_owned(),
        _ => parts.join("/").replacen("//", "/", 1),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn obj(v: Json) -> Map<String, Json> {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn path_strings_follow_pathlib() {
        assert_eq!(py_path_str(Path::new("./a/b.yaml")), "a/b.yaml");
        assert_eq!(py_path_str(Path::new("a//b/")), "a/b");
        assert_eq!(py_path_str(Path::new("/x/y.yaml")), "/x/y.yaml");
        assert_eq!(py_path_str(Path::new(".")), ".");
    }

    /// `test_spec_merge.py`: class derivations accumulate, name-keyed collections unite,
    /// scalars take the first non-null value.
    #[test]
    fn merge_unions_by_name_and_first_scalar_wins() {
        let a = obj(
            json!({"title": "A", "class_derivations": {"X": {"populated_from": "x"}},
            "enum_derivations": {"E": {"populated_from": "e"}}}),
        );
        let b = obj(
            json!({"title": "B", "version": "2", "class_derivations": [{"name": "Y"}],
            "enum_derivations": {"E": {"populated_from": "e"}, "F": {}}}),
        );
        let m = merge_spec_dicts(vec![a, b]).unwrap();
        assert_eq!(m["title"], "A");
        assert_eq!(m["version"], "2");
        assert_eq!(
            m["class_derivations"],
            json!([{"X": {"populated_from": "x"}}, {"name": "Y"}])
        );
        assert_eq!(m["enum_derivations"].as_object().unwrap().len(), 2);
        assert_eq!(class_derivation_names(&m), ["X", "Y"]);
    }

    #[test]
    fn merge_rejects_conflicts() {
        let a = obj(json!({"enum_derivations": {"E": {"populated_from": "e"}}}));
        let b = obj(json!({"enum_derivations": {"E": {"populated_from": "other"}}}));
        assert_eq!(
            merge_spec_dicts(vec![a, b]).unwrap_err(),
            "Conflicting enum_derivations for 'E'"
        );
        let a = obj(json!({"mint_ids": true}));
        let b = obj(json!({"mint_ids": false}));
        assert_eq!(
            merge_spec_dicts(vec![a, b]).unwrap_err(),
            "Conflicting values for 'mint_ids' across spec files: True and False"
        );
    }

    #[test]
    fn list_entities_handles_the_three_shapes() {
        for spec in [
            json!({"class_derivations": {"B": {}, "A": {}}}),
            json!({"class_derivations": [{"name": "B"}, {"name": "A"}]}),
            json!({"class_derivations": [{"B": {}}, {"A": {}}]}),
        ] {
            assert_eq!(class_derivation_names(&obj(spec)), ["B", "A"]);
        }
    }
}
