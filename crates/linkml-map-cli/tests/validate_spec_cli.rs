//! `validate-spec`: ports of upstream `tests/test_cli/test_cli_validate.py` and the
//! `validate-spec` / `--list-entities` tests of `test_cli_multi_spec.py` (v0.6.0-rc1).
//!
//! The command is exercised through [`run_validate_spec`] with in-memory stdout / stderr
//! (upstream's `CliRunner`), plus one subprocess test for the real binary's exit status.

use std::path::{Path, PathBuf};
use std::process::Command;

use linkml_map_cli::validate_spec::{ValidateSpecOptions, run_validate_spec};

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/examples")
}

fn flattening_tr() -> String {
    s(examples().join("flattening/transform/denormalize.transform.yaml"))
}

fn personinfo_tr() -> String {
    s(examples().join("personinfo_basic/transform/personinfo-to-agent.transform.yaml"))
}

fn personinfo_schema() -> String {
    s(examples().join("personinfo_basic/source/personinfo.yaml"))
}

fn s(p: impl AsRef<Path>) -> String {
    p.as_ref().to_str().unwrap().to_owned()
}

struct Outcome {
    code: i32,
    out: String,
    err: String,
}

fn run(opts: ValidateSpecOptions) -> Outcome {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = run_validate_spec(&opts, &mut out, &mut err);
    Outcome {
        code,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn files(paths: &[&str]) -> ValidateSpecOptions {
    ValidateSpecOptions {
        spec_files: paths.iter().map(|p| (*p).to_owned()).collect(),
        ..Default::default()
    }
}

fn write(dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::write(&path, text).unwrap();
    s(path)
}

const BOGUS_EXPR_SPEC: &str = "class_derivations:\n  Person:\n    populated_from: Person\n    slot_derivations:\n      primary_email:\n        expr: \"bogus_slot\"\n";

/// `test_validate_spec_valid_file`: a valid spec exits 0 and prints `<path>: ok` on stdout.
#[test]
fn valid_file_prints_ok_on_stdout() {
    let tr = flattening_tr();
    let r = run(files(&[&tr]));
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(r.out, format!("{tr}: ok\n"));
    assert_eq!(r.err, "");
}

/// `test_validate_spec_multiple_files`: every file gets its own line.
#[test]
fn multiple_files_each_report() {
    let (a, b) = (flattening_tr(), personinfo_tr());
    let r = run(ValidateSpecOptions {
        no_warnings: true,
        ..files(&[&a, &b])
    });
    assert_eq!(r.code, 0, "{}", r.err);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(
        lines
            .iter()
            .all(|l| l.ends_with(": ok") || l.ends_with(": ok (with warnings)"))
    );
}

/// `test_validate_spec_invalid_file` / `_non_mapping` / `_mixed_valid_invalid`.
#[test]
fn invalid_files_report_errors_on_stderr_and_exit_1() {
    let tmp = tempfile::tempdir().unwrap();
    let bad = write(tmp.path(), "bad.transform.yaml", "not_a_real_field: oops\n");
    let r = run(files(&[&bad]));
    assert_eq!(r.code, 1);
    assert!(
        r.err.starts_with(&format!("{bad}:\n  $: [error] ")),
        "{}",
        r.err
    );
    assert!(r.err.contains("not_a_real_field"), "{}", r.err);
    assert_eq!(r.out, "");

    let list = write(tmp.path(), "list.yaml", "- item\n");
    let r = run(files(&[&list]));
    assert_eq!(r.code, 1);
    assert!(
        r.err
            .contains("$: [error] Expected a YAML mapping at top level, got list"),
        "{}",
        r.err
    );

    let tr = flattening_tr();
    let bogus = write(tmp.path(), "bogus.yaml", "bogus_field: 123\n");
    let r = run(files(&[&tr, &bogus]));
    assert_eq!(r.code, 1);
    assert!(r.out.lines().any(|l| l.ends_with(": ok")), "{}", r.out);
    assert!(r.err.contains("bogus_field"), "{}", r.err);
}

/// `test_validate_spec_with_schemas`.
#[test]
fn explicit_schemas_validate() {
    let tr = flattening_tr();
    let r = run(ValidateSpecOptions {
        source_schema: Some(s(examples().join("flattening/source/normalized.yaml"))),
        target_schema: Some(s(examples().join("flattening/target/denormalized.yaml"))),
        ..files(&[&tr])
    });
    assert_eq!(r.code, 0, "{}", r.err);
    assert!(r.out.contains("ok"));
}

/// `test_validate_spec_semantic_errors`.
#[test]
fn semantic_errors_name_the_missing_class() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = write(
        tmp.path(),
        "bad_ref.yaml",
        "class_derivations:\n  NonExistentClass:\n    populated_from: Person\n",
    );
    let r = run(ValidateSpecOptions {
        target_schema: Some(personinfo_schema()),
        ..files(&[&spec])
    });
    assert_eq!(r.code, 1);
    assert!(
        r.err.contains(
            "class_derivations[NonExistentClass]: [error] Target class 'NonExistentClass' not found in target schema"
        ),
        "{}",
        r.err
    );
}

/// `test_validate_spec_strict_flag` and `test_validate_spec_no_warnings`: a warning prints
/// under a header, ends `ok (with warnings)`, fails under `--strict`, and is hidden by
/// `--no-warnings`.
#[test]
fn warnings_strict_and_no_warnings() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = write(tmp.path(), "expr_warn.yaml", BOGUS_EXPR_SPEC);
    let with = |strict, no_warnings| {
        run(ValidateSpecOptions {
            source_schema: Some(personinfo_schema()),
            strict,
            no_warnings,
            ..files(&[&spec])
        })
    };

    let r = with(false, false);
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(r.out, format!("{spec}: ok (with warnings)\n"));
    assert!(r.err.starts_with(&format!("{spec}:\n  ")), "{}", r.err);
    assert!(
        r.err.contains("bogus_slot") && r.err.contains("[warning]"),
        "{}",
        r.err
    );

    let r = with(true, false);
    assert_eq!(r.code, 1);
    assert!(
        r.err.contains("bogus_slot") && r.err.contains("[error]"),
        "{}",
        r.err
    );

    let r = with(false, true);
    assert_eq!(r.code, 0);
    assert!(!r.err.contains("bogus_slot"), "{}", r.err);
    assert_eq!(r.out, format!("{spec}: ok (with warnings)\n"));

    // Warnings still fail `--strict --no-warnings` (upstream: `strict and has_warnings`).
    let r = with(true, true);
    assert_eq!(r.code, 1);
}

