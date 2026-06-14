//! Integration tests for `SchemaViewProvider`.
//!
//! Uses `tests/data/simple_enum.yaml` (no imports — fully self-contained) as
//! the primary fixture.  The schema looks like:
//!
//! ```yaml
//! id: https://example.org/simple-enum
//! name: simple-enum
//! classes:
//!   Asset:
//!     attributes:
//!       id: { range: string }
//!   Signal:
//!     is_a: Asset
//!     attributes:
//!       signalType: { range: SignalTypes }
//! enums:
//!   SignalTypes:
//!     permissible_values:
//!       A: { description: example }
//! ```

use linkml_map_core::schema::{RangeKind, SchemaError, SchemaProvider};
use linkml_map_schemaview::SchemaViewProvider;

fn simple_enum_path() -> std::path::PathBuf {
    // CARGO_MANIFEST_DIR is set at compile time to the crate directory:
    //   C:\Users\abora\linkml-map-rs\crates\linkml-map-schemaview
    // Three levels up lands at C:\Users\abora, then into LinkML-MCP.
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("../../../LinkML-MCP/rust/src/schemaview/tests/data/simple_enum.yaml")
}

fn load_simple_enum() -> SchemaViewProvider {
    let path = simple_enum_path();
    SchemaViewProvider::load_from_path(&path)
        .unwrap_or_else(|e| panic!("failed to load simple_enum.yaml at {}: {e}", path.display()))
}

// ── Inline YAML (no file I/O needed for basic checks) ────────────────────────

const INLINE_SCHEMA: &str = r#"
id: https://example.org/test-inline
name: test-inline
prefixes:
  ex: https://example.org/
default_prefix: ex
classes:
  Container:
    tree_root: true
    attributes:
      id:
        identifier: true
        range: string
      label:
        range: string
        required: true
      count:
        range: integer
        multivalued: false
      tags:
        range: string
        multivalued: true
      status:
        range: StatusEnum
      address:
        range: Address
  Address:
    attributes:
      street:
        range: string
enums:
  StatusEnum:
    permissible_values:
      active:
        description: Currently active
        meaning: ex:Active
      inactive:
        description: No longer active
"#;

fn load_inline() -> SchemaViewProvider {
    SchemaViewProvider::from_yaml_str(INLINE_SCHEMA)
        .unwrap_or_else(|e| panic!("failed to parse inline schema: {e}"))
}

// ── Tests: file-based (simple_enum.yaml) ─────────────────────────────────────

#[test]
fn file_load_succeeds() {
    let _p = load_simple_enum();
}

#[test]
fn file_all_class_names_contains_asset_and_signal() {
    let p = load_simple_enum();
    let mut names = p.all_class_names();
    names.sort();
    assert!(
        names.contains(&"Asset".to_owned()),
        "expected Asset, got {names:?}"
    );
    assert!(
        names.contains(&"Signal".to_owned()),
        "expected Signal, got {names:?}"
    );
}

#[test]
fn file_get_class_asset() {
    let p = load_simple_enum();
    let cls = p.get_class("Asset").unwrap();
    assert_eq!(cls.name, "Asset");
    assert!(cls.is_a.is_none());
    assert!(cls.mixins.is_empty());
}

#[test]
fn file_get_class_signal_is_a_asset() {
    let p = load_simple_enum();
    let cls = p.get_class("Signal").unwrap();
    assert_eq!(cls.name, "Signal");
    assert_eq!(cls.is_a.as_deref(), Some("Asset"));
}

#[test]
fn file_get_class_unknown_is_err() {
    let p = load_simple_enum();
    let err = p.get_class("NoSuchClass").unwrap_err();
    assert!(
        matches!(err, SchemaError::ClassNotFound(_)),
        "expected ClassNotFound, got {err:?}"
    );
}

#[test]
fn file_asset_induced_slots_contains_id() {
    let p = load_simple_enum();
    let slots = p.induced_slots("Asset").unwrap();
    let names: Vec<&str> = slots.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"id"), "expected 'id' slot, got {names:?}");
    let id_slot = slots.iter().find(|s| s.name == "id").unwrap();
    // range: string  →  RangeKind::Type("string")
    assert_eq!(id_slot.range, RangeKind::Type("string".into()));
    assert!(!id_slot.multivalued);
}

#[test]
fn file_signal_inherits_id_slot_from_asset() {
    let p = load_simple_enum();
    let slots = p.induced_slots("Signal").unwrap();
    let names: Vec<&str> = slots.iter().map(|s| s.name.as_str()).collect();
    // Signal is_a Asset, so must inherit the 'id' slot
    assert!(
        names.contains(&"id"),
        "Signal should inherit 'id' from Asset; got {names:?}"
    );
}

