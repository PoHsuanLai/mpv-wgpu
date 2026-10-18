//! Property values as the player reads them, whichever core produced them.

/// A structured mpv value: the track and chapter lists arrive as these, and so does every
/// observed property.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Value {
    None,
    Flag(bool),
    Int64(i64),
    Double(f64),
    String(String),
    Array(Vec<Value>),
    Map(Vec<(String, Value)>),
}
