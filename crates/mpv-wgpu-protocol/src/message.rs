//! Messages and their binary encoding.
//!
//! One message is one packet. Byte 0 is the message tag; the fields follow in
//! declaration order, little endian. A string is a `u32` byte length and UTF-8
//! bytes. An optional `f64` is a presence byte and then eight bytes. A
//! [`Value`] is a tag byte and a payload. Decoding checks every length against
//! the bytes that are left, so a hostile packet cannot make the decoder
//! allocate more than the packet is long.

use std::fmt;

/// Protocol version carried in [`Hello`]. Both sides must agree exactly.
pub const PROTOCOL_VERSION: u16 = 1;

/// Largest encoded message, in bytes. Longer values fail to encode.
pub const MAX_MESSAGE: usize = 512 * 1024;

/// Nesting limit for [`Value`]. mpv's property trees are two or three deep.
const MAX_DEPTH: usize = 16;

/// A property or command result: the subset of `mpv_node` the player uses.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    /// No value, or the property is unavailable.
    None,
    /// A boolean.
    Flag(bool),
    /// A 64-bit integer.
    Int64(i64),
    /// A double.
    Double(f64),
    /// A UTF-8 string.
    String(String),
    /// A list.
    Array(Vec<Value>),
    /// Key and value pairs, in the order mpv listed them.
    Map(Vec<(String, Value)>),
}

/// The type mpv is asked to deliver an observed property as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// `MPV_FORMAT_FLAG`.
    Flag,
    /// `MPV_FORMAT_INT64`.
    Int64,
    /// `MPV_FORMAT_DOUBLE`.
    Double,
    /// `MPV_FORMAT_STRING`.
    String,
    /// `MPV_FORMAT_NODE`.
    Node,
}

impl Format {
    fn tag(self) -> u8 {
        match self {
            Format::Flag => 1,
            Format::Int64 => 2,
            Format::Double => 3,
            Format::String => 4,
            Format::Node => 5,
        }
    }

    fn from_tag(tag: u8) -> Result<Self, DecodeError> {
        match tag {
            1 => Ok(Format::Flag),
            2 => Ok(Format::Int64),
            3 => Ok(Format::Double),
            4 => Ok(Format::String),
            5 => Ok(Format::Node),
            other => Err(DecodeError::UnknownTag(other)),
        }
    }
}

/// The plugin's first message: who it is and what it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    /// [`PROTOCOL_VERSION`] the plugin speaks.
    pub protocol: u16,
    /// `mpv_client_api_version()`: major in the high 16 bits, minor in the low.
    pub client_api: u32,
    /// The `mpv-version` property, such as `mpv v0.41.0`.
    pub mpv_version: String,
    /// The plugin crate's own version.
    pub plugin_version: String,
}

/// A frame is ready in a ring slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// The ring generation the slot belongs to ([`Resize::generation`]).
    pub generation: u32,
    /// Slot index within the ring.
    pub slot: u8,
    /// Counts up by one per rendered frame within a plugin run.
    pub seq: u64,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Bytes per row.
    pub stride: u32,
    /// `time-pos` when the frame was rendered, if mpv had one.
    pub pts: Option<f64>,
    /// `CLOCK_MONOTONIC` nanoseconds when the plugin finished rendering.
    pub sent_ns: u64,
}

/// A new ring. The memfd travels with this message as `SCM_RIGHTS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resize {
    /// Identifies this ring. Frames of an older generation are stale.
    pub generation: u32,
    /// Width in pixels of every slot.
    pub width: u32,
    /// Height in pixels of every slot.
    pub height: u32,
    /// Bytes per row of every slot.
    pub stride: u32,
    /// Number of slots.
    pub slots: u8,
}

/// Names one slot of one ring generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotRef {
    /// The ring generation.
    pub generation: u32,
    /// Slot index.
    pub slot: u8,
}

