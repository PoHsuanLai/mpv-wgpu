//! Property values as the player reads them, whichever core produced them.

/// A structured mpv value: the track and chapter lists arrive as these.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    None,
    Flag(bool),
    Int64(i64),
    Double(f64),
    String(String),
    Array(Vec<Node>),
    Map(Vec<(String, Node)>),
}

/// The data of an observed property.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PropertyData {
    None,
    String(String),
    Flag(bool),
    Int64(i64),
    Double(f64),
    Node(Node),
}
