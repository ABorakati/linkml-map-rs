//! The Python containers [`Value`] models beyond list and string-keyed map:
//! `tuple` (a [`Value::Tuple`]), `set` ([`Value::Set`], see [`crate::pyset`])
//! and a `dict` with any hashable key ([`Value::Dict`]).
//!
//! A [`Value::Map`] is what the engine binds from a record: its keys are
//! strings and, upstream, it is a `DynObj` (always truthy, a missing attribute
//! is `None`). A [`Value::Dict`] is a dict built by an expression (a display, a
//! comprehension, `dict(...)`): any hashable key, insertion ordered, falsy when
//! empty. `1`, `1.0` and `True` are one key, as in Python.

use indexmap::IndexMap;

use super::Value;
use crate::pyset::{self, PySet, SetItem};

/// What Python's `hash`/`==` see of a hashable [`Value`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PyKey {
    None,
    /// An integer, or an integral float, bool or decimal equal to one.
    Int(i64),
    /// A float (bits) that is not an `i64`-sized integer; also a decimal that
    /// is exactly that float. NaN compares equal to itself here (as the same
    /// object does in CPython, which checks identity first).
    Float(u64),
    /// A decimal equal to no int or float: its canonical text.
    Decimal(String),
    Str(String),
    Temporal(&'static str, String),
    Tuple(Vec<PyKey>),
    /// An integer beyond `i64`, or an integral float or decimal equal to one.
    Big(crate::pynum::BigInt),
    /// A complex number with a non-zero imaginary part (the bits of both
    /// parts); one without is its real part.
    Complex(u64, u64),
}

/// `TypeError: unhashable type: '...'`.
fn unhashable(v: &Value) -> String {
    format!("unhashable type: '{}'", v.python_type_name())
}

impl Value {
    /// Python's name for the type of the value.
    #[must_use]
    pub fn python_type_name(&self) -> &'static str {
        match self {
            Value::Null => "NoneType",
            Value::Bool(_) => "bool",
            Value::Int(_) | Value::BigInt(_) => "int",
            Value::Float(_) => "float",
            Value::Complex(_) => "complex",
            Value::Decimal(_) => "Decimal",
            Value::Temporal(t) => t.type_name(),
            Value::Str(_) => "str",
            Value::List(_) => "list",
            Value::Map(_) | Value::Dict(_) => "dict",
            Value::Tuple(_) => "tuple",
            Value::Set(_) => "set",
        }
    }

    /// The key of a hashable value.
    ///
    /// # Errors
    /// `TypeError: unhashable type` for a list, dict or set (also when nested in
    /// a tuple).
    pub fn py_key(&self) -> Result<PyKey, String> {
        Ok(match self {
            Value::Null => PyKey::None,
            Value::Bool(b) => PyKey::Int(i64::from(*b)),
            Value::Int(i) => PyKey::Int(*i),
            Value::BigInt(n) => PyKey::Big((**n).clone()),
            Value::Float(f) => float_key(*f),
            // A complex number with no imaginary part is its real part.
            Value::Complex(z) if z.im == 0.0 => float_key(z.re),
            Value::Complex(z) => PyKey::Complex(z.re.to_bits(), z.im.to_bits()),
            Value::Decimal(d) => {
                let f = d.to_f64();
                if f.is_finite() && d.cmp_f64(f) == Some(std::cmp::Ordering::Equal) {
                    float_key(f)
                } else if let Some(n) = decimal_integer(d) {
                    int_key(n)
                } else {
                    PyKey::Decimal(d.canonical())
                }
            }
            Value::Temporal(t) => PyKey::Temporal(t.type_name(), t.to_string()),
            Value::Str(s) => PyKey::Str(s.clone()),
            Value::Tuple(items) => {
                PyKey::Tuple(items.iter().map(Value::py_key).collect::<Result<_, _>>()?)
            }
            other => return Err(unhashable(other)),
        })
    }

    /// `hash(value)` when CPython's is deterministic across processes; `None`
    /// when it is randomised (`str`, `date`) or address based (NaN).
    ///
    /// # Errors
    /// `TypeError: unhashable type`.
    pub fn py_hash(&self) -> Result<Option<i64>, String> {
        Ok(match self {
            Value::Null => Some(pyset::HASH_NONE),
            Value::Bool(b) => Some(pyset::hash_i64(i64::from(*b))),
            Value::Int(i) => Some(pyset::hash_i64(*i)),
            Value::Float(f) => pyset::hash_f64(*f),
            Value::BigInt(n) => Some(crate::pynum::hash(crate::pynum::IntRef::Big(n))),
            Value::Complex(z) => complex_hash(z.re, z.im),
            Value::Decimal(d) => {
                let (negative, coeff, exp) = d.hash_parts();
                Some(pyset::hash_decimal(negative, coeff, exp))
            }
            Value::Temporal(_) | Value::Str(_) => None,
            Value::Tuple(items) => {
                let mut hashes = Vec::with_capacity(items.len());
                let mut deterministic = true;
                for item in items {
                    match item.py_hash()? {
                        Some(h) => hashes.push(h),
                        None => deterministic = false,
                    }
                }
                deterministic.then(|| pyset::hash_tuple(&hashes))
            }
            other => return Err(unhashable(other)),
        })
    }
}

