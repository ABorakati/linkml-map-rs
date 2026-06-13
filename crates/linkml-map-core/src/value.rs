// Dynamic Value enum for runtime transformation.
// TODO: Complete Value variants and serialization.

/// A dynamically-typed value during transformation.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// Null value
    Null,
    // Bool(bool),
    // Int(i64),
    // Float(f64),
    // Str(String),
    // List(Vec<Value>),
    // Map(indexmap::IndexMap<String, Value>),
}

impl Default for Value {
    fn default() -> Self {
        Value::Null
    }
}
