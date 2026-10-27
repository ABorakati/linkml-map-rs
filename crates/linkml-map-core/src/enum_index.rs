//! Resolve the spellings a spec uses for an enum value to the enum's
//! permissible value keys.
//!
//! Port of upstream `linkml_map.utils.enum_index`. Spellings are matched in
//! four tiers: the key, then `meaning` (compared as expanded URIs), then
//! `title`, then `aliases`. The first tier with any match decides, so a value
//! that is already a key always resolves to itself, even when another
//! permissible value uses it as a title.
//!
//! Upstream's `EnumIndex` stores the schema's `expand_curie` as a callable.
//! The index here is built once per plan and shared across threads, so it
//! cannot borrow the schema; the schema is passed to [`EnumIndex::match_value`]
//! and [`EnumIndex::resolve`] instead.
//!
//! PARITY: Python's `SchemaView.expand_curie` expands exactly-two-part `a:b`
//! strings; the `curies` crate behind the schemaview provider may also expand
//! `a:b:c` when `a` is a known prefix. Negligible in practice.

use std::collections::{BTreeSet, HashMap};

use crate::pyfmt::py_repr_str;
use crate::schema::{EnumDef, SchemaProvider};

/// Lookup from each spelling of an enum's permissible values to their keys.
#[derive(Debug, Clone)]
pub struct EnumIndex {
    pub enum_name: String,
    /// `[key, expanded meaning, title, alias]`, each spelling to the set of
    /// keys that spell it (sorted, so `sorted(keys)` is free).
    tiers: [HashMap<String, BTreeSet<String>>; 4],
}

/// Expand a CURIE to a URI, returning anything else unchanged
/// (upstream `sv.expand_curie`).
fn expand(sv: &dyn SchemaProvider, value: &str) -> String {
    sv.expand_curie(value).unwrap_or_else(|| value.to_owned())
}

impl EnumIndex {
    /// Build the index for `enum_def`. `sv` expands `meaning` CURIEs.
    pub fn from_enum(enum_def: &EnumDef, sv: &dyn SchemaProvider) -> Self {
        let mut tiers: [HashMap<String, BTreeSet<String>>; 4] = Default::default();
        for pv in &enum_def.permissible_values {
            let key = &pv.text;
            tiers[0].entry(key.clone()).or_default().insert(key.clone());
            if let Some(meaning) = pv.meaning.as_deref().filter(|m| !m.is_empty()) {
                tiers[1]
                    .entry(expand(sv, meaning))
                    .or_default()
                    .insert(key.clone());
            }
            if let Some(title) = pv.title.as_deref().filter(|t| !t.is_empty()) {
                tiers[2]
                    .entry(title.to_owned())
                    .or_default()
                    .insert(key.clone());
            }
            for alias in &pv.aliases {
                tiers[3]
                    .entry(alias.clone())
                    .or_default()
                    .insert(key.clone());
            }
        }
        Self {
            enum_name: enum_def.name.clone(),
            tiers,
        }
    }

    /// The keys `value` resolves to: those of the first tier with any match;
    /// empty when nothing matches, more than one when the value is ambiguous
    /// in that tier.
    pub fn match_value(&self, value: &str, sv: &dyn SchemaProvider) -> BTreeSet<String> {
        let expanded = expand(sv, value);
        let spellings = [value, expanded.as_str(), value, value];
        for (tier, spelling) in self.tiers.iter().zip(spellings) {
            if let Some(keys) = tier.get(spelling) {
                return keys.clone();
            }
        }
        BTreeSet::new()
    }

    /// The key `value` resolves to, or why it does not resolve to exactly one.
    ///
    /// The problem strings are upstream's:
    /// `'red' matches more than one permissible value of enum Color: ['R', 'r']`
    /// and `'blue' is not a permissible value of enum Color`.
    pub fn resolve(&self, value: &str, sv: &dyn SchemaProvider) -> Result<String, String> {
        let keys = self.match_value(value, sv);
        match keys.len() {
            1 => Ok(keys.into_iter().next().unwrap_or_default()),
            0 => Err(format!(
                "{} is not a permissible value of enum {}",
                py_repr_str(value),
                self.enum_name
            )),
            _ => {
                let listed: Vec<String> = keys.iter().map(|k| py_repr_str(k)).collect();
                Err(format!(
                    "{} matches more than one permissible value of enum {}: [{}]",
                    py_repr_str(value),
                    self.enum_name,
                    listed.join(", ")
                ))
            }
        }
    }
}

