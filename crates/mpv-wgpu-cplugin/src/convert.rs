//! `mpv_node` and property data to protocol values, and back for writes.

use std::ffi::{CStr, c_char, c_void};

use mpv_wgpu_protocol::Value;

use crate::ffi::{
    MPV_FORMAT_DOUBLE, MPV_FORMAT_FLAG, MPV_FORMAT_INT64, MPV_FORMAT_NODE, MPV_FORMAT_NODE_ARRAY,
    MPV_FORMAT_NODE_MAP, MPV_FORMAT_NONE, MPV_FORMAT_STRING, mpv_node,
};

/// Same nesting limit as the protocol decoder.
const MAX_DEPTH: usize = 16;

/// Text from a C string. A null pointer is the empty string; invalid UTF-8 is repaired.
///
/// # Safety
///
/// `text` is null or points to a NUL-terminated string that stays valid for the call.
pub(crate) unsafe fn text(text: *const c_char) -> String {
    if text.is_null() {
        return String::new();
    }
    // SAFETY: not null, and the caller promises a valid NUL-terminated string.
    unsafe { CStr::from_ptr(text) }
        .to_string_lossy()
        .into_owned()
}

/// A node as a [`Value`]. Byte arrays and unknown formats become [`Value::None`].
///
/// # Safety
///
/// `node` is a valid `mpv_node` whose pointers, including those of nested lists,
/// stay valid for the call.
pub(crate) unsafe fn node(node: &mpv_node) -> Value {
    // SAFETY: forwarded from the caller.
    unsafe { node_at(node, 0) }
}

unsafe fn node_at(node: &mpv_node, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return Value::None;
    }
    // SAFETY (all union reads below): `format` says which member is valid.
    match node.format {
        MPV_FORMAT_STRING => Value::String(unsafe { text(node.u.string) }),
        MPV_FORMAT_FLAG => Value::Flag(unsafe { node.u.flag } != 0),
        MPV_FORMAT_INT64 => Value::Int64(unsafe { node.u.int64 }),
        MPV_FORMAT_DOUBLE => Value::Double(unsafe { node.u.double_ }),
        MPV_FORMAT_NODE_ARRAY | MPV_FORMAT_NODE_MAP => {
            // SAFETY: a list node's `list` is valid for its format, and not null.
            let list = unsafe { node.u.list.as_ref() };
            let Some(list) = list else {
                return Value::None;
            };
            let count = usize::try_from(list.num).unwrap_or(0);
            let values: &[mpv_node] = if count == 0 || list.values.is_null() {
                &[]
            } else {
                // SAFETY: mpv guarantees `values[0..num]` are valid when `num > 0`.
                unsafe { std::slice::from_raw_parts(list.values, count) }
            };
            if node.format == MPV_FORMAT_NODE_ARRAY {
                return Value::Array(
                    values
                        .iter()
                        .map(|item| unsafe { node_at(item, depth + 1) })
                        .collect(),
                );
            }
            if list.keys.is_null() {
                return Value::None;
            }
            // SAFETY: for a map, `keys[0..num]` are valid when `num > 0`.
            let keys = unsafe { std::slice::from_raw_parts(list.keys, values.len()) };
            Value::Map(
                keys.iter()
                    .zip(values)
                    .map(|(&key, item)| (unsafe { text(key) }, unsafe { node_at(item, depth + 1) }))
                    .collect(),
            )
        }
        _ => Value::None,
    }
}