fn float_key(f: f64) -> PyKey {
    if f.fract() == 0.0 && f.abs() < 9.0e18 {
        PyKey::Int(f as i64)
    } else if f.is_finite() && f.fract() == 0.0 {
        int_key(crate::pynum::float_to_bigint(f))
    } else {
        PyKey::Float(f.to_bits())
    }
}

/// The key of an integer: `Int` while it fits `i64`, as a [`Value::Int`] does.
fn int_key(n: crate::pynum::BigInt) -> PyKey {
    match i64::try_from(&n) {
        Ok(i) => PyKey::Int(i),
        Err(_) => PyKey::Big(n),
    }
}

/// The integer a decimal equals, when it equals one of a size an int can be
/// compared at (`Decimal('1E+30') == 10**30`).
fn decimal_integer(d: &crate::value::Decimal) -> Option<crate::pynum::BigInt> {
    let (negative, coeff, exp) = d.hash_parts();
    let digits = coeff.trim_end_matches('0');
    let zeros = coeff.len() - digits.len();
    let shift = i64::try_from(zeros).ok()? + exp;
    if shift < 0 && !digits.is_empty() {
        return None;
    }
    if digits.len() as i64 + shift > 5000 {
        return None;
    }
    let mut n: crate::pynum::BigInt = if digits.is_empty() {
        0.into()
    } else {
        digits.parse().ok()?
    };
    if shift > 0 {
        n *= crate::pynum::BigInt::from(10).pow(shift as u32);
    }
    Some(if negative { -n } else { n })
}

/// `hash(complex)`: `hash(re) + 1000003 * hash(im)` in unsigned 64-bit
/// arithmetic; `None` when either part is NaN.
fn complex_hash(re: f64, im: f64) -> Option<i64> {
    let (re, im) = (pyset::hash_f64(re)?, pyset::hash_f64(im)?);
    let combined = (re as u64).wrapping_add(1_000_003u64.wrapping_mul(im as u64));
    Some(if combined == u64::MAX {
        -2
    } else {
        combined as i64
    })
}

impl SetItem for Value {
    type Key = PyKey;

    fn py_hash(&self) -> Result<Option<i64>, String> {
        Value::py_hash(self)
    }

    fn set_key(&self) -> PyKey {
        self.py_key().unwrap_or(PyKey::None)
    }

    fn set_eq(&self, other: &Value) -> bool {
        self.py_eq(other)
    }
}

/// A Python `dict` with hashable keys of any type, in insertion order.
#[derive(Debug, Clone, Default)]
pub struct PyDict {
    entries: IndexMap<PyKey, (Value, Value)>,
}