/// Everything that crosses the socket, in either direction.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Plugin to player: the handshake, sent once, first.
    Hello(Hello),
    /// Plugin to player: a rendered frame.
    Frame(Frame),
    /// Plugin to player: cumulative count of frames the plugin could not
    /// render because every slot was in use.
    Dropped {
        /// Total since the plugin started.
        count: u64,
    },
    /// Plugin to player: an observed property changed.
    Property {
        /// The property name.
        name: String,
        /// Its value, or [`Value::None`] when unavailable.
        value: Value,
    },
    /// Plugin to player: `MPV_EVENT_FILE_LOADED`.
    FileLoaded,
    /// Plugin to player: `MPV_EVENT_END_FILE`.
    EndFile {
        /// `mpv_end_file_reason`.
        reason: u8,
        /// `mpv_error`, 0 unless the reason is an error.
        error: i32,
    },
    /// Plugin to player: a log line from mpv.
    Log {
        /// `mpv_log_level`.
        level: u8,
        /// The module prefix.
        prefix: String,
        /// The text, newline included.
        text: String,
    },
    /// Plugin to player: the answer to a [`Message::Command`],
    /// [`Message::Set`] or [`Message::Get`].
    Reply {
        /// The request's id.
        id: u32,
        /// `mpv_error`; negative on failure.
        error: i32,
        /// The result, for a successful [`Message::Get`] or command.
        value: Value,
    },
    /// Player to plugin: render into this ring from now on. Carries a memfd.
    Resize(Resize),
    /// Player to plugin: the slot was uploaded. Frees it and drives `report_swap`.
    Presented(SlotRef),
    /// Player to plugin: the slot was not shown. Frees it, no `report_swap`.
    Released(SlotRef),
    /// Player to plugin: run an mpv command.
    Command {
        /// Echoed in the [`Message::Reply`].
        id: u32,
        /// The command and its arguments.
        args: Vec<String>,
    },
    /// Player to plugin: set a property.
    Set {
        /// Echoed in the [`Message::Reply`].
        id: u32,
        /// The property name.
        name: String,
        /// The new value. Only flag, integer, double and string are accepted.
        value: Value,
    },
    /// Player to plugin: read a property.
    Get {
        /// Echoed in the [`Message::Reply`].
        id: u32,
        /// The property name.
        name: String,
    },
    /// Player to plugin: report changes of a property.
    Observe {
        /// The property name.
        name: String,
        /// How mpv should deliver the value.
        format: Format,
    },
    /// Either direction: this side is finished. The plugin sends it when mpv
    /// shuts down; the player sends it to ask the plugin to quit mpv.
    Bye,
}

/// A message that cannot be encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EncodeError {
    /// The encoding would pass [`MAX_MESSAGE`].
    TooLarge,
}

impl fmt::Display for EncodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodeError::TooLarge => write!(f, "message is larger than {MAX_MESSAGE} bytes"),
        }
    }
}

impl std::error::Error for EncodeError {}

/// A packet that is not a valid message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecodeError {
    /// The packet ended before the message did.
    Truncated,
    /// The packet is longer than the message.
    Trailing,
    /// A message, value or format tag this version does not know.
    UnknownTag(u8),
    /// A string is not UTF-8.
    BadUtf8,
    /// A value nests deeper than the limit.
    TooDeep,
    /// A flag byte that is neither 0 nor 1.
    BadFlag(u8),
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecodeError::Truncated => f.write_str("packet is truncated"),
            DecodeError::Trailing => f.write_str("packet has trailing bytes"),
            DecodeError::UnknownTag(tag) => write!(f, "unknown tag {tag}"),
            DecodeError::BadUtf8 => f.write_str("string is not utf-8"),
            DecodeError::TooDeep => f.write_str("value nests too deeply"),
            DecodeError::BadFlag(byte) => write!(f, "flag byte {byte} is not 0 or 1"),
        }
    }
}

impl std::error::Error for DecodeError {}

const T_HELLO: u8 = 1;
const T_FRAME: u8 = 2;
const T_DROPPED: u8 = 3;
const T_PROPERTY: u8 = 4;
const T_FILE_LOADED: u8 = 5;
const T_END_FILE: u8 = 6;
const T_LOG: u8 = 7;
const T_REPLY: u8 = 8;
const T_RESIZE: u8 = 9;
const T_PRESENTED: u8 = 10;
const T_RELEASED: u8 = 11;
const T_COMMAND: u8 = 12;
const T_SET: u8 = 13;
const T_GET: u8 = 14;
const T_OBSERVE: u8 = 15;
const T_BYE: u8 = 16;

const V_NONE: u8 = 0;
const V_FLAG: u8 = 1;
const V_INT64: u8 = 2;
const V_DOUBLE: u8 = 3;
const V_STRING: u8 = 4;
const V_ARRAY: u8 = 5;
const V_MAP: u8 = 6;

struct Writer<'a> {
    out: &'a mut Vec<u8>,
}