/// Property data of an event or reply, read as `format` says.
///
/// # Safety
///
/// `data` is null, or points to a value of the C type `format` names, valid for the call.
pub(crate) unsafe fn property(format: i32, data: *const c_void) -> Value {
    if data.is_null() {
        return Value::None;
    }
    // SAFETY (each read): `format` names the type `data` points to.
    match format {
        MPV_FORMAT_FLAG => Value::Flag(unsafe { *data.cast::<i32>() } != 0),
        MPV_FORMAT_INT64 => Value::Int64(unsafe { *data.cast::<i64>() }),
        MPV_FORMAT_DOUBLE => Value::Double(unsafe { *data.cast::<f64>() }),
        MPV_FORMAT_STRING => Value::String(unsafe { text(*data.cast::<*const c_char>()) }),
        MPV_FORMAT_NODE => unsafe { node(&*data.cast::<mpv_node>()) },
        MPV_FORMAT_NONE => Value::None,
        _ => Value::None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::ffi::CString;

    use super::*;
    use crate::ffi::{mpv_node_list, mpv_node_union};

    fn leaf(format: i32, u: mpv_node_union) -> mpv_node {
        mpv_node { u, format }
    }

    #[test]
    fn scalars_convert() {
        let text_c = CString::new("hello").unwrap();
        let cases = [
            (
                leaf(MPV_FORMAT_FLAG, mpv_node_union { flag: 1 }),
                Value::Flag(true),
            ),
            (
                leaf(MPV_FORMAT_INT64, mpv_node_union { int64: -7 }),
                Value::Int64(-7),
            ),
            (
                leaf(MPV_FORMAT_DOUBLE, mpv_node_union { double_: 1.5 }),
                Value::Double(1.5),
            ),
            (
                leaf(
                    MPV_FORMAT_STRING,
                    mpv_node_union {
                        string: text_c.as_ptr().cast_mut(),
                    },
                ),
                Value::String("hello".into()),
            ),
            (
                leaf(MPV_FORMAT_NONE, mpv_node_union { int64: 0 }),
                Value::None,
            ),
            (leaf(9, mpv_node_union { int64: 0 }), Value::None),
        ];
        for (input, expected) in cases {
            // SAFETY: the nodes above hold valid members for their formats.
            assert_eq!(unsafe { node(&input) }, expected);
        }
    }

    #[test]
    fn maps_and_arrays_convert() {
        let key_id = CString::new("id").unwrap();
        let key_kind = CString::new("type").unwrap();
        let kind = CString::new("audio").unwrap();
        let mut values = [
            leaf(MPV_FORMAT_INT64, mpv_node_union { int64: 2 }),
            leaf(
                MPV_FORMAT_STRING,
                mpv_node_union {
                    string: kind.as_ptr().cast_mut(),
                },
            ),
        ];
        let mut keys = [key_id.as_ptr().cast_mut(), key_kind.as_ptr().cast_mut()];
        let mut map_list = mpv_node_list {
            num: 2,
            values: values.as_mut_ptr(),
            keys: keys.as_mut_ptr(),
        };
        let mut items = [leaf(
            MPV_FORMAT_NODE_MAP,
            mpv_node_union {
                list: &mut map_list,
            },
        )];
        let mut array_list = mpv_node_list {
            num: 1,
            values: items.as_mut_ptr(),
            keys: std::ptr::null_mut(),
        };
        let array = leaf(
            MPV_FORMAT_NODE_ARRAY,
            mpv_node_union {
                list: &mut array_list,
            },
        );
        // SAFETY: every pointer above outlives the call and matches its format.
        let value = unsafe { node(&array) };
        assert_eq!(
            value,
            Value::Array(vec![Value::Map(vec![
                ("id".into(), Value::Int64(2)),
                ("type".into(), Value::String("audio".into())),
            ])])
        );
    }

    #[test]
    fn broken_lists_and_nulls_do_not_crash() {
        let null_list = leaf(
            MPV_FORMAT_NODE_ARRAY,
            mpv_node_union {
                list: std::ptr::null_mut(),
            },
        );
        // SAFETY: a null list pointer is handled.
        assert_eq!(unsafe { node(&null_list) }, Value::None);
        let mut empty = mpv_node_list {
            num: 0,
            values: std::ptr::null_mut(),
            keys: std::ptr::null_mut(),
        };
        let empty_array = leaf(MPV_FORMAT_NODE_ARRAY, mpv_node_union { list: &mut empty });
        // SAFETY: `num` is 0, so no element is read.
        assert_eq!(unsafe { node(&empty_array) }, Value::Array(Vec::new()));
        // SAFETY: null text is the empty string.
        assert_eq!(unsafe { text(std::ptr::null()) }, "");
        // SAFETY: null data is no value.
        assert_eq!(
            unsafe { property(MPV_FORMAT_DOUBLE, std::ptr::null()) },
            Value::None
        );
    }

    #[test]
    fn property_data_reads_by_format() {
        let flag: i32 = 1;
        let number: f64 = 12.5;
        let integer: i64 = 40;
        let c = CString::new("pq").unwrap();
        let string_ptr: *const c_char = c.as_ptr();
        // SAFETY: each pointer is to a value of the format passed with it.
        unsafe {
            assert_eq!(
                property(MPV_FORMAT_FLAG, (&raw const flag).cast()),
                Value::Flag(true)
            );
            assert_eq!(
                property(MPV_FORMAT_DOUBLE, (&raw const number).cast()),
                Value::Double(12.5)
            );
            assert_eq!(
                property(MPV_FORMAT_INT64, (&raw const integer).cast()),
                Value::Int64(40)
            );
            assert_eq!(
                property(MPV_FORMAT_STRING, (&raw const string_ptr).cast()),
                Value::String("pq".into())
            );
        }
    }
}