impl PyDict {
    /// `{}`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `len(d)`.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `not d`.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// `d[key] = value`: an existing equal key keeps its original object
    /// (`d[1.0] = x` on `{1: y}` leaves the key `1`).
    ///
    /// # Errors
    /// `TypeError: unhashable type` for the key.
    pub fn insert(&mut self, key: Value, value: Value) -> Result<(), String> {
        let k = key.py_key()?;
        match self.entries.get_mut(&k) {
            Some(entry) => entry.1 = value,
            None => {
                self.entries.insert(k, (key, value));
            }
        }
        Ok(())
    }

    /// `d.get(key)`.
    ///
    /// # Errors
    /// `TypeError: unhashable type` for the key.
    pub fn get(&self, key: &Value) -> Result<Option<&Value>, String> {
        Ok(self.entries.get(&key.py_key()?).map(|(_, v)| v))
    }

    /// `key in d`.
    ///
    /// # Errors
    /// `TypeError: unhashable type` for the key.
    pub fn contains_key(&self, key: &Value) -> Result<bool, String> {
        Ok(self.entries.contains_key(&key.py_key()?))
    }

    /// `d.pop(key)`.
    ///
    /// # Errors
    /// `TypeError: unhashable type` for the key.
    pub fn remove(&mut self, key: &Value) -> Result<Option<Value>, String> {
        Ok(self.entries.shift_remove(&key.py_key()?).map(|(_, v)| v))
    }

    /// The `(key, value)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&Value, &Value)> {
        self.entries.values().map(|(k, v)| (k, v))
    }

    /// The keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        self.entries.values().map(|(k, _)| k)
    }

    /// The values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = &Value> {
        self.entries.values().map(|(_, v)| v)
    }

    /// The same keys with each value replaced by `f(value)`, stopping at the
    /// first error.
    ///
    /// # Errors
    /// Whatever `f` returns.
    pub fn try_map_values<E>(
        self,
        mut f: impl FnMut(Value) -> Result<Value, E>,
    ) -> Result<PyDict, E> {
        let mut entries = IndexMap::with_capacity(self.entries.len());
        for (k, (key, value)) in self.entries {
            entries.insert(k, (key, f(value)?));
        }
        Ok(PyDict { entries })
    }

    /// A dict from the pairs of a string-keyed map.
    #[must_use]
    pub fn from_map(map: &IndexMap<String, Value>) -> Self {
        let entries = map
            .iter()
            .map(|(k, v)| (PyKey::Str(k.clone()), (Value::Str(k.clone()), v.clone())))
            .collect();
        PyDict { entries }
    }

    /// The same pairs as a string-keyed map when every key is a string.
    #[must_use]
    pub fn to_string_map(&self) -> Option<IndexMap<String, Value>> {
        self.iter()
            .map(|(k, v)| match k {
                Value::Str(s) => Some((s.clone(), v.clone())),
                _ => None,
            })
            .collect()
    }

    /// Python `==`: the same keys with equal values, whatever the order.
    #[must_use]
    pub fn py_eq(&self, other: &PyDict) -> bool {
        self.len() == other.len()
            && self
                .entries
                .iter()
                .all(|(k, (_, v))| other.entries.get(k).is_some_and(|(_, w)| v.py_eq(w)))
    }
}

impl FromIterator<(Value, Value)> for PyDict {
    /// Collect pairs, keeping the first key object of equal keys. An unhashable
    /// key panics: callers check hashability first.
    fn from_iter<I: IntoIterator<Item = (Value, Value)>>(iter: I) -> Self {
        let mut dict = PyDict::new();
        for (k, v) in iter {
            dict.insert(k, v).expect("hashable key");
        }
        dict
    }
}

