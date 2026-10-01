//! Reading mpv's structured property values.

use rsmpv::Node;

use crate::types::Finite;

/// The value under `key` in a node map.
pub(crate) fn field<'a>(entries: &'a [(String, Node)], key: &str) -> Option<&'a Node> {
    entries
        .iter()
        .find_map(|(name, value)| (name == key).then_some(value))
}

/// A string field. An empty string is absent: mpv writes `""` for unset tags.
pub(crate) fn text(entries: &[(String, Node)], key: &str) -> Option<String> {
    match field(entries, key) {
        Some(Node::String(value)) if !value.is_empty() => Some(value.clone()),
        _ => None,
    }
}

/// A flag field. Anything but a true flag reads as `false`.
pub(crate) fn flag(entries: &[(String, Node)], key: &str) -> bool {
    matches!(field(entries, key), Some(Node::Flag(true)))
}

/// An integer field.
pub(crate) fn int(entries: &[(String, Node)], key: &str) -> Option<i64> {
    match field(entries, key) {
        Some(Node::Int64(value)) => Some(*value),
        _ => None,
    }
}

/// A seconds field, from an integer or a double.
pub(crate) fn seconds(entries: &[(String, Node)], key: &str) -> Option<Finite> {
    match field(entries, key) {
        Some(Node::Double(value)) => Finite::new(*value),
        Some(Node::Int64(value)) => Finite::new(*value as f64),
        _ => None,
    }
}
