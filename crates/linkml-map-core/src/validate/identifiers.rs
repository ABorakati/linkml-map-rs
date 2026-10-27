//! Identifier-slot checks of the semantic validator (upstream `76db207`, #342).
//!
//! Ports `validator._class_default_mint_ids`, `_with_inherited_derivations`, the
//! `class_defaults` check at the head of `validate_spec_semantics` and the
//! "target class has required slots with no derivation" block of
//! `_validate_class_derivation`, which now also reports identifier slots that a
//! class derivation neither derives nor mints.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::{Ctx, Severity, ValidationMessage};
use crate::datamodel::ClassDerivation;
use crate::engine::content_id::{mints_ids, missing_identifier_message};
use crate::schema::SchemaProvider;

/// `mint_ids` per target class from the spec's `class_defaults`
/// (`_class_default_mint_ids`).
fn class_default_mint_ids<'a>(ctx: &Ctx<'a>) -> Vec<(&'a str, Option<bool>)> {
    ctx.spec
        .class_defaults
        .iter()
        .flatten()
        .map(|(name, d)| (name.as_str(), d.mint_ids))
        .collect()
}

/// Error for every `class_defaults` entry naming a class the target schema
/// lacks. Only when a target schema is given; emitted before every other
/// message, as upstream.
pub(super) fn check_class_defaults(ctx: &Ctx, messages: &mut Vec<ValidationMessage>) {
    if ctx.target.is_none() {
        return;
    }
    for (name, _) in class_default_mint_ids(ctx) {
        if !ctx.target_classes.contains(name) {
            messages.push(ValidationMessage::new(
                Severity::Error,
                format!("class_defaults[{name}]"),
                format!("class_defaults names '{name}', which is not a class in the target schema"),
            ));
        }
    }
}

/// Slot-derivation names and `mint_ids` of `cd`, including what it inherits
/// (`_with_inherited_derivations`).
///
/// Mirrors the runtime's ancestor merge: `mixins` then `is_a`, depth-first,
/// with `cd`'s own `mint_ids` taking precedence over the first ancestor that
/// sets one. Parents that are not top-level class derivations (e.g. target
/// schema classes) contribute nothing.
fn with_inherited_derivations(
    cd: &ClassDerivation,
    pool: &BTreeMap<&str, &ClassDerivation>,
) -> (BTreeSet<String>, Option<bool>) {
    fn visit(
        node: &ClassDerivation,
        pool: &BTreeMap<&str, &ClassDerivation>,
        seen: &mut HashSet<*const ClassDerivation>,
        names: &mut BTreeSet<String>,
        mint: &mut Option<bool>,
    ) {
        if !seen.insert(std::ptr::from_ref(node)) {
            return;
        }
        names.extend(node.slot_derivations.iter().flatten().map(|(_, sd)| sd.name.clone()));
        if mint.is_none() {
            *mint = node.mint_ids;
        }
        let parents = node.mixins.iter().flatten().chain(node.is_a.iter());
        for parent in parents {
            if let Some(parent_cd) = pool.get(parent.as_str()) {
                visit(parent_cd, pool, seen, names, mint);
            }
        }
    }

    let mut names = BTreeSet::new();
    let mut mint = None;
    visit(cd, pool, &mut HashSet::new(), &mut names, &mut mint);
    (names, mint)
}

/// Report target slots of `cd`'s class that no derivation (own or inherited)
/// covers: identifier slots as info (minted) or error (not minted), other
/// required slots as a warning.
pub(super) fn check_underived_slots(
    ctx: &Ctx,
    target: &dyn SchemaProvider,
    cd: &ClassDerivation,
    cd_path: &str,
    messages: &mut Vec<ValidationMessage>,
) {
    let cd_name = cd.name.as_str();
    // Top-level class derivations by name (`_collect_class_derivation_pool`).
    let pool: BTreeMap<&str, &ClassDerivation> = ctx
        .spec
        .class_derivations
        .iter()
        .flatten()
        .map(|c| (c.name.as_str(), c))
        .collect();
    let (derived, class_mint_ids) = with_inherited_derivations(cd, &pool);
    let instantiable = !target.is_abstract_or_mixin(cd_name);
    let default_mint_ids = class_default_mint_ids(ctx)
        .into_iter()
        .find(|(name, _)| *name == cd_name)
        .and_then(|(_, mint)| mint);
    let Ok(slots) = target.induced_slots(cd_name) else {
        return;
    };
    for slot in slots {
        if derived.contains(&slot.name) {
            continue;
        }
        if slot.identifier && !instantiable {
            continue;
        }
        if slot.identifier && mints_ids(ctx.spec.mint_ids, default_mint_ids, class_mint_ids) {
            messages.push(ValidationMessage::new(
                Severity::Info,
                cd_path,
                format!(
                    "Identifier slot '{}' has no derivation; \
                     it will be synthesized from a hash of the record's content",
                    slot.name
                ),
            ));
        } else if slot.identifier {
            messages.push(ValidationMessage::new(
                Severity::Error,
                cd_path,
                missing_identifier_message(cd_name, &slot.name),
            ));
        } else if slot.required {
            messages.push(ValidationMessage::new(
                Severity::Warning,
                cd_path,
                format!("Required target slot '{}' has no derivation", slot.name),
            ));
        }
    }
}
