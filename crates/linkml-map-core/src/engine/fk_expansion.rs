//! How far an [`super::ObjectIndex`] follows references below a bound value.
//!
//! Upstream's `ProxyObject` dereferences a foreign key on every attribute access, so
//! `{boss.boss.label}` reads the label of the boss's boss and `src.boss.friends` yields the
//! friends' objects. A [`crate::value::Value`] has no lazy reference, so the engine expands the
//! referenced objects eagerly instead, and only as far as the spec's expressions can reach: along
//! the attribute names they access, as deep as their longest chain of accesses.

use std::collections::HashSet;

use crate::datamodel::{ClassDerivation, SlotDerivation, TransformationSpecification};

/// Deepest chain of references followed; a longer chain in an expression is cut here.
const MAX_DEPTH: usize = 6;

/// The attribute names accessed (`x.<name>`) by the `expr:`s of `spec`, and the largest number of
/// accesses in any one of them.
pub(crate) fn expression_attribute_access(
    spec: &TransformationSpecification,
) -> (HashSet<String>, usize) {
    let mut names = HashSet::new();
    let mut depth = 0;
    for cd in spec.class_derivations.iter().flatten() {
        scan_class_derivation(cd, &mut names, &mut depth);
    }
    (names, depth.min(MAX_DEPTH))
}

fn scan_class_derivation(cd: &ClassDerivation, names: &mut HashSet<String>, depth: &mut usize) {
    for sd in cd.slot_derivations.iter().flat_map(|sds| sds.values()) {
        scan_slot_derivation(sd, names, depth);
    }
}

fn scan_slot_derivation(sd: &SlotDerivation, names: &mut HashSet<String>, depth: &mut usize) {
    let texts = sd.expr.as_deref().into_iter().chain(
        sd.expression_mappings
            .iter()
            .flat_map(|m| m.values())
            .filter_map(|kv| kv.value.as_ref()?.as_str()),
    );
    for text in texts {
        *depth = (*depth).max(scan_attribute_accesses(text, names));
    }
    for nested in sd.class_derivations.iter().flatten() {
        scan_class_derivation(nested, names, depth);
    }
}

/// Collect the names after a `.` that follows an operand (`a.b`, `f(x).b`, `a[0].b`), counting the
/// accesses. A decimal point (`1.5`) is not one; a `.` in a string literal is, harmlessly.
fn scan_attribute_accesses(text: &str, names: &mut HashSet<String>) -> usize {
    let bytes = text.as_bytes();
    let is_ident = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut count = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'.'
            && i > 0
            && (is_ident(bytes[i - 1]) || matches!(bytes[i - 1], b')' | b']' | b'}'))
            && bytes
                .get(i + 1)
                .is_some_and(|&b| b.is_ascii_alphabetic() || b == b'_')
        {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && is_ident(bytes[end]) {
                end += 1;
            }
            names.insert(text[start..end].to_owned());
            count += 1;
            i = end;
        } else {
            i += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_attribute_accesses_and_skips_decimal_points() {
        let mut names = HashSet::new();
        assert_eq!(
            scan_attribute_accesses("{boss.boss.label} + 1.5", &mut names),
            2
        );
        assert_eq!(
            names,
            HashSet::from(["boss".to_owned(), "label".to_owned()])
        );
        let mut names = HashSet::new();
        assert_eq!(
            scan_attribute_accesses("[f.label for f in src.boss.friends]", &mut names),
            3
        );
        assert!(names.contains("friends"));
        assert_eq!(
            scan_attribute_accesses("{age} * 12", &mut HashSet::new()),
            0
        );
    }
}
