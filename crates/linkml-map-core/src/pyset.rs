//! Python's `set`, with CPython 3.13's iteration order.
//!
//! A CPython set iterates its hash table in slot order, so the order of
//! `list({3, 1, 2})` (and `repr`, `str`, JSON and YAML output, `join`, ...)
//! follows from the elements' hashes, the table size and the insertion and
//! deletion history. This module reproduces `Objects/setobject.c` for
//! elements whose hash is deterministic: open addressing with
//! [`LINEAR_PROBES`] linear probes then perturbation, the resize rules, dummy
//! entries, `set_merge`'s table copy, and the iteration choices of the set
//! operators.
//!
//! PARITY: `str` and `bytes` hashes are randomised per process
//! (`PYTHONHASHSEED`), so the order of a set holding any of them is not
//! reproducible even upstream. Such a set (and any set containing an element
//! with a randomised or identity hash: `str`, `date`, NaN) falls back to
//! insertion order, which is the deterministic choice; once a set has fallen
//! back it stays ordered by insertion. Everything else about it (membership,
//! equality, algebra) is unaffected.
//!
//! The table is generic over the element so the engine's [`crate::value::Value`]
//! and the `asteval` port's value type share one implementation (see [`SetItem`]).

use std::fmt::Debug;
use std::hash::Hash;

use indexmap::IndexMap;

/// `PySet_MINSIZE`.
const MIN_SIZE: usize = 8;
/// `LINEAR_PROBES`: after a collision the next nine slots are tried in order.
const LINEAR_PROBES: usize = 9;
/// `PERTURB_SHIFT`.
const PERTURB_SHIFT: u32 = 5;

/// What a set needs to know about an element.
pub trait SetItem: Clone + Debug {
    /// The element's identity under Python `==`/`hash` (`1`, `1.0` and `True`
    /// are one key): the index of the insertion-ordered fallback.
    type Key: Clone + Eq + Hash + Debug;

    /// `hash(self)` when CPython's is deterministic across processes, `None`
    /// when it is randomised or identity-based; `Err(message)` when the value
    /// is unhashable (`unhashable type: 'list'`).
    ///
    /// # Errors
    /// The `TypeError` message for an unhashable element.
    fn py_hash(&self) -> Result<Option<i64>, String>;

    /// The key of a hashable element (called only after [`py_hash`](Self::py_hash) succeeded).
    fn set_key(&self) -> Self::Key;

    /// Python `self == other` for two hashable values.
    fn set_eq(&self, other: &Self) -> bool;
}

/// One slot of CPython's table.
#[derive(Clone, Debug)]
enum Slot<T> {
    /// `key == NULL`: never used since the last resize.
    Empty,
    /// `key == dummy`: a deleted entry.
    Dummy,
    Full {
        hash: i64,
        key: T,
    },
}

/// `PySetObject`'s table: `table`/`mask`, `fill` (active + dummy), `used`
/// (active) and the `pop` finger.
#[derive(Clone, Debug)]
struct Table<T> {
    slots: Vec<Slot<T>>,
    fill: usize,
    used: usize,
    finger: usize,
}

/// `(i * 5 + 1 + perturb) & mask` in `size_t` arithmetic.
fn next_probe(i: usize, perturb: u64, mask: usize) -> usize {
    ((i as u64)
        .wrapping_mul(5)
        .wrapping_add(1)
        .wrapping_add(perturb) as usize)
        & mask
}

impl<T: SetItem> Table<T> {
    fn new() -> Self {
        Table {
            slots: vec_of_empty(MIN_SIZE),
            fill: 0,
            used: 0,
            finger: 0,
        }
    }

    fn mask(&self) -> usize {
        self.slots.len() - 1
    }