impl Writer<'_> {
    fn u8(&mut self, value: u8) {
        self.out.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    fn i32(&mut self, value: i32) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    fn f64(&mut self, value: f64) {
        self.out.extend_from_slice(&value.to_le_bytes());
    }

    fn str(&mut self, value: &str) -> Result<(), EncodeError> {
        let len = u32::try_from(value.len()).map_err(|_| EncodeError::TooLarge)?;
        self.u32(len);
        self.out.extend_from_slice(value.as_bytes());
        Ok(())
    }

    fn count(&mut self, len: usize) -> Result<(), EncodeError> {
        self.u32(u32::try_from(len).map_err(|_| EncodeError::TooLarge)?);
        Ok(())
    }

    fn value(&mut self, value: &Value) -> Result<(), EncodeError> {
        match value {
            Value::None => self.u8(V_NONE),
            Value::Flag(flag) => {
                self.u8(V_FLAG);
                self.u8(u8::from(*flag));
            }
            Value::Int64(number) => {
                self.u8(V_INT64);
                self.out.extend_from_slice(&number.to_le_bytes());
            }
            Value::Double(number) => {
                self.u8(V_DOUBLE);
                self.f64(*number);
            }
            Value::String(text) => {
                self.u8(V_STRING);
                self.str(text)?;
            }
            Value::Array(items) => {
                self.u8(V_ARRAY);
                self.count(items.len())?;
                for item in items {
                    self.value(item)?;
                }
            }
            Value::Map(entries) => {
                self.u8(V_MAP);
                self.count(entries.len())?;
                for (key, item) in entries {
                    self.str(key)?;
                    self.value(item)?;
                }
            }
        }
        if self.out.len() > MAX_MESSAGE {
            return Err(EncodeError::TooLarge);
        }
        Ok(())
    }

    fn slot(&mut self, slot: SlotRef) {
        self.u32(slot.generation);
        self.u8(slot.slot);
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if self.bytes.len() < len {
            return Err(DecodeError::Truncated);
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.array()?))
    }

    fn i64(&mut self) -> Result<i64, DecodeError> {
        Ok(i64::from_le_bytes(self.array()?))
    }

    fn f64(&mut self) -> Result<f64, DecodeError> {
        Ok(f64::from_le_bytes(self.array()?))
    }

    fn string(&mut self) -> Result<String, DecodeError> {
        let len = self.u32()? as usize;
        let raw = self.take(len)?;
        String::from_utf8(raw.to_vec()).map_err(|_| DecodeError::BadUtf8)
    }

    /// An element count. Every element is at least one byte, so a count past
    /// the remaining bytes is a truncated packet, found before any allocation.
    fn count(&mut self) -> Result<usize, DecodeError> {
        let count = self.u32()? as usize;
        if count > self.bytes.len() {
            return Err(DecodeError::Truncated);
        }
        Ok(count)
    }

    fn value(&mut self, depth: usize) -> Result<Value, DecodeError> {
        if depth > MAX_DEPTH {
            return Err(DecodeError::TooDeep);
        }
        match self.u8()? {
            V_NONE => Ok(Value::None),
            V_FLAG => Ok(Value::Flag(self.flag()?)),
            V_INT64 => Ok(Value::Int64(self.i64()?)),
            V_DOUBLE => Ok(Value::Double(self.f64()?)),
            V_STRING => Ok(Value::String(self.string()?)),
            V_ARRAY => {
                let count = self.count()?;
                let mut items = Vec::with_capacity(count);
                for _ in 0..count {
                    items.push(self.value(depth + 1)?);
                }
                Ok(Value::Array(items))
            }
            V_MAP => {
                let count = self.count()?;
                let mut entries = Vec::with_capacity(count);
                for _ in 0..count {
                    let key = self.string()?;
                    entries.push((key, self.value(depth + 1)?));
                }
                Ok(Value::Map(entries))
            }
            other => Err(DecodeError::UnknownTag(other)),
        }
    }

    fn flag(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => Err(DecodeError::BadFlag(other)),
        }
    }

    fn slot(&mut self) -> Result<SlotRef, DecodeError> {
        Ok(SlotRef {
            generation: self.u32()?,
            slot: self.u8()?,
        })
    }
}

