//! The socket to the plugin, the thread that reads it, and the inbox it fills.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mpv_wgpu_protocol::{Channel, Frame, Message, SlotRef, Value};

use crate::core::CoreEvent;
use crate::notify::{Notify, lock};
use crate::types::{EndReason, Error};
use crate::value::{Node, PropertyData};

/// How long a command, property write or property read may wait for mpv.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
struct Inbox {
    events: VecDeque<CoreEvent>,
    frame: Option<Frame>,
    replies: HashMap<u32, (i32, Value)>,
    gone: bool,
    dropped: u64,
}

pub(super) struct Link {
    channel: Channel,
    inbox: Mutex<Inbox>,
    replied: Condvar,
    notify: Arc<Notify>,
    next_id: AtomicU32,
}

impl Link {
    pub(super) fn new(channel: Channel, notify: Arc<Notify>) -> Arc<Self> {
        Arc::new(Self {
            channel,
            inbox: Mutex::new(Inbox::default()),
            replied: Condvar::new(),
            notify,
            next_id: AtomicU32::new(1),
        })
    }

    pub(super) fn spawn_reader(self: &Arc<Self>) -> std::io::Result<JoinHandle<()>> {
        let link = Arc::clone(self);
        thread::Builder::new()
            .name("mpv-wgpu-link".into())
            .spawn(move || link.read_loop())
    }

    pub(super) fn send(
        &self,
        message: &Message,
        fd: Option<std::os::fd::BorrowedFd<'_>>,
    ) -> Result<(), Error> {
        self.channel.send(message, fd).map_err(|error| {
            log::debug!("send to the mpv plugin failed: {error}");
            Error::HostGone
        })
    }

    pub(super) fn shutdown(&self) {
        self.channel.shutdown();
    }

    /// Send a request and wait for its reply. Returns mpv's error code and the value.
    pub(super) fn request(&self, make: impl FnOnce(u32) -> Message) -> Result<(i32, Value), Error> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if lock(&self.inbox).gone {
            return Err(Error::HostGone);
        }
        self.send(&make(id), None)?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        let mut inbox = lock(&self.inbox);
        loop {
            if let Some(reply) = inbox.replies.remove(&id) {
                return Ok(reply);
            }
            if inbox.gone {
                return Err(Error::HostGone);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(Error::HostTimeout);
            }
            inbox = self
                .replied
                .wait_timeout(inbox, left)
                .unwrap_or_else(|poison| poison.into_inner())
                .0;
        }
    }

    pub(super) fn next_event(&self) -> Option<CoreEvent> {
        lock(&self.inbox).events.pop_front()
    }

    pub(super) fn take_frame(&self) -> Option<Frame> {
        lock(&self.inbox).frame.take()
    }

    pub(super) fn is_gone(&self) -> bool {
        lock(&self.inbox).gone
    }

    pub(super) fn has_events(&self) -> bool {
        !lock(&self.inbox).events.is_empty()
    }

    pub(super) fn dropped(&self) -> u64 {
        lock(&self.inbox).dropped
    }

    fn read_loop(&self) {
        let mut scratch = Vec::new();
        loop {
            match self.channel.recv(&mut scratch) {
                Ok(Some((message, _fd))) => {
                    if !self.handle(message) {
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    log::warn!("the mpv plugin sent something unreadable: {error}");
                    break;
                }
            }
        }
        lock(&self.inbox).gone = true;
        self.replied.notify_all();
        self.notify.signal();
    }

    /// Fold one message from the plugin in. False ends the reader.
    fn handle(&self, message: Message) -> bool {
        match message {
            Message::Frame(frame) => {
                let superseded = lock(&self.inbox).frame.replace(frame);
                if let Some(old) = superseded {
                    // The player never saw it: free the slot without a swap report.
                    let _ = self.channel.send(
                        &Message::Released(SlotRef {
                            generation: old.generation,
                            slot: old.slot,
                        }),
                        None,
                    );
                }
                self.notify.signal();
            }
            Message::Property { name, value } => {
                self.push(CoreEvent::Property(name, property_data(value)));
            }
            Message::FileLoaded => self.push(CoreEvent::FileLoaded),
            Message::EndFile { reason, .. } => self.push(CoreEvent::Ended(end_reason(reason))),
            Message::Log { prefix, text, .. } => self.push(CoreEvent::Log { prefix, text }),
            Message::Reply { id, error, value } => {
                lock(&self.inbox).replies.insert(id, (error, value));
                self.replied.notify_all();
            }
            Message::Dropped { count } => lock(&self.inbox).dropped = count,
            Message::Bye => return false,
            other => log::warn!("unexpected message from the mpv plugin: {other:?}"),
        }
        true
    }

    fn push(&self, event: CoreEvent) {
        lock(&self.inbox).events.push_back(event);
        self.notify.signal();
    }
}

/// mpv's `mpv_end_file_reason` numbers.
fn end_reason(reason: u8) -> EndReason {
    match reason {
        0 => EndReason::Eof,
        2 => EndReason::Stop,
        3 => EndReason::Quit,
        5 => EndReason::Redirect,
        _ => EndReason::Error,
    }
}

fn property_data(value: Value) -> PropertyData {
    match value {
        Value::None => PropertyData::None,
        Value::Flag(flag) => PropertyData::Flag(flag),
        Value::Int64(number) => PropertyData::Int64(number),
        Value::Double(number) => PropertyData::Double(number),
        Value::String(text) => PropertyData::String(text),
        other @ (Value::Array(_) | Value::Map(_)) => PropertyData::Node(node(other)),
    }
}

fn node(value: Value) -> Node {
    match value {
        Value::None => Node::None,
        Value::Flag(flag) => Node::Flag(flag),
        Value::Int64(number) => Node::Int64(number),
        Value::Double(number) => Node::Double(number),
        Value::String(text) => Node::String(text),
        Value::Array(items) => Node::Array(items.into_iter().map(node).collect()),
        Value::Map(entries) => Node::Map(
            entries
                .into_iter()
                .map(|(key, item)| (key, node(item)))
                .collect(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn end_reasons_follow_mpv_numbers() {
        let cases = [
            (0, EndReason::Eof),
            (2, EndReason::Stop),
            (3, EndReason::Quit),
            (4, EndReason::Error),
            (5, EndReason::Redirect),
            (99, EndReason::Error),
        ];
        for (number, expected) in cases {
            assert_eq!(end_reason(number), expected, "{number}");
        }
    }

    #[test]
    fn structured_values_become_nodes() {
        let value = Value::Array(vec![Value::Map(vec![
            ("id".into(), Value::Int64(1)),
            ("selected".into(), Value::Flag(true)),
        ])]);
        assert_eq!(
            property_data(value),
            PropertyData::Node(Node::Array(vec![Node::Map(vec![
                ("id".into(), Node::Int64(1)),
                ("selected".into(), Node::Flag(true)),
            ])]))
        );
        assert_eq!(property_data(Value::None), PropertyData::None);
        assert_eq!(property_data(Value::Double(2.5)), PropertyData::Double(2.5));
    }
}