    /// `set_lookkey`: the slot holding `key`, if any.
    fn lookup(&self, hash: i64, key: &T) -> Option<usize> {
        let mask = self.mask();
        let mut i = (hash as u64 as usize) & mask;
        let mut perturb = hash as u64;
        loop {
            let probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for idx in i..=i + probes {
                match &self.slots[idx] {
                    Slot::Empty => return None,
                    Slot::Full { hash: h, key: k } if *h == hash && k.set_eq(key) => {
                        return Some(idx);
                    }
                    _ => {}
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = next_probe(i, perturb, mask);
        }
    }

    /// `set_add_entry`; `true` when the element was not already present.
    fn add(&mut self, key: T, hash: i64) -> bool {
        let mask = self.mask();
        let mut i = (hash as u64 as usize) & mask;
        let mut perturb = hash as u64;
        let mut freeslot: Option<usize> = None;
        let target = 'search: loop {
            let probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            for idx in i..=i + probes {
                match &self.slots[idx] {
                    Slot::Empty => break 'search idx,
                    Slot::Full { hash: h, key: k } if *h == hash && k.set_eq(&key) => {
                        return false;
                    }
                    // Observed on CPython 3.13: the *last* dummy met before the unused
                    // slot is the one reused.
                    Slot::Dummy => freeslot = Some(idx),
                    _ => {}
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = next_probe(i, perturb, mask);
        };
        if let Some(slot) = freeslot {
            self.slots[slot] = Slot::Full { hash, key };
            self.used += 1;
            return true;
        }
        self.slots[target] = Slot::Full { hash, key };
        self.fill += 1;
        self.used += 1;
        if self.fill * 5 >= mask * 3 {
            let minused = if self.used > 50_000 {
                self.used * 2
            } else {
                self.used * 4
            };
            self.resize(minused);
        }
        true
    }

    /// `set_insert_clean`: insert into a table known to have no duplicates and
    /// no dummies.
    fn insert_clean(slots: &mut [Slot<T>], hash: i64, key: T) {
        let mask = slots.len() - 1;
        let mut i = (hash as u64 as usize) & mask;
        let mut perturb = hash as u64;
        loop {
            if matches!(slots[i], Slot::Empty) {
                slots[i] = Slot::Full { hash, key };
                return;
            }
            if i + LINEAR_PROBES <= mask
                && let Some(slot) = slots[i + 1..=i + LINEAR_PROBES]
                    .iter_mut()
                    .find(|s| matches!(s, Slot::Empty))
            {
                *slot = Slot::Full { hash, key };
                return;
            }
            perturb >>= PERTURB_SHIFT;
            i = next_probe(i, perturb, mask);
        }
    }

    /// `set_table_resize`: rebuild with room for `minused` entries, dropping
    /// dummies.
    fn resize(&mut self, minused: usize) {
        let mut newsize = MIN_SIZE;
        while newsize <= minused {
            newsize <<= 1;
        }
        // "No dummies, so no point doing anything" for the small table.
        if newsize == MIN_SIZE && self.slots.len() == MIN_SIZE && self.fill == self.used {
            return;
        }
        let old = std::mem::replace(&mut self.slots, vec_of_empty(newsize));
        for slot in old {
            if let Slot::Full { hash, key } = slot {
                Self::insert_clean(&mut self.slots, hash, key);
            }
        }
        self.fill = self.used;
    }

    /// `set_discard_entry`: `true` when the element was present.
    fn discard(&mut self, hash: i64, key: &T) -> bool {
        match self.lookup(hash, key) {
            Some(idx) => {
                self.slots[idx] = Slot::Dummy;
                self.used -= 1;
                true
            }
            None => false,
        }
    }

    fn contains(&self, hash: i64, key: &T) -> bool {
        self.lookup(hash, key).is_some()
    }

    fn iter(&self) -> impl Iterator<Item = (i64, &T)> {
        self.slots.iter().filter_map(|s| match s {
            Slot::Full { hash, key } => Some((*hash, key)),
            _ => None,
        })
    }

    /// `set_pop`: the entry at or after the finger.
    fn pop(&mut self) -> Option<T> {
        if self.used == 0 {
            return None;
        }
        let mask = self.mask();
        let mut idx = self.finger & mask;
        while !matches!(self.slots[idx], Slot::Full { .. }) {
            idx += 1;
            if idx > mask {
                idx = 0;
            }
        }
        let Slot::Full { key, .. } = std::mem::replace(&mut self.slots[idx], Slot::Dummy) else {
            unreachable!("the scan stops at a full slot")
        };
        self.used -= 1;
        self.finger = idx + 1;
        Some(key)
    }

    /// `set_merge` of another table into this one.
    fn merge(&mut self, other: &Table<T>) {
        if other.used == 0 {
            return;
        }
        // One big resize up front: few overlapping keys are expected.
        if (self.fill + other.used) * 5 >= self.mask() * 3 {
            self.resize((self.used + other.used) * 2);
        }
        // An empty table of the same size and no dummies to drop: copy as is.
        if self.fill == 0 && self.mask() == other.mask() && other.fill == other.used {
            self.slots.clone_from(&other.slots);
            self.fill = other.fill;
            self.used = other.used;
            return;
        }
        // An empty table needs no duplicate checks.
        if self.fill == 0 {
            self.fill = other.used;
            self.used = other.used;
            for (hash, key) in other.iter() {
                Self::insert_clean(&mut self.slots, hash, key.clone());
            }
            return;
        }
        for (hash, key) in other.iter() {
            self.add(key.clone(), hash);
        }
    }
}

fn vec_of_empty<T>(n: usize) -> Vec<Slot<T>> {
    let mut v = Vec::with_capacity(n);
    v.resize_with(n, || Slot::Empty);
    v
}

#[derive(Clone, Debug)]
enum Repr<T: SetItem> {
    /// Every element has a deterministic hash: CPython's table.
    Table(Table<T>),
    /// An element's hash is randomised: insertion order.
    Ordered(IndexMap<T::Key, T>),
}

/// A Python `set`.
#[derive(Clone, Debug)]
pub struct PySet<T: SetItem> {
    repr: Repr<T>,
}

impl<T: SetItem> Default for PySet<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator over a set in Python's iteration order.
pub struct Iter<'a, T: SetItem> {
    inner: IterInner<'a, T>,
}

enum IterInner<'a, T: SetItem> {
    Table(std::slice::Iter<'a, Slot<T>>),
    Ordered(indexmap::map::Values<'a, T::Key, T>),
}

impl<'a, T: SetItem> Iterator for Iter<'a, T> {
    type Item = &'a T;

    fn next(&mut self) -> Option<&'a T> {
        match &mut self.inner {
            IterInner::Table(slots) => slots.find_map(|s| match s {
                Slot::Full { key, .. } => Some(key),
                _ => None,
            }),
            IterInner::Ordered(values) => values.next(),
        }
    }
}

impl<T: SetItem> PySet<T> {
    /// `set()`.
    #[must_use]
    pub fn new() -> Self {
        PySet {
            repr: Repr::Table(Table::new()),
        }
    }

    /// A set that iterates exactly as `items` are given, whatever CPython's
    /// table would do: for a set object whose iteration order is already known
    /// (the `set` of another Python process, say). Elements equal to an earlier
    /// one are dropped.
    ///
    /// When rebuilding the set by insertion gives the same order (as it does for
    /// a set that has not been resized down or had elements removed), the result
    /// keeps CPython's table order for further operations.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn from_iteration_order(items: Vec<T>) -> Result<Self, String> {
        let rebuilt = Self::from_items(items.clone())?;
        let same =
            rebuilt.len() == items.len() && rebuilt.iter().zip(&items).all(|(a, b)| a.set_eq(b));
        if same {
            return Ok(rebuilt);
        }
        let mut map = IndexMap::new();
        for item in items {
            item.py_hash()?;
            map.entry(item.set_key()).or_insert(item);
        }
        Ok(PySet {
            repr: Repr::Ordered(map),
        })
    }

    /// `len(s)`.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.repr {
            Repr::Table(t) => t.used,
            Repr::Ordered(m) => m.len(),
        }
    }

    /// `not s`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// `iter(s)`.
    #[must_use]
    pub fn iter(&self) -> Iter<'_, T> {
        Iter {
            inner: match &self.repr {
                Repr::Table(t) => IterInner::Table(t.slots.iter()),
                Repr::Ordered(m) => IterInner::Ordered(m.values()),
            },
        }
    }

    /// Whether the set still follows CPython's table order (every element has a
    /// deterministic hash).
    #[must_use]
    pub fn is_table_ordered(&self) -> bool {
        matches!(self.repr, Repr::Table(_))
    }

    /// Switch to insertion order, keeping the current iteration order.
    fn make_ordered(&mut self) {
        if let Repr::Table(t) = &self.repr {
            let map: IndexMap<T::Key, T> =
                t.iter().map(|(_, k)| (k.set_key(), k.clone())).collect();
            self.repr = Repr::Ordered(map);
        }
    }

    /// `set_add_entry` with the hash already computed.
    fn add_hashed(&mut self, item: T, hash: Option<i64>) -> bool {
        if let (Repr::Table(t), Some(h)) = (&mut self.repr, hash) {
            return t.add(item, h);
        }
        self.make_ordered();
        let Repr::Ordered(m) = &mut self.repr else {
            unreachable!("made ordered above")
        };
        match m.entry(item.set_key()) {
            indexmap::map::Entry::Occupied(_) => false,
            indexmap::map::Entry::Vacant(v) => {
                v.insert(item);
                true
            }
        }
    }

    /// `s.add(item)`; `true` when it was not already present.
    ///
    /// # Errors
    /// `TypeError: unhashable type` for a list, dict or set element.
    pub fn add(&mut self, item: T) -> Result<bool, String> {
        let hash = item.py_hash()?;
        Ok(self.add_hashed(item, hash))
    }

    /// `item in s`.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn contains(&self, item: &T) -> Result<bool, String> {
        let hash = item.py_hash()?;
        Ok(self.contains_hashed(item, hash))
    }

    fn contains_hashed(&self, item: &T, hash: Option<i64>) -> bool {
        match (&self.repr, hash) {
            (Repr::Table(t), Some(h)) => t.contains(h, item),
            // Every element has a deterministic hash, the needle has not: it
            // equals none of them.
            (Repr::Table(_), None) => false,
            (Repr::Ordered(m), _) => m.contains_key(&item.set_key()),
        }
    }

    /// `s.discard(item)`; `true` when it was present.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn discard(&mut self, item: &T) -> Result<bool, String> {
        let hash = item.py_hash()?;
        Ok(self.discard_hashed(item, hash))
    }

    fn discard_hashed(&mut self, item: &T, hash: Option<i64>) -> bool {
        match (&mut self.repr, hash) {
            (Repr::Table(t), Some(h)) => t.discard(h, item),
            (Repr::Table(_), None) => false,
            (Repr::Ordered(m), _) => m.shift_remove(&item.set_key()).is_some(),
        }
    }

    /// `s.pop()`; `None` when empty (`KeyError: 'pop from an empty set'`).
    pub fn pop(&mut self) -> Option<T> {
        match &mut self.repr {
            Repr::Table(t) => t.pop(),
            Repr::Ordered(m) => m.shift_remove_index(0).map(|(_, v)| v),
        }
    }

    /// `s.clear()`.
    pub fn clear(&mut self) {
        self.repr = Repr::Table(Table::new());
    }

    /// `set(items)` / a set display: add one element after another.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn from_items(items: impl IntoIterator<Item = T>) -> Result<Self, String> {
        let mut set = Self::new();
        set.update_items(items)?;
        Ok(set)
    }

    /// `s.update(iterable)` for a generic iterable: one `set_add_key` each.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn update_items(&mut self, items: impl IntoIterator<Item = T>) -> Result<(), String> {
        for item in items {
            self.add(item)?;
        }
        Ok(())
    }

    /// `s.update(some_dict)`: CPython resizes once for the whole dict up front.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn update_dict_keys(&mut self, keys: Vec<T>) -> Result<(), String> {
        // Hash everything first so an unhashable key fails before any change
        // (an unhashable key cannot be a dict key anyway).
        let mut hashed = Vec::with_capacity(keys.len());
        for key in keys {
            let hash = key.py_hash()?;
            hashed.push((key, hash));
        }
        if let Repr::Table(t) = &mut self.repr
            && (t.fill + hashed.len()) * 5 >= t.mask() * 3
        {
            let minused = (t.used + hashed.len()) * 2;
            t.resize(minused);
        }
        for (key, hash) in hashed {
            self.add_hashed(key, hash);
        }
        Ok(())
    }

    /// `set_merge(self, other)`.
    pub fn merge(&mut self, other: &PySet<T>) {
        if other.is_empty() || std::ptr::eq(self, other) {
            return;
        }
        if let (Repr::Table(mine), Repr::Table(theirs)) = (&mut self.repr, &other.repr) {
            mine.merge(theirs);
            return;
        }
        for item in other.iter() {
            self.add_unchecked(item.clone());
        }
    }

    /// Add an element of another set, which is hashable by construction.
    fn add_unchecked(&mut self, item: T) {
        let hash = item.py_hash().ok().flatten();
        self.add_hashed(item, hash);
    }

    fn contains_unchecked(&self, item: &T) -> bool {
        let hash = item.py_hash().ok().flatten();
        self.contains_hashed(item, hash)
    }

    fn discard_unchecked(&mut self, item: &T) -> bool {
        let hash = item.py_hash().ok().flatten();
        self.discard_hashed(item, hash)
    }

    /// `set(other_set)` / `s.copy()`.
    #[must_use]
    pub fn copy(&self) -> Self {
        let mut out = Self::new();
        out.merge(self);
        out
    }

    /// `a | b` (`set_or`): a copy of `a`, updated with `b`.
    #[must_use]
    pub fn union(&self, other: &PySet<T>) -> Self {
        let mut out = self.copy();
        if !std::ptr::eq(self, other) {
            out.merge(other);
        }
        out
    }

    /// `a & b` (`set_intersection` of two sets): the smaller set is walked.
    #[must_use]
    pub fn intersection(&self, other: &PySet<T>) -> Self {
        if std::ptr::eq(self, other) {
            return self.copy();
        }
        let mut out = Self::new();
        let (outer, inner) = if other.len() > self.len() {
            (other, self)
        } else {
            (self, other)
        };
        // CPython walks `other` and probes `so`, after swapping so that `so`
        // is the larger set; so it walks the smaller.
        let (walk, probe) = (inner, outer);
        for item in walk.iter() {
            if probe.contains_unchecked(item) {
                out.add_unchecked(item.clone());
            }
        }
        out
    }

    /// `s.intersection(iterable)` for a non-set: walk the iterable.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn intersection_items(&self, items: impl IntoIterator<Item = T>) -> Result<Self, String> {
        let mut out = Self::new();
        for item in items {
            if self.contains(&item)? {
                out.add(item)?;
            }
        }
        Ok(out)
    }

    /// `a - b` (`set_difference` of two sets).
    #[must_use]
    pub fn difference(&self, other: &PySet<T>) -> Self {
        // Much larger than the other: copy, then discard the other's elements.
        if (self.len() >> 2) > other.len() {
            let mut out = self.copy();
            out.difference_update(other);
            return out;
        }
        let mut out = Self::new();
        for item in self.iter() {
            if !other.contains_unchecked(item) {
                out.add_unchecked(item.clone());
            }
        }
        out
    }

    /// `s.difference(iterable)` for a non-set: a copy with each item discarded.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn difference_items(&self, items: impl IntoIterator<Item = T>) -> Result<Self, String> {
        let mut out = self.copy();
        out.difference_update_items(items)?;
        Ok(out)
    }

    /// `s.difference_update(other_set)` (`set_difference_update_internal`).
    pub fn difference_update(&mut self, other: &PySet<T>) {
        if std::ptr::eq(self, other) {
            self.clear();
            return;
        }
        for item in other.iter() {
            self.discard_unchecked(item);
        }
        self.drop_dummies();
    }

    /// `s.difference_update(iterable)` for a non-set.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn difference_update_items(
        &mut self,
        items: impl IntoIterator<Item = T>,
    ) -> Result<(), String> {
        for item in items {
            self.discard(&item)?;
        }
        self.drop_dummies();
        Ok(())
    }

    /// "If more than 1/4th are dummies, then resize them away."
    fn drop_dummies(&mut self) {
        if let Repr::Table(t) = &mut self.repr
            && t.fill - t.used > t.mask() / 4
        {
            let minused = if t.used > 50_000 {
                t.used * 2
            } else {
                t.used * 4
            };
            t.resize(minused);
        }
    }

    /// `a ^ b` (`set_symmetric_difference`): a copy of `b`, then each element
    /// of `a` is removed from it or, when absent, added.
    #[must_use]
    pub fn symmetric_difference(&self, other: &PySet<T>) -> Self {
        let mut out = other.copy();
        out.symmetric_difference_update_from(self);
        out
    }

    /// `s.symmetric_difference(iterable)` for a non-set.
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn symmetric_difference_items(
        &self,
        items: impl IntoIterator<Item = T>,
    ) -> Result<Self, String> {
        // The fresh set built from the iterable is updated in place (no copy).
        let mut other = Self::from_items(items)?;
        other.symmetric_difference_update_from(self);
        Ok(other)
    }

    /// `set_symmetric_difference_update(self, other)`.
    pub fn symmetric_difference_update_from(&mut self, other: &PySet<T>) {
        let items: Vec<&T> = other.iter().collect();
        for item in items {
            if !self.discard_unchecked(item) {
                self.add_unchecked(item.clone());
            }
        }
    }

    /// `s.intersection_update(other)`: the result's table replaces `s`'s.
    pub fn intersection_update(&mut self, other: &PySet<T>) {
        *self = self.intersection(other);
    }

    /// The same set with every element converted by `f`, keeping the table
    /// layout (and so the iteration order) when each converted element has the
    /// hash of the original; otherwise the converted elements in iteration order.
    #[must_use]
    pub fn map_items<U: SetItem>(&self, f: impl Fn(&T) -> U) -> PySet<U> {
        if let Repr::Table(t) = &self.repr {
            let mut slots = Vec::with_capacity(t.slots.len());
            let mut same_layout = true;
            for slot in &t.slots {
                slots.push(match slot {
                    Slot::Empty => Slot::Empty,
                    Slot::Dummy => Slot::Dummy,
                    Slot::Full { hash, key } => {
                        let mapped = f(key);
                        same_layout &= matches!(mapped.py_hash(), Ok(Some(h)) if h == *hash);
                        Slot::Full {
                            hash: *hash,
                            key: mapped,
                        }
                    }
                });
            }
            if same_layout {
                return PySet {
                    repr: Repr::Table(Table {
                        slots,
                        fill: t.fill,
                        used: t.used,
                        finger: t.finger,
                    }),
                };
            }
        }
        let mut out = PySet::<U>::new();
        for item in self.iter() {
            // Elements of a set are hashable; their images are best effort.
            let _ = out.add(f(item));
        }
        out
    }

    /// `a <= b`.
    #[must_use]
    pub fn is_subset(&self, other: &PySet<T>) -> bool {
        self.len() <= other.len() && self.iter().all(|item| other.contains_unchecked(item))
    }

    /// `a == b`.
    #[must_use]
    pub fn set_equal(&self, other: &PySet<T>) -> bool {
        self.len() == other.len() && self.is_subset(other)
    }

    /// `a.isdisjoint(b)`.
    #[must_use]
    pub fn is_disjoint(&self, other: &PySet<T>) -> bool {
        let (walk, probe) = if self.len() > other.len() {
            (other, self)
        } else {
            (self, other)
        };
        walk.iter().all(|item| !probe.contains_unchecked(item))
    }

    /// The elements as a vector, in iteration order.
    #[must_use]
    pub fn to_vec(&self) -> Vec<T> {
        self.iter().cloned().collect()
    }
}