impl Message {
    /// Append the encoding of `self` to `out`. On error `out` is cleared.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), EncodeError> {
        out.clear();
        let result = self.write(&mut Writer { out });
        if result.is_err() || out.len() > MAX_MESSAGE {
            out.clear();
            return Err(EncodeError::TooLarge);
        }
        Ok(())
    }

    fn write(&self, w: &mut Writer<'_>) -> Result<(), EncodeError> {
        match self {
            Message::Hello(hello) => {
                w.u8(T_HELLO);
                w.u16(hello.protocol);
                w.u32(hello.client_api);
                w.str(&hello.mpv_version)?;
                w.str(&hello.plugin_version)?;
            }
            Message::Frame(frame) => {
                w.u8(T_FRAME);
                w.u32(frame.generation);
                w.u8(frame.slot);
                w.u64(frame.seq);
                w.u32(frame.width);
                w.u32(frame.height);
                w.u32(frame.stride);
                match frame.pts {
                    Some(pts) => {
                        w.u8(1);
                        w.f64(pts);
                    }
                    None => w.u8(0),
                }
                w.u64(frame.sent_ns);
            }
            Message::Dropped { count } => {
                w.u8(T_DROPPED);
                w.u64(*count);
            }
            Message::Property { name, value } => {
                w.u8(T_PROPERTY);
                w.str(name)?;
                w.value(value)?;
            }
            Message::FileLoaded => w.u8(T_FILE_LOADED),
            Message::EndFile { reason, error } => {
                w.u8(T_END_FILE);
                w.u8(*reason);
                w.i32(*error);
            }
            Message::Log {
                level,
                prefix,
                text,
            } => {
                w.u8(T_LOG);
                w.u8(*level);
                w.str(prefix)?;
                w.str(text)?;
            }
            Message::Reply { id, error, value } => {
                w.u8(T_REPLY);
                w.u32(*id);
                w.i32(*error);
                w.value(value)?;
            }
            Message::Resize(resize) => {
                w.u8(T_RESIZE);
                w.u32(resize.generation);
                w.u32(resize.width);
                w.u32(resize.height);
                w.u32(resize.stride);
                w.u8(resize.slots);
            }
            Message::Presented(slot) => {
                w.u8(T_PRESENTED);
                w.slot(*slot);
            }
            Message::Released(slot) => {
                w.u8(T_RELEASED);
                w.slot(*slot);
            }
            Message::Command { id, args } => {
                w.u8(T_COMMAND);
                w.u32(*id);
                w.count(args.len())?;
                for arg in args {
                    w.str(arg)?;
                }
            }
            Message::Set { id, name, value } => {
                w.u8(T_SET);
                w.u32(*id);
                w.str(name)?;
                w.value(value)?;
            }
            Message::Get { id, name } => {
                w.u8(T_GET);
                w.u32(*id);
                w.str(name)?;
            }
            Message::Observe { name, format } => {
                w.u8(T_OBSERVE);
                w.str(name)?;
                w.u8(format.tag());
            }
            Message::Bye => w.u8(T_BYE),
        }
        if w.out.len() > MAX_MESSAGE {
            return Err(EncodeError::TooLarge);
        }
        Ok(())
    }

    /// Decode one whole packet.
    pub fn decode(packet: &[u8]) -> Result<Message, DecodeError> {
        let mut r = Reader { bytes: packet };
        let message = match r.u8()? {
            T_HELLO => Message::Hello(Hello {
                protocol: r.u16()?,
                client_api: r.u32()?,
                mpv_version: r.string()?,
                plugin_version: r.string()?,
            }),
            T_FRAME => Message::Frame(Frame {
                generation: r.u32()?,
                slot: r.u8()?,
                seq: r.u64()?,
                width: r.u32()?,
                height: r.u32()?,
                stride: r.u32()?,
                pts: match r.u8()? {
                    0 => None,
                    1 => Some(r.f64()?),
                    other => return Err(DecodeError::BadFlag(other)),
                },
                sent_ns: r.u64()?,
            }),
            T_DROPPED => Message::Dropped { count: r.u64()? },
            T_PROPERTY => Message::Property {
                name: r.string()?,
                value: r.value(0)?,
            },
            T_FILE_LOADED => Message::FileLoaded,
            T_END_FILE => Message::EndFile {
                reason: r.u8()?,
                error: r.i32()?,
            },
            T_LOG => Message::Log {
                level: r.u8()?,
                prefix: r.string()?,
                text: r.string()?,
            },
            T_REPLY => Message::Reply {
                id: r.u32()?,
                error: r.i32()?,
                value: r.value(0)?,
            },
            T_RESIZE => Message::Resize(Resize {
                generation: r.u32()?,
                width: r.u32()?,
                height: r.u32()?,
                stride: r.u32()?,
                slots: r.u8()?,
            }),
            T_PRESENTED => Message::Presented(r.slot()?),
            T_RELEASED => Message::Released(r.slot()?),
            T_COMMAND => {
                let id = r.u32()?;
                let count = r.count()?;
                let mut args = Vec::with_capacity(count);
                for _ in 0..count {
                    args.push(r.string()?);
                }
                Message::Command { id, args }
            }
            T_SET => Message::Set {
                id: r.u32()?,
                name: r.string()?,
                value: r.value(0)?,
            },
            T_GET => Message::Get {
                id: r.u32()?,
                name: r.string()?,
            },
            T_OBSERVE => Message::Observe {
                name: r.string()?,
                format: Format::from_tag(r.u8()?)?,
            },
            T_BYE => Message::Bye,
            other => return Err(DecodeError::UnknownTag(other)),
        };
        if !r.bytes.is_empty() {
            return Err(DecodeError::Trailing);
        }
        Ok(message)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn tracks() -> Value {
        Value::Array(vec![Value::Map(vec![
            ("id".into(), Value::Int64(1)),
            ("type".into(), Value::String("audio".into())),
            ("selected".into(), Value::Flag(true)),
            ("demux-fps".into(), Value::Double(23.976)),
            ("missing".into(), Value::None),
        ])])
    }

    fn samples() -> Vec<Message> {
        vec![
            Message::Hello(Hello {
                protocol: PROTOCOL_VERSION,
                client_api: 0x0002_0005,
                mpv_version: "mpv v0.41.0".into(),
                plugin_version: "0.1.0".into(),
            }),
            Message::Frame(Frame {
                generation: 7,
                slot: 2,
                seq: u64::MAX,
                width: 3840,
                height: 2160,
                stride: 15360,
                pts: Some(12.5),
                sent_ns: 99,
            }),
            Message::Frame(Frame {
                generation: 0,
                slot: 0,
                seq: 0,
                width: 1,
                height: 1,
                stride: 256,
                pts: None,
                sent_ns: 0,
            }),
            Message::Dropped { count: 12 },
            Message::Property {
                name: "track-list".into(),
                value: tracks(),
            },
            Message::Property {
                name: "pause".into(),
                value: Value::Flag(false),
            },
            Message::FileLoaded,
            Message::EndFile {
                reason: 4,
                error: -13,
            },
            Message::Log {
                level: 30,
                prefix: "ao".into(),
                text: "no device\n".into(),
            },
            Message::Reply {
                id: 3,
                error: -12,
                value: Value::None,
            },
            Message::Reply {
                id: 4,
                error: 0,
                value: Value::Double(f64::INFINITY),
            },
            Message::Resize(Resize {
                generation: 1,
                width: 640,
                height: 360,
                stride: 2560,
                slots: 3,
            }),
            Message::Presented(SlotRef {
                generation: 1,
                slot: 0,
            }),
            Message::Released(SlotRef {
                generation: 1,
                slot: 1,
            }),
            Message::Command {
                id: 9,
                args: vec!["loadfile".into(), "/tmp/a b.mkv".into(), "replace".into()],
            },
            Message::Command {
                id: 0,
                args: Vec::new(),
            },
            Message::Set {
                id: 10,
                name: "volume".into(),
                value: Value::Double(35.0),
            },
            Message::Get {
                id: 11,
                name: "speed".into(),
            },
            Message::Observe {
                name: "chapter-list".into(),
                format: Format::Node,
            },
            Message::Bye,
        ]
    }

    #[test]
    fn every_message_round_trips() {
        let mut buffer = Vec::new();
        for message in samples() {
            message.encode(&mut buffer).unwrap();
            assert!(buffer.len() <= MAX_MESSAGE);
            assert_eq!(Message::decode(&buffer).unwrap(), message, "{message:?}");
        }
    }

    #[test]
    fn every_format_round_trips() {
        for format in [
            Format::Flag,
            Format::Int64,
            Format::Double,
            Format::String,
            Format::Node,
        ] {
            assert_eq!(Format::from_tag(format.tag()), Ok(format));
        }
        assert_eq!(Format::from_tag(0), Err(DecodeError::UnknownTag(0)));
        assert_eq!(Format::from_tag(6), Err(DecodeError::UnknownTag(6)));
    }

    #[test]
    fn nan_survives_as_nan() {
        let mut buffer = Vec::new();
        let message = Message::Reply {
            id: 1,
            error: 0,
            value: Value::Double(f64::NAN),
        };
        message.encode(&mut buffer).unwrap();
        let Message::Reply {
            value: Value::Double(number),
            ..
        } = Message::decode(&buffer).unwrap()
        else {
            panic!("not a double reply");
        };
        assert!(number.is_nan());
    }

    #[test]
    fn every_truncation_is_an_error_not_a_panic() {
        let mut buffer = Vec::new();
        for message in samples() {
            message.encode(&mut buffer).unwrap();
            for cut in 0..buffer.len() {
                let result = Message::decode(&buffer[..cut]);
                assert!(result.is_err(), "{message:?} cut to {cut} bytes decoded");
            }
        }
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut buffer = Vec::new();
        Message::Bye.encode(&mut buffer).unwrap();
        buffer.push(0);
        assert_eq!(Message::decode(&buffer), Err(DecodeError::Trailing));
    }

    #[test]
    fn malformed_packets_are_named() {
        let cases: &[(&str, &[u8], DecodeError)] = &[
            ("empty", &[], DecodeError::Truncated),
            (
                "unknown message tag",
                &[0xee],
                DecodeError::UnknownTag(0xee),
            ),
            ("tag zero", &[0], DecodeError::UnknownTag(0)),
            (
                "bad utf-8 in a name",
                &[T_GET, 0, 0, 0, 0, 2, 0, 0, 0, 0xff, 0xfe],
                DecodeError::BadUtf8,
            ),
            (
                "unknown value tag",
                &[T_PROPERTY, 0, 0, 0, 0, 99],
                DecodeError::UnknownTag(99),
            ),
            (
                "flag byte 2",
                &[T_PROPERTY, 0, 0, 0, 0, V_FLAG, 2],
                DecodeError::BadFlag(2),
            ),
            (
                "string longer than the packet",
                &[T_GET, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0x7f, b'x'],
                DecodeError::Truncated,
            ),
            (
                "array count larger than the packet",
                &[T_PROPERTY, 0, 0, 0, 0, V_ARRAY, 0xff, 0xff, 0xff, 0xff],
                DecodeError::Truncated,
            ),
            (
                "command count larger than the packet",
                &[T_COMMAND, 0, 0, 0, 0, 0xff, 0xff, 0xff, 0xff],
                DecodeError::Truncated,
            ),
            (
                "observe with an unknown format",
                &[T_OBSERVE, 0, 0, 0, 0, 77],
                DecodeError::UnknownTag(77),
            ),
        ];
        for (name, packet, expected) in cases {
            assert_eq!(Message::decode(packet), Err(*expected), "{name}");
        }
        // A frame is 26 bytes of fixed fields, then the pts presence byte.
        let mut frame = vec![T_FRAME];
        frame.extend_from_slice(&[0; 25]);
        frame.push(5);
        assert_eq!(Message::decode(&frame), Err(DecodeError::BadFlag(5)));
    }

    #[test]
    fn deep_nesting_is_refused() {
        let mut packet = vec![T_PROPERTY, 0, 0, 0, 0];
        for _ in 0..(MAX_DEPTH + 2) {
            packet.extend_from_slice(&[V_ARRAY, 1, 0, 0, 0]);
        }
        packet.push(V_NONE);
        assert_eq!(Message::decode(&packet), Err(DecodeError::TooDeep));
    }

    #[test]
    fn oversized_messages_fail_to_encode() {
        let mut buffer = Vec::new();
        let message = Message::Log {
            level: 0,
            prefix: String::new(),
            text: "x".repeat(MAX_MESSAGE),
        };
        assert_eq!(message.encode(&mut buffer), Err(EncodeError::TooLarge));
        assert!(buffer.is_empty());
        let huge = Message::Property {
            name: "p".into(),
            value: Value::Array(vec![Value::String("y".repeat(1024)); 1024]),
        };
        assert_eq!(huge.encode(&mut buffer), Err(EncodeError::TooLarge));
    }

    #[test]
    fn a_pseudo_random_stream_never_panics() {
        let mut state = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (next() % 64) as usize;
            let mut packet: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            if let Some(first) = packet.first_mut() {
                *first = (next() % 20) as u8;
            }
            let _ = Message::decode(&packet);
        }
    }
}
