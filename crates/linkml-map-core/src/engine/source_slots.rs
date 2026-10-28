//! The slots each source class declares, resolved once.
//!
//! Upstream's `Bindings` answers `None` for a name that is not in the row but is a
//! slot of the source class (`_collect_schema_slots`: `class_induced_slots` of the
//! source type, read when the `Bindings` is built). Doing that per row walks the
//! schema every time, so [`SourceSlots`] does it once per class, and a pipeline
//! that builds an engine per row attaches the table with
//! [`ObjectTransformer::with_source_slots`](super::ObjectTransformer::with_source_slots).

use std::sync::Arc;

use super::FastMap;
use crate::expr::DeclaredSlots;
use crate::schema::SchemaProvider;

/// Class name -> the names of the slots it declares (induced: own, inherited and
/// mixed in), for every class of the source schema.
///
/// Plain data behind `Arc`s, so one table is shared by every worker thread and
/// each row's evaluations take a cheap handle on their class's set.
#[derive(Debug, Clone, Default)]
pub struct SourceSlots {
    by_class: FastMap<String, Arc<DeclaredSlots>>,
}

impl SourceSlots {
    /// Resolve the declared slots of every class of `schema`.
    ///
    /// A class whose slots cannot be induced is absent, which reads as declaring
    /// none, as upstream's `source_type in sv.all_classes()` guard does for a
    /// source type that is not a class.
    pub fn build(schema: &dyn SchemaProvider) -> Self {
        let by_class = schema
            .all_class_names()
            .into_iter()
            .filter_map(|class| Some((class.clone(), Self::class_slots(schema, &class)?)))
            .collect();
        Self { by_class }
    }

    /// The slots `class` declares, resolved afresh; `None` when `class` is not a
    /// class of `schema`.
    pub(super) fn class_slots(
        schema: &dyn SchemaProvider,
        class: &str,
    ) -> Option<Arc<DeclaredSlots>> {
        let slots = schema.induced_slots(class).ok()?;
        Some(Arc::new(slots.into_iter().map(|s| s.name).collect()))
    }

    /// The slots `class` declares; `None` for a name that is not a class of the
    /// schema the table was built from.
    pub fn slots_of(&self, class: &str) -> Option<&Arc<DeclaredSlots>> {
        self.by_class.get(class)
    }
}