/// `spec_scan`: deprecated fields are warnings of category `deprecated`, even without a schema.
#[test]
fn deprecated_fields_warn() {
    let tmp = tempfile::tempdir().unwrap();
    let spec = write(
        tmp.path(),
        "old.yaml",
        "source_schema: nowhere.yaml\nclass_derivations:\n  A:\n    slot_derivations:\n      s:\n        derived_from: [t]\n",
    );
    let r = run(files(&[&spec]));
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(r.out, format!("{spec}: ok (with warnings)\n"));
    assert!(
        r.err
            .contains("$.source_schema: [warning] 'source_schema' is set to a bare string"),
        "{}",
        r.err
    );
    assert!(
        r.err.contains("1 SlotDerivation(s) use 'derived_from'"),
        "{}",
        r.err
    );
}

fn split_specs(root: &Path) -> String {
    let dir = root.join("specs");
    std::fs::create_dir_all(&dir).unwrap();
    write(
        &dir,
        "person.yaml",
        "class_derivations:\n  Person:\n    populated_from: Person\n    slot_derivations:\n      id: {}\n      name: {}\n",
    );
    write(
        &dir,
        "org.yaml",
        "class_derivations:\n  Org:\n    populated_from: Org\n    slot_derivations:\n      id: {}\n      title: {}\n",
    );
    s(dir)
}

/// `test_validate_spec_merge`, `_merge_emit_to_file`, `_merge_emit_stdout`, `_merge_emit_entity`.
#[test]
fn merge_validates_and_emits() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = split_specs(tmp.path());
    let (person, org) = (format!("{dir}/person.yaml"), format!("{dir}/org.yaml"));

    let r = run(ValidateSpecOptions {
        merge: true,
        ..files(&[&person, &org])
    });
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(r.out, "Merged spec: ok\n");

    let emitted = s(tmp.path().join("resolved.yaml"));
    let r = run(ValidateSpecOptions {
        merge: true,
        emit_spec: Some(emitted.clone()),
        ..files(&[&person, &org])
    });
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(
        r.out,
        format!("Merged spec: ok\nWrote resolved spec to {emitted}\n")
    );
    let text = std::fs::read_to_string(&emitted).unwrap();
    assert!(
        text.starts_with("class_derivations:\n- name: Person\n"),
        "{text}"
    );
    assert!(text.contains("- name: Org\n"), "{text}");

    let r = run(ValidateSpecOptions {
        merge: true,
        emit_spec: Some("-".into()),
        ..files(&[&person, &org])
    });
    assert_eq!(r.code, 0, "{}", r.err);
    let after_ok = r.out.split_once("Merged spec: ok\n").unwrap().1;
    assert!(after_ok.starts_with("class_derivations:\n"), "{after_ok}");

    let filtered = s(tmp.path().join("one.yaml"));
    let r = run(ValidateSpecOptions {
        merge: true,
        entity: Some("Person".into()),
        emit_spec: Some(filtered.clone()),
        ..files(&[&person, &org])
    });
    assert_eq!(r.code, 0, "{}", r.err);
    let text = std::fs::read_to_string(&filtered).unwrap();
    assert!(
        text.contains("name: Person") && !text.contains("Org"),
        "{text}"
    );
}