/// Build the index for a schema's enum, or `None` if its values aren't all
/// listed.
///
/// Enums defined partly or wholly by a query or by other enums
/// (`reachable_from`, `inherits` and the like) have no closed list of
/// permissible values to resolve against, so their values pass through
/// unchanged; so do enums with no permissible values or that are unknown.
pub fn enum_index(sv: &dyn SchemaProvider, enum_name: &str) -> Option<EnumIndex> {
    let enum_def = sv.get_enum(enum_name).ok()?;
    if enum_def.permissible_values.is_empty() || enum_def.is_dynamic {
        return None;
    }
    Some(EnumIndex::from_enum(&enum_def, sv))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{InMemorySchemaBuilder, PermissibleValue};

    fn pv(
        text: &str,
        title: Option<&str>,
        meaning: Option<&str>,
        aliases: &[&str],
    ) -> PermissibleValue {
        PermissibleValue {
            text: text.into(),
            title: title.map(Into::into),
            meaning: meaning.map(Into::into),
            aliases: aliases.iter().map(|a| (*a).into()).collect(),
            ..Default::default()
        }
    }

    fn schema(enum_def: EnumDef) -> crate::schema::InMemorySchema {
        InMemorySchemaBuilder::new().add_enum(enum_def).build()
    }

    // enum_index.py::EnumIndex.match doctest
    #[test]
    fn match_doctest() {
        let e = EnumDef {
            name: "SexEnum".into(),
            permissible_values: vec![
                pv("OMOP:8532", Some("FEMALE"), Some("OMOP:8532"), &[]),
                pv("OMOP:8507", Some("MALE"), Some("OMOP:8507"), &[]),
                pv("MALE", Some("male (legacy)"), None, &[]),
            ],
            ..Default::default()
        };
        let sv = schema(e.clone());
        let index = EnumIndex::from_enum(&e, &sv);
        let m = |v: &str| index.match_value(v, &sv).into_iter().collect::<Vec<_>>();
        assert_eq!(m("FEMALE"), ["OMOP:8532"]);
        // The key wins over another value's title.
        assert_eq!(m("MALE"), ["MALE"]);
        assert!(m("unknown").is_empty());
    }

    // enum_index.py::EnumIndex.resolve doctest
    #[test]
    fn resolve_doctest() {
        let e = EnumDef {
            name: "Color".into(),
            permissible_values: vec![
                pv("R", Some("red"), None, &[]),
                pv("r", Some("red"), None, &[]),
            ],
            ..Default::default()
        };
        let sv = schema(e.clone());
        let index = EnumIndex::from_enum(&e, &sv);
        assert_eq!(index.resolve("R", &sv), Ok("R".to_owned()));
        assert_eq!(
            index.resolve("red", &sv),
            Err(
                "'red' matches more than one permissible value of enum Color: ['R', 'r']"
                    .to_owned()
            )
        );
        assert_eq!(
            index.resolve("blue", &sv),
            Err("'blue' is not a permissible value of enum Color".to_owned())
        );
    }

    #[test]
    fn tiers_are_key_meaning_title_alias() {
        let e = EnumDef {
            name: "E".into(),
            permissible_values: vec![
                pv("A", Some("alpha"), Some("ex:A"), &["first", "one"]),
                pv("B", None, None, &["alpha"]),
            ],
            ..Default::default()
        };
        let sv = schema(e.clone());
        let index = EnumIndex::from_enum(&e, &sv);
        let r = |v: &str| index.resolve(v, &sv);
        assert_eq!(r("A"), Ok("A".into()));
        assert_eq!(r("ex:A"), Ok("A".into()));
        // `alpha` is A's title (tier 2) and B's alias (tier 3): the title wins.
        assert_eq!(r("alpha"), Ok("A".into()));
        assert_eq!(r("one"), Ok("A".into()));
        assert!(r("Alpha").is_err());
    }

    #[test]
    fn problem_strings_use_python_repr() {
        let e = EnumDef {
            name: "E".into(),
            permissible_values: vec![pv("A", None, None, &[])],
            ..Default::default()
        };
        let sv = schema(e.clone());
        let index = EnumIndex::from_enum(&e, &sv);
        assert_eq!(
            index.resolve("it's", &sv),
            Err("\"it's\" is not a permissible value of enum E".to_owned())
        );
    }

    #[test]
    fn open_and_empty_enums_have_no_index() {
        let open = EnumDef {
            name: "Open".into(),
            permissible_values: vec![pv("A", None, None, &[])],
            is_dynamic: true,
        };
        let empty = EnumDef {
            name: "Empty".into(),
            ..Default::default()
        };
        let closed = EnumDef {
            name: "Closed".into(),
            permissible_values: vec![pv("A", None, None, &[])],
            is_dynamic: false,
        };
        let sv = InMemorySchemaBuilder::new()
            .add_enum(open)
            .add_enum(empty)
            .add_enum(closed)
            .build();
        assert!(enum_index(&sv, "Open").is_none());
        assert!(enum_index(&sv, "Empty").is_none());
        assert!(enum_index(&sv, "Missing").is_none());
        assert!(enum_index(&sv, "Closed").is_some());
    }
}
