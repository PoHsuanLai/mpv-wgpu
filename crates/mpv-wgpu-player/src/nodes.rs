//! Reading mpv's structured property values.

use crate::value::Value;

use crate::types::Finite;

/// The value under `key` in a node map.
pub(crate) fn field<'a>(entries: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    entries
        .iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}

/// A string field. An empty string is absent: mpv writes `""` for unset tags.
pub(crate) fn text(entries: &[(String, Value)], key: &str) -> Option<String> {
    match field(entries, key) {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

/// A flag field. Anything but a true flag reads as `false`.
pub(crate) fn flag(entries: &[(String, Value)], key: &str) -> bool {
    matches!(field(entries, key), Some(Value::Flag(true)))
}

/// An integer field.
pub(crate) fn int(entries: &[(String, Value)], key: &str) -> Option<i64> {
    match field(entries, key) {
        Some(Value::Int64(value)) => Some(*value),
        _ => None,
    }
}

/// A seconds field, from an integer or a double.
pub(crate) fn seconds(entries: &[(String, Value)], key: &str) -> Option<Finite> {
    match field(entries, key) {
        Some(Value::Double(value)) => Finite::new(*value),
        Some(Value::Int64(value)) => Finite::new(*value as f64),
        _ => None,
    }
}