// === CPython's hash functions =============================================

/// `sys.hash_info.modulus`: the Mersenne prime 2**61 - 1.
pub const HASH_MODULUS: u64 = (1 << 61) - 1;
/// `sys.hash_info.inf`.
pub const HASH_INF: i64 = 314_159;
/// `hash(None)`: a constant since CPython 3.12.
pub const HASH_NONE: i64 = 0xFCA8_6420;

/// The final `-1` to `-2` mapping every `hash()` applies.
fn no_minus_one(h: i64) -> i64 {
    if h == -1 { -2 } else { h }
}

/// `hash(int)`: the value modulo 2**61 - 1, signed.
#[must_use]
pub fn hash_i64(n: i64) -> i64 {
    let magnitude = (u128::from(n.unsigned_abs()) % u128::from(HASH_MODULUS)) as i64;
    no_minus_one(if n < 0 { -magnitude } else { magnitude })
}

/// `hash(float)` (`_Py_HashDouble`); `None` for NaN, whose hash is the address
/// of the object.
#[must_use]
pub fn hash_f64(v: f64) -> Option<i64> {
    if v.is_nan() {
        return None;
    }
    if v.is_infinite() {
        return Some(if v > 0.0 { HASH_INF } else { -HASH_INF });
    }
    let (mut m, mut e) = frexp(v);
    let sign: i64 = if m < 0.0 {
        m = -m;
        -1
    } else {
        1
    };
    let mut x: u64 = 0;
    while m != 0.0 {
        x = ((x << 28) & HASH_MODULUS) | (x >> (61 - 28));
        m *= 268_435_456.0; // 2**28
        e -= 28;
        let y = m as u64;
        m -= y as f64;
        x += y;
        if x >= HASH_MODULUS {
            x -= HASH_MODULUS;
        }
    }
    // Reduce the exponent modulo 61 (always non-negative).
    let e = if e >= 0 {
        e % 61
    } else {
        61 - 1 - ((-1 - e) % 61)
    } as u32;
    x = ((x << e) & HASH_MODULUS) | (x >> (61 - e));
    Some(no_minus_one(x as i64 * sign))
}