#[test]
fn file_signal_slot_signal_type_range_is_enum() {
    let p = load_simple_enum();
    let slots = p.induced_slots("Signal").unwrap();
    let st = slots.iter().find(|s| s.name == "signalType").unwrap();
    assert_eq!(st.range, RangeKind::Enum("SignalTypes".into()));
}

#[test]
fn file_get_enum_signal_types() {
    let p = load_simple_enum();
    let e = p.get_enum("SignalTypes").unwrap();
    assert_eq!(e.name, "SignalTypes");
    assert_eq!(e.permissible_values.len(), 1);
    assert_eq!(e.permissible_values[0].text, "A");
    assert_eq!(
        e.permissible_values[0].description.as_deref(),
        Some("example")
    );
}

#[test]
fn file_all_enum_names() {
    let p = load_simple_enum();
    let enums = p.all_enum_names();
    assert!(
        enums.contains(&"SignalTypes".to_owned()),
        "expected SignalTypes, got {enums:?}"
    );
}

#[test]
fn file_get_enum_unknown_is_err() {
    let p = load_simple_enum();
    let err = p.get_enum("NoSuchEnum").unwrap_err();
    assert!(matches!(err, SchemaError::EnumNotFound(_)));
}

// ── Tests: inline YAML ────────────────────────────────────────────────────────

#[test]
fn inline_load_succeeds() {
    let _p = load_inline();
}

#[test]
fn inline_all_class_names() {
    let p = load_inline();
    let mut names = p.all_class_names();
    names.sort();
    assert!(names.contains(&"Container".to_owned()));
    assert!(names.contains(&"Address".to_owned()));
}

#[test]
fn inline_tree_root_class() {
    let p = load_inline();
    let root = p.tree_root_class();
    assert_eq!(root, Some("Container".to_owned()));
}

#[test]
fn inline_identifier_slot() {
    let p = load_inline();
    let slot = p.identifier_slot("Container").unwrap().unwrap();
    assert_eq!(slot.name, "id");
    assert!(slot.identifier);
}

#[test]
fn inline_identifier_slot_none_for_address() {
    let p = load_inline();
    let result = p.identifier_slot("Address").unwrap();
    assert!(result.is_none());
}

#[test]
fn inline_induced_slot_range_types() {
    let p = load_inline();

    let label = p.induced_slot("label", "Container").unwrap();
    assert_eq!(label.range, RangeKind::Type("string".into()));
    assert!(label.required);
    assert!(!label.multivalued);

    let count = p.induced_slot("count", "Container").unwrap();
    assert_eq!(count.range, RangeKind::Type("integer".into()));
    assert!(!count.multivalued);

    let tags = p.induced_slot("tags", "Container").unwrap();
    assert!(tags.multivalued);

    let status = p.induced_slot("status", "Container").unwrap();
    assert_eq!(status.range, RangeKind::Enum("StatusEnum".into()));

    let address = p.induced_slot("address", "Container").unwrap();
    assert_eq!(address.range, RangeKind::Class("Address".into()));
}

#[test]
fn inline_induced_slot_unknown_slot_is_err() {
    let p = load_inline();
    let err = p.induced_slot("nonexistent", "Container").unwrap_err();
    assert!(matches!(err, SchemaError::SlotNotFound { .. }));
}

#[test]
fn inline_get_enum_with_pvs() {
    let p = load_inline();
    let e = p.get_enum("StatusEnum").unwrap();
    assert_eq!(e.name, "StatusEnum");
    assert_eq!(e.permissible_values.len(), 2);
    let texts: Vec<&str> = e
        .permissible_values
        .iter()
        .map(|p| p.text.as_str())
        .collect();
    assert!(texts.contains(&"active"));
    assert!(texts.contains(&"inactive"));

    let active = e
        .permissible_values
        .iter()
        .find(|pv| pv.text == "active")
        .unwrap();
    assert_eq!(active.description.as_deref(), Some("Currently active"));
    assert_eq!(active.meaning.as_deref(), Some("ex:Active"));
}

#[test]
fn inline_all_enum_names() {
    let p = load_inline();
    let names = p.all_enum_names();
    assert!(names.contains(&"StatusEnum".to_owned()));
}

#[test]
fn inline_trait_object_dispatch() {
    let p = load_inline();
    let provider: &dyn SchemaProvider = &p;
    assert!(provider.get_class("Container").is_ok());
    assert_eq!(provider.tree_root_class(), Some("Container".to_owned()));
}