impl Value {
    /// The form a value takes when it leaves an expression: a [`Value::Dict`]
    /// whose keys are all strings becomes the string-keyed [`Value::Map`] a
    /// record is (at any depth); one with another kind of key stays a dict.
    #[must_use]
    pub fn into_record_form(self) -> Value {
        match self {
            Value::Null
            | Value::Bool(_)
            | Value::Int(_)
            | Value::BigInt(_)
            | Value::Float(_)
            | Value::Complex(_)
            | Value::Str(_)
            | Value::Decimal(_)
            | Value::Temporal(_) => self,
            Value::List(mut items) => {
                items
                    .iter_mut()
                    .for_each(|v| *v = std::mem::take(v).into_record_form());
                Value::List(items)
            }
            Value::Tuple(mut items) => {
                items
                    .iter_mut()
                    .for_each(|v| *v = std::mem::take(v).into_record_form());
                Value::Tuple(items)
            }
            Value::Map(mut map) => {
                map.values_mut()
                    .for_each(|v| *v = std::mem::take(v).into_record_form());
                Value::Map(map)
            }
            Value::Set(set) => Value::Set(set),
            Value::Dict(dict) => {
                let pairs: Vec<(Value, Value)> = dict
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone().into_record_form()))
                    .collect();
                if pairs.iter().all(|(k, _)| matches!(k, Value::Str(_))) {
                    Value::Map(
                        pairs
                            .into_iter()
                            .map(|(k, v)| match k {
                                Value::Str(s) => (s, v),
                                _ => unreachable!("checked to be strings"),
                            })
                            .collect(),
                    )
                } else {
                    Value::Dict(Box::new(pairs.into_iter().collect()))
                }
            }
        }
    }
}

/// A new set value.
#[must_use]
pub fn set_value(set: PySet<Value>) -> Value {
    Value::Set(Box::new(set))
}

/// A new dict value.
#[must_use]
pub fn dict_value(dict: PyDict) -> Value {
    Value::Dict(Box::new(dict))
}