/// `test_validate_spec_entity_without_merge_errors`.
#[test]
fn entity_without_merge_is_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = split_specs(tmp.path());
    let person = format!("{dir}/person.yaml");
    let r = run(ValidateSpecOptions {
        entity: Some("Person".into()),
        ..files(&[&person])
    });
    assert_eq!(r.code, 1);
    assert_eq!(r.err, "--entity and --emit-spec require --merge\n");
}

/// `test_list_entities_prints_sorted_names`, `_multiple_files`, `_empty_dir_reports_cleanly`,
/// `test_merge_conflict_reports_cleanly`.
#[test]
fn list_entities_and_merge_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = split_specs(tmp.path());
    let opts = ValidateSpecOptions {
        list_entities: true,
        ..files(&[&dir])
    };
    let r = run(opts);
    assert_eq!((r.code, r.out.as_str()), (0, "Org\nPerson\n"));

    let (person, org) = (format!("{dir}/person.yaml"), format!("{dir}/org.yaml"));
    let r = run(ValidateSpecOptions {
        list_entities: true,
        ..files(&[&person, &org])
    });
    assert_eq!((r.code, r.out.as_str()), (0, "Org\nPerson\n"));

    let empty = tmp.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let r = run(ValidateSpecOptions {
        list_entities: true,
        ..files(&[&s(&empty)])
    });
    assert_eq!(r.code, 1);
    assert!(r.err.contains("No YAML files"), "{}", r.err);

    let conflict = tmp.path().join("conflict");
    std::fs::create_dir_all(&conflict).unwrap();
    write(
        &conflict,
        "a.yaml",
        "slot_derivations:\n  s1:\n    populated_from: x\n",
    );
    write(
        &conflict,
        "b.yaml",
        "slot_derivations:\n  s1:\n    populated_from: y\n",
    );
    let r = run(ValidateSpecOptions {
        merge: true,
        ..files(&[&s(&conflict)])
    });
    assert_eq!(r.code, 1);
    assert!(
        r.err.contains("Conflicting slot_derivations for 's1'"),
        "{}",
        r.err
    );
}

/// A directory is expanded recursively to its sorted `.yaml` / `.yml` files, one report
/// each; with no YAML at all the message is upstream's.
#[test]
fn directories_expand_to_their_yaml_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("specs");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    let b = write(&dir, "b.yaml", "class_derivations: {}\n");
    let a = write(&dir.join("sub"), "a.yml", "class_derivations: {}\n");
    write(&dir, "notes.txt", "ignored");
    let r = run(files(&[&s(&dir)]));
    assert_eq!(r.code, 0, "{}", r.err);
    assert_eq!(r.out, format!("{b}: ok\n{a}: ok\n"));
}

/// The binary: stdout carries only the `ok` lines, the exit status is upstream's, and a
/// path that does not exist is a usage error (click's exit status 2).
#[test]
fn binary_streams_and_exit_codes() {
    let tr = flattening_tr();
    let bin = env!("CARGO_BIN_EXE_linkml-tr-rs");
    let out = Command::new(bin)
        .args(["validate-spec", &tr])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{tr}: ok\n"));

    let tmp = tempfile::tempdir().unwrap();
    let bad = write(tmp.path(), "bad.yaml", "bogus: 1\n");
    let out = Command::new(bin)
        .args(["validate-spec", &bad])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(out.stdout.is_empty());
    assert!(String::from_utf8_lossy(&out.stderr).starts_with(&format!("{bad}:\n")));

    let out = Command::new(bin)
        .args(["validate-spec", "/definitely/not/here.yaml"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
}