/// `math.frexp`: mantissa in `[0.5, 1)` (signed) and exponent.
fn frexp(x: f64) -> (f64, i32) {
    if x == 0.0 || !x.is_finite() {
        return (x, 0);
    }
    let bits = x.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        let (m, e) = frexp(x * 2f64.powi(54));
        return (m, e - 54);
    }
    let m = f64::from_bits((bits & !(0x7ffu64 << 52)) | (1022u64 << 52));
    (m, exp - 1022)
}

/// `hash(tuple)` from the hashes of its items (the xxHash-based algorithm of
/// CPython 3.8 and later).
#[must_use]
pub fn hash_tuple(item_hashes: &[i64]) -> i64 {
    const PRIME_1: u64 = 11_400_714_785_074_694_791;
    const PRIME_2: u64 = 14_029_467_366_897_019_727;
    const PRIME_5: u64 = 2_870_177_450_012_600_261;
    let mut acc = PRIME_5;
    for &h in item_hashes {
        acc = acc.wrapping_add((h as u64).wrapping_mul(PRIME_2));
        acc = acc.rotate_left(31);
        acc = acc.wrapping_mul(PRIME_1);
    }
    acc = acc.wrapping_add((item_hashes.len() as u64) ^ (PRIME_5 ^ 3_527_539));
    if acc == u64::MAX {
        return 1_546_275_796;
    }
    acc as i64
}