/// The text simplejson (`dumps_json`) writes for a dict key: a string as it
/// is, an int as `str`, a float as its `repr` (`NaN`, `Infinity`), `true`,
/// `false`, `null`, a decimal as `str`.
///
/// # Errors
/// `TypeError: keys must be str, int, float, bool or None, not ...` for any
/// other key (a tuple, a date).
pub fn simplejson_key(key: &Value) -> Result<String, String> {
    Ok(match key {
        Value::Str(s) => s.clone(),
        Value::Int(i) => i.to_string(),
        Value::BigInt(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_owned(),
        Value::Float(f) if f.is_nan() => "NaN".to_owned(),
        Value::Float(f) if f.is_infinite() => {
            if *f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
        }
        Value::Float(f) => crate::pyfmt::py_float_repr(*f),
        Value::Decimal(d) => d.to_sci_string(),
        Value::Temporal(t) => {
            return Err(format!(
                "keys must be str, int, float, bool or None, not datetime.{}",
                t.type_name()
            ));
        }
        other => {
            return Err(format!(
                "keys must be str, int, float, bool or None, not {}",
                other.python_type_name()
            ));
        }
    })
}

/// [`simplejson_key`] for the generic (infallible) conversions: a key JSON
/// cannot write falls back to its `str()`.
#[must_use]
pub fn json_key(key: &Value) -> String {
    simplejson_key(key).unwrap_or_else(|_| crate::expr::eval::py_str(key))
}

/// `==` for the container variants (the arms of [`Value::py_eq`] beyond the
/// scalars, lists and maps).
pub(super) fn py_eq_containers(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| p.py_eq(q))
        }
        (Value::Set(x), Value::Set(y)) => x.set_equal(y),
        (Value::Dict(x), Value::Dict(y)) => x.py_eq(y),
        // A dict built by an expression equals a record map with the same
        // string-keyed contents (the record is the same dict upstream unless
        // it is a `DynObj`, whose equality is identity).
        (Value::Dict(d), Value::Map(m)) | (Value::Map(m), Value::Dict(d)) => {
            d.len() == m.len()
                && d.iter().all(|(k, v)| match k {
                    Value::Str(k) => m.get(k).is_some_and(|w| v.py_eq(w)),
                    _ => false,
                })
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::Decimal;

    fn dec(text: &str) -> Value {
        Value::Decimal(Decimal::parse_python(text).unwrap())
    }

    #[test]
    fn equal_numbers_share_a_hash_and_a_key() {
        let one = [
            Value::Int(1),
            Value::Bool(true),
            Value::Float(1.0),
            dec("1"),
            dec("1.000"),
        ];
        for v in &one {
            assert_eq!(v.py_hash(), Ok(Some(1)), "{v:?}");
            assert_eq!(v.py_key(), Ok(PyKey::Int(1)), "{v:?}");
        }
        // A decimal exactly equal to a float is that float.
        assert_eq!(dec("1.5").py_hash(), Value::Float(1.5).py_hash());
        assert_eq!(dec("-0.25").py_hash(), Value::Float(-0.25).py_hash());
        assert_eq!(dec("1.5").py_key(), Value::Float(1.5).py_key());
        // `Decimal("0.1") != 0.1`: a different key, whatever the hash.
        assert_ne!(dec("0.1").py_key(), Value::Float(0.1).py_key());
        // A tuple hashes through its items (and has no deterministic hash with a str).
        let pair = Value::Tuple(vec![Value::Int(1), Value::Int(2)]);
        assert_eq!(pair.py_hash(), Ok(Some(crate::pyset::hash_tuple(&[1, 2]))));
        let with_str = Value::Tuple(vec![Value::Int(1), Value::Str("a".into())]);
        assert_eq!(with_str.py_hash(), Ok(None));
        assert!(Value::List(vec![]).py_hash().is_err());
        assert!(Value::Tuple(vec![Value::List(vec![])]).py_hash().is_err());
    }

    #[test]
    fn a_dict_keeps_the_first_key_object_and_insertion_order() {
        let mut d = PyDict::new();
        d.insert(Value::Int(1), Value::Str("a".into())).unwrap();
        d.insert(Value::Str("1".into()), Value::Str("b".into()))
            .unwrap();
        d.insert(Value::Float(1.0), Value::Str("c".into())).unwrap();
        d.insert(Value::Bool(true), Value::Str("d".into())).unwrap();
        assert_eq!(d.len(), 2);
        let keys: Vec<&Value> = d.keys().collect();
        assert_eq!(keys, [&Value::Int(1), &Value::Str("1".into())]);
        assert_eq!(d.get(&Value::Float(1.0)), Ok(Some(&Value::Str("d".into()))));
        assert!(d.insert(Value::List(vec![]), Value::Null).is_err());
        assert!(d.to_string_map().is_none());
    }

    #[test]
    fn dict_keys_as_simplejson_writes_them() {
        assert_eq!(simplejson_key(&Value::Int(3)).unwrap(), "3");
        assert_eq!(simplejson_key(&Value::Float(2.5)).unwrap(), "2.5");
        assert_eq!(simplejson_key(&Value::Float(f64::NAN)).unwrap(), "NaN");
        assert_eq!(simplejson_key(&Value::Bool(false)).unwrap(), "false");
        assert_eq!(simplejson_key(&Value::Null).unwrap(), "null");
        assert!(simplejson_key(&Value::Tuple(vec![])).is_err());
    }

    #[test]
    fn a_dict_with_only_string_keys_becomes_a_record_map() {
        let strings: PyDict = [(Value::Str("a".into()), Value::Int(1))]
            .into_iter()
            .collect();
        assert!(matches!(
            Value::Dict(Box::new(strings)).into_record_form(),
            Value::Map(_)
        ));
        let mixed: PyDict = [(Value::Int(1), Value::Int(1))].into_iter().collect();
        assert!(matches!(
            Value::List(vec![Value::Dict(Box::new(mixed))]).into_record_form(),
            Value::List(items) if matches!(items[0], Value::Dict(_))
        ));
    }

    #[test]
    fn sets_of_values_iterate_in_cpython_order() {
        let set = PySet::from_items([Value::Int(3), Value::Int(1), Value::Int(2)]).unwrap();
        assert_eq!(set.to_vec(), [Value::Int(1), Value::Int(2), Value::Int(3)]);
        assert!(set.is_table_ordered());
        // A string has a randomised hash: insertion order from then on.
        let mut with_str = set.clone();
        with_str.add(Value::Str("z".into())).unwrap();
        assert!(!with_str.is_table_ordered());
        assert_eq!(with_str.len(), 4);
        assert!(with_str.contains(&Value::Float(2.0)).unwrap());
    }
}