/// `hash(Decimal)`: the hash of the number's rational value, so it equals the
/// hash of the equal `int` or `float`.
#[must_use]
pub fn hash_decimal(negative: bool, coefficient: &str, exponent: i64) -> i64 {
    fn mul_mod(a: u64, b: u64) -> u64 {
        (u128::from(a) * u128::from(b) % u128::from(HASH_MODULUS)) as u64
    }
    fn pow_mod(mut base: u64, mut exp: u64) -> u64 {
        let mut acc = 1;
        base %= HASH_MODULUS;
        while exp > 0 {
            if exp & 1 == 1 {
                acc = mul_mod(acc, base);
            }
            base = mul_mod(base, base);
            exp >>= 1;
        }
        acc
    }
    let coeff = coefficient.bytes().fold(0_u64, |acc, d| {
        (acc * 10 + u64::from(d - b'0')) % HASH_MODULUS
    });
    let exp_hash = if exponent >= 0 {
        pow_mod(10, exponent.unsigned_abs())
    } else {
        // 10**(P-2) is the inverse of 10 modulo P.
        let inverse = pow_mod(10, HASH_MODULUS - 2);
        pow_mod(inverse, exponent.unsigned_abs())
    };
    let h = mul_mod(coeff, exp_hash) as i64;
    no_minus_one(if negative { -h } else { h })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A test element with Python's `==` and `hash`.
    #[derive(Clone, Debug)]
    enum E {
        I(i64),
        F(f64),
        N,
        T(Vec<E>),
    }

    #[derive(Clone, Debug, PartialEq, Eq, Hash)]
    enum K {
        N,
        I(i64),
        F(u64),
        T(Vec<K>),
    }

    impl SetItem for E {
        type Key = K;

        fn py_hash(&self) -> Result<Option<i64>, String> {
            Ok(match self {
                E::I(i) => Some(hash_i64(*i)),
                E::F(f) => hash_f64(*f),
                E::N => Some(HASH_NONE),
                E::T(items) => {
                    let mut hashes = Vec::new();
                    for item in items {
                        match item.py_hash()? {
                            Some(h) => hashes.push(h),
                            None => return Ok(None),
                        }
                    }
                    Some(hash_tuple(&hashes))
                }
            })
        }

        fn set_key(&self) -> K {
            match self {
                E::I(i) => K::I(*i),
                E::F(f) if f.fract() == 0.0 && f.abs() < 9.0e18 => K::I(*f as i64),
                E::F(f) => K::F(f.to_bits()),
                E::N => K::N,
                E::T(items) => K::T(items.iter().map(SetItem::set_key).collect()),
            }
        }

        fn set_eq(&self, other: &Self) -> bool {
            self.set_key() == other.set_key()
        }
    }

    fn decode(j: &serde_json::Value) -> E {
        use serde_json::Value as J;
        match j {
            J::Null => E::N,
            J::Bool(b) => E::I(i64::from(*b)),
            J::Number(n) => E::I(n.as_i64().expect("i64")),
            J::Object(o) if o.contains_key("f") => {
                let bits = u64::from_str_radix(o["f"].as_str().unwrap(), 16).unwrap();
                E::F(f64::from_bits(bits))
            }
            J::Object(o) => E::T(o["t"].as_array().unwrap().iter().map(decode).collect()),
            other => panic!("bad element {other}"),
        }
    }

    fn items(j: &serde_json::Value) -> Vec<E> {
        j.as_array().unwrap().iter().map(decode).collect()
    }

    fn same(a: &E, b: &E) -> bool {
        match (a, b) {
            (E::F(x), E::F(y)) => x.to_bits() == y.to_bits(),
            (E::T(x), E::T(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q)),
            (E::I(x), E::I(y)) => x == y,
            (E::N, E::N) => true,
            _ => false,
        }
    }

    type Sets = std::collections::HashMap<String, PySet<E>>;

    /// Replays `tests/data/pyset_scenarios.json` (CPython 3.13's iteration order
    /// after every step of random set operations; see
    /// `tools/gen_pyset_scenarios.py`).
    #[test]
    fn scenarios_iterate_as_cpython() {
        let scenarios: serde_json::Value =
            serde_json::from_str(include_str!("../tests/data/pyset_scenarios.json")).unwrap();
        for (n, sc) in scenarios.as_array().unwrap().iter().enumerate() {
            let mut sets = Sets::new();
            let snaps = sc["snaps"].as_array().unwrap();
            for (step, op) in sc["ops"].as_array().unwrap().iter().enumerate() {
                let a = op.as_array().unwrap();
                let name = a[1].as_str().unwrap().to_owned();
                let get = |sets: &Sets, i: usize| sets[a[i].as_str().unwrap()].clone();
                match a[0].as_str().unwrap() {
                    "new" => {
                        sets.insert(name, PySet::from_items(items(&a[2])).unwrap());
                    }
                    "setdict" => {
                        let mut s = PySet::new();
                        s.update_dict_keys(items(&a[2])).unwrap();
                        sets.insert(name, s);
                    }
                    "add" => {
                        sets.get_mut(&name).unwrap().add(decode(&a[2])).unwrap();
                    }
                    "discard" => {
                        sets.get_mut(&name)
                            .unwrap()
                            .discard(&decode(&a[2]))
                            .unwrap();
                    }
                    "pop" => {
                        sets.get_mut(&name).unwrap().pop();
                    }
                    "clear" => sets.get_mut(&name).unwrap().clear(),
                    "op" => {
                        let (x, y) = (get(&sets, 3), get(&sets, 4));
                        let r = match a[2].as_str().unwrap() {
                            "|" => x.union(&y),
                            "&" => x.intersection(&y),
                            "-" => x.difference(&y),
                            _ => x.symmetric_difference(&y),
                        };
                        sets.insert(name, r);
                    }
                    "update" => {
                        let y = get(&sets, 2);
                        sets.get_mut(&name).unwrap().merge(&y);
                    }
                    "dupdate" => {
                        sets.get_mut(&name)
                            .unwrap()
                            .update_dict_keys(items(&a[2]))
                            .unwrap();
                    }
                    "inter_u" => {
                        let y = get(&sets, 2);
                        sets.get_mut(&name).unwrap().intersection_update(&y);
                    }
                    "sym_u" => {
                        let y = get(&sets, 2);
                        sets.get_mut(&name)
                            .unwrap()
                            .symmetric_difference_update_from(&y);
                    }
                    "copy" => {
                        let s = get(&sets, 2).copy();
                        sets.insert(name, s);
                    }
                    kind @ ("union_items" | "diff_items" | "inter_items" | "sym_items") => {
                        let s = get(&sets, 2);
                        let its = items(&a[3]);
                        let r = match kind {
                            "union_items" => {
                                let mut c = s.copy();
                                c.update_items(its).unwrap();
                                c
                            }
                            "diff_items" => s.difference_items(its).unwrap(),
                            "inter_items" => s.intersection_items(its).unwrap(),
                            _ => s.symmetric_difference_items(its).unwrap(),
                        };
                        sets.insert(name, r);
                    }
                    other => panic!("unknown op {other}"),
                }
                for (set_name, expected) in snaps[step].as_object().unwrap() {
                    let want = items(expected);
                    let got = sets[set_name].to_vec();
                    let ok =
                        want.len() == got.len() && want.iter().zip(&got).all(|(p, q)| same(p, q));
                    assert!(
                        ok,
                        "scenario {n} step {step} ({op}) set {set_name}:\n want {want:?}\n got  {got:?}"
                    );
                }
            }
        }
    }

    fn ints(items: &[i64]) -> PySet<E> {
        PySet::from_items(items.iter().map(|&i| E::I(i))).unwrap()
    }

    fn order(s: &PySet<E>) -> Vec<i64> {
        s.iter()
            .map(|e| match e {
                E::I(i) => *i,
                E::F(f) => *f as i64,
                _ => -999,
            })
            .collect()
    }

    #[test]
    fn hashes_match_cpython() {
        assert_eq!(hash_i64(-1), -2);
        assert_eq!(hash_i64(-2), -2);
        assert_eq!(hash_i64(1 << 61), 1);
        assert_eq!(hash_f64(1.5), Some(1_152_921_504_606_846_977));
        assert_eq!(hash_f64(f64::INFINITY), Some(314_159));
        assert_eq!(hash_f64(-0.0), Some(0));
        assert_eq!(hash_f64(1e300), Some(1_224_995_262_755_759_164));
        assert_eq!(hash_f64(0.1), Some(230_584_300_921_369_408));
        assert_eq!(hash_tuple(&[1, 2]), -3_550_055_125_485_641_917);
        assert_eq!(hash_tuple(&[]), 5_740_354_900_026_072_187);
        assert_eq!(hash_decimal(false, "15", -1), 1_152_921_504_606_846_977);
    }

    #[test]
    fn small_int_sets_iterate_as_cpython() {
        // python3.13: list({3, 1, 2}), list({10, 9, 8, 7, 6, 5}), list({-1, -2, 100})
        assert_eq!(order(&ints(&[3, 1, 2])), [1, 2, 3]);
        assert_eq!(order(&ints(&[10, 9, 8, 7, 6, 5])), [5, 6, 7, 8, 9, 10]);
        assert_eq!(order(&ints(&[-1, -2, 100])), [100, -1, -2]);
    }
}
