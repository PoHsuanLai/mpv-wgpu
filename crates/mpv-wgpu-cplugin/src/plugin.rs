//! The plugin's thread: the mpv event loop, the render loop, and the socket reader.

use std::ffi::{CString, c_char, c_int, c_void};
use std::os::fd::OwnedFd;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use mpv_wgpu_protocol::{
    Channel, Format, Frame as FrameMessage, Hello, Message, PROTOCOL_VERSION, Ring, RingLayout,
    Value, monotonic_ns,
};

use crate::convert;
use crate::ffi::*;
use crate::schedule::{Action, Frame, Schedule, Slots};

const SOCKET_ENV: &str = "MPV_WGPU_SOCKET";
const PLUGIN_VERSION: &str = env!("CARGO_PKG_VERSION");

/// mpv's client handle. The client API is thread-safe, except that only one
/// thread may call `mpv_wait_event`.
#[derive(Clone, Copy)]
struct Handle(*mut mpv_handle);

// SAFETY: mpv documents the client API as callable from any thread. Only the
// event-loop thread calls `mpv_wait_event`.
unsafe impl Send for Handle {}
// SAFETY: see `Send`.
unsafe impl Sync for Handle {}

struct State {
    ring: Option<Arc<Ring>>,
    slots: Slots,
    schedule: Schedule,
    /// `Presented` messages whose `report_swap` has not been made yet.
    swaps: u32,
    quit: bool,
}

struct Shared {
    handle: Handle,
    channel: Channel,
    /// Set by mpv's render update callback.
    update: AtomicBool,
    state: Mutex<State>,
    sent_drops: Mutex<u64>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn say(text: &str) {
    eprintln!("mpv-wgpu-cplugin: {text}");
}

fn error_text(code: c_int) -> String {
    // SAFETY: `mpv_error_string` returns a static NUL-terminated string.
    unsafe { convert::text(mpv_error_string(code)) }
}

impl Shared {
    fn wake(&self) {
        // SAFETY: the handle is valid until `mpv_open_cplugin` returns, which
        // happens after every thread that holds `Shared` is done.
        unsafe { mpv_wakeup(self.handle.0) };
    }

    fn send(&self, message: &Message) -> bool {
        match self.channel.send(message, None) {
            Ok(()) => true,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::BrokenPipe {
                    say(&format!("send failed: {error}"));
                }
                self.begin_quit();
                false
            }
        }
    }

    /// The player is gone or broken: ask mpv to quit, once.
    fn begin_quit(&self) {
        {
            let mut state = lock(&self.state);
            if state.quit {
                return;
            }
            state.quit = true;
        }
        let quit = c"quit";
        let mut args = [quit.as_ptr(), ptr::null()];
        // SAFETY: a NULL-terminated argument array of valid C strings.
        unsafe { mpv_command_async(self.handle.0, 0, args.as_mut_ptr()) };
        self.wake();
    }
}

unsafe extern "C" fn on_update(context: *mut c_void) {
    // SAFETY: `context` is the `Shared` the loop leaked a reference to, and the
    // callback is removed before that reference is released.
    let shared = unsafe { &*context.cast::<Shared>() };
    shared.update.store(true, Ordering::Release);
    shared.wake();
}

/// Entry point mpv calls on the plugin's own thread.
///
/// # Safety
///
/// Called by mpv with a valid client handle that stays valid until this returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn mpv_open_cplugin(handle: *mut c_void) -> c_int {
    let result = catch_unwind(AssertUnwindSafe(|| run(Handle(handle.cast()))));
    match result {
        Ok(code) => code,
        Err(_) => {
            say("panicked; leaving mpv running without a player");
            -1
        }
    }
}

fn run(handle: Handle) -> c_int {
    let Ok(name) = std::env::var(SOCKET_ENV) else {
        say(&format!(
            "{SOCKET_ENV} is not set; this plugin only runs under mpv-wgpu-player"
        ));
        return -1;
    };
    // The context must exist before mpv opens a file: `vo=libmpv` waits for it.
    let mut context: *mut mpv_render_context = ptr::null_mut();
    let mut params = [
        mpv_render_param {
            kind: MPV_RENDER_PARAM_API_TYPE,
            data: c"sw".as_ptr().cast_mut().cast(),
        },
        mpv_render_param {
            kind: 0,
            data: ptr::null_mut(),
        },
    ];
    // SAFETY: valid handle; `params` is a zero-terminated parameter list.
    let created = unsafe { mpv_render_context_create(&mut context, handle.0, params.as_mut_ptr()) };
    if created < 0 {
        say(&format!(
            "cannot create a software render context: {}",
            error_text(created)
        ));
        return -1;
    }
    let channel = match Channel::connect_abstract(&name) {
        Ok(channel) => channel,
        Err(error) => {
            say(&format!("cannot connect to the player: {error}"));
            // SAFETY: the context was created above and is not in use.
            unsafe { mpv_render_context_free(context) };
            return -1;
        }
    };
    let shared = Arc::new(Shared {
        handle,
        channel,
        update: AtomicBool::new(false),
        state: Mutex::new(State {
            ring: None,
            slots: Slots::new(),
            schedule: Schedule::new(),
            swaps: 0,
            quit: false,
        }),
        sent_drops: Mutex::new(0),
    });
    let shared_pointer = Arc::as_ptr(&shared).cast_mut().cast::<c_void>();
    // SAFETY: `shared` outlives the callback: it is cleared before `shared` drops.
    unsafe {
        mpv_render_context_set_update_callback(context, Some(on_update), shared_pointer);
    }
    // SAFETY: valid handle and a static C string.
    unsafe { mpv_request_log_messages(handle.0, c"warn".as_ptr()) };

    let hello = Message::Hello(Hello {
        protocol: PROTOCOL_VERSION,
        client_api: unsafe { mpv_client_api_version() } as u32,
        mpv_version: property_string(handle, c"mpv-version").unwrap_or_default(),
        plugin_version: PLUGIN_VERSION.to_string(),
    });
    if shared.send(&hello) {
        let reader = {
            let shared = Arc::clone(&shared);
            thread::Builder::new()
                .name("mpv-wgpu-reader".into())
                .spawn(move || read_loop(&shared))
        };
        match reader {
            Ok(reader) => {
                event_loop(&shared, context);
                shared.channel.shutdown();
                let _ = reader.join();
            }
            Err(error) => say(&format!("cannot start the reader thread: {error}")),
        }
    }
    // SAFETY: no thread calls the render API any more. Clearing the callback
    // first guarantees `shared` is not used after the free.
    unsafe {
        mpv_render_context_set_update_callback(context, None, ptr::null_mut());
        mpv_render_context_free(context);
    }
    0
}

fn property_string(handle: Handle, name: &std::ffi::CStr) -> Option<String> {
    let mut out: *mut c_char = ptr::null_mut();
    // SAFETY: valid handle and name; `out` receives an mpv-allocated string.
    let code = unsafe {
        mpv_get_property(
            handle.0,
            name.as_ptr(),
            MPV_FORMAT_STRING,
            (&raw mut out).cast(),
        )
    };
    if code < 0 || out.is_null() {
        return None;
    }
    // SAFETY: mpv returned a NUL-terminated string that we free right after.
    let text = unsafe { convert::text(out) };
    // SAFETY: `out` came from mpv_get_property and is freed once.
    unsafe { mpv_free(out.cast()) };
    Some(text)
}

fn property_double(handle: Handle, name: &std::ffi::CStr) -> Option<f64> {
    let mut out = 0.0f64;
    // SAFETY: valid handle and name; `out` is a double as the format says.
    let code = unsafe {
        mpv_get_property(
            handle.0,
            name.as_ptr(),
            MPV_FORMAT_DOUBLE,
            (&raw mut out).cast(),
        )
    };
    (code >= 0 && out.is_finite()).then_some(out)
}

// ---- socket reader -------------------------------------------------------

fn read_loop(shared: &Shared) {
    let mut scratch = Vec::new();
    loop {
        match shared.channel.recv(&mut scratch) {
            Ok(Some((message, fd))) => {
                if !handle_message(shared, message, fd) {
                    break;
                }
            }
            Ok(None) => break,
            Err(error) => {
                say(&format!("receive failed: {error}"));
                break;
            }
        }
    }
    shared.begin_quit();
}

/// Act on one message from the player. False ends the reader.
fn handle_message(shared: &Shared, message: Message, fd: Option<OwnedFd>) -> bool {
    match message {
        Message::Resize(resize) => {
            let Some(fd) = fd else {
                say("resize arrived without a memfd");
                return false;
            };
            let layout = match RingLayout::new(
                resize.generation,
                resize.width,
                resize.height,
                resize.stride,
                resize.slots,
            ) {
                Ok(layout) => layout,
                Err(error) => {
                    say(&format!("{error}"));
                    return false;
                }
            };
            match Ring::from_fd(fd, layout) {
                Ok(ring) => {
                    let mut state = lock(&shared.state);
                    state.ring = Some(Arc::new(ring));
                    state.slots.reset(resize.generation, resize.slots);
                    state.schedule.resized();
                }
                Err(error) => {
                    say(&format!("cannot map the ring: {error}"));
                    return false;
                }
            }
            shared.wake();
        }
        Message::Presented(slot) => {
            let mut state = lock(&shared.state);
            state.slots.release(slot.generation, slot.slot);
            state.swaps += 1;
            drop(state);
            shared.wake();
        }
        Message::Released(slot) => {
            lock(&shared.state)
                .slots
                .release(slot.generation, slot.slot);
            shared.wake();
        }
        Message::Command { id, args } => command(shared, id, &args),
        Message::Set { id, name, value } => {
            let error = set_property(shared.handle, &name, &value);
            shared.send(&Message::Reply {
                id,
                error,
                value: Value::None,
            });
        }
        Message::Get { id, name } => {
            let (error, value) = get_property(shared.handle, &name);
            shared.send(&Message::Reply { id, error, value });
        }
        Message::Observe { name, format } => observe(shared.handle, &name, format),
        Message::Bye => return false,
        other => say(&format!("unexpected message from the player: {other:?}")),
    }
    true
}

const MPV_ERROR_INVALID_PARAMETER: c_int = -4;

fn command(shared: &Shared, id: u32, args: &[String]) {
    let owned: Result<Vec<CString>, _> = args.iter().map(|a| CString::new(a.as_str())).collect();
    let Ok(owned) = owned else {
        shared.send(&Message::Reply {
            id,
            error: MPV_ERROR_INVALID_PARAMETER,
            value: Value::None,
        });
        return;
    };
    let mut pointers: Vec<*const c_char> = owned.iter().map(|a| a.as_ptr()).collect();
    pointers.push(ptr::null());
    // SAFETY: a NULL-terminated array of valid C strings; mpv copies them
    // before returning.
    let code = unsafe { mpv_command_async(shared.handle.0, u64::from(id), pointers.as_mut_ptr()) };
    if code < 0 {
        shared.send(&Message::Reply {
            id,
            error: code,
            value: Value::None,
        });
    }
}

fn set_property(handle: Handle, name: &str, value: &Value) -> c_int {
    let Ok(name) = CString::new(name) else {
        return MPV_ERROR_INVALID_PARAMETER;
    };
    match value {
        Value::Flag(flag) => {
            let mut data: c_int = c_int::from(*flag);
            // SAFETY: valid handle and name; `data` is an int as the format says.
            unsafe {
                mpv_set_property(
                    handle.0,
                    name.as_ptr(),
                    MPV_FORMAT_FLAG,
                    (&raw mut data).cast(),
                )
            }
        }
        Value::Int64(number) => {
            let mut data = *number;
            // SAFETY: as above, with an int64.
            unsafe {
                mpv_set_property(
                    handle.0,
                    name.as_ptr(),
                    MPV_FORMAT_INT64,
                    (&raw mut data).cast(),
                )
            }
        }
        Value::Double(number) => {
            let mut data = *number;
            // SAFETY: as above, with a double.
            unsafe {
                mpv_set_property(
                    handle.0,
                    name.as_ptr(),
                    MPV_FORMAT_DOUBLE,
                    (&raw mut data).cast(),
                )
            }
        }
        Value::String(text) => {
            let Ok(text) = CString::new(text.as_str()) else {
                return MPV_ERROR_INVALID_PARAMETER;
            };
            let mut data: *const c_char = text.as_ptr();
            // SAFETY: a string property takes a `char *` pointer, valid for the call.
            unsafe {
                mpv_set_property(
                    handle.0,
                    name.as_ptr(),
                    MPV_FORMAT_STRING,
                    (&raw mut data).cast(),
                )
            }
        }
        Value::None | Value::Array(_) | Value::Map(_) => MPV_ERROR_INVALID_PARAMETER,
    }
}

fn get_property(handle: Handle, name: &str) -> (c_int, Value) {
    let Ok(name) = CString::new(name) else {
        return (MPV_ERROR_INVALID_PARAMETER, Value::None);
    };
    let mut node = mpv_node {
        u: mpv_node_union { int64: 0 },
        format: MPV_FORMAT_NONE,
    };
    // SAFETY: valid handle and name; `node` receives an mpv-owned node.
    let code = unsafe {
        mpv_get_property(
            handle.0,
            name.as_ptr(),
            MPV_FORMAT_NODE,
            (&raw mut node).cast(),
        )
    };
    if code < 0 {
        return (code, Value::None);
    }
    // SAFETY: mpv filled `node`; it is freed right after the copy.
    let value = unsafe { convert::node(&node) };
    // SAFETY: `node` was filled by mpv_get_property and is freed once.
    unsafe { mpv_free_node_contents(&mut node) };
    (0, value)
}

fn observe(handle: Handle, name: &str, format: Format) {
    let Ok(c_name) = CString::new(name) else {
        return;
    };
    let format = match format {
        Format::Flag => MPV_FORMAT_FLAG,
        Format::Int64 => MPV_FORMAT_INT64,
        Format::Double => MPV_FORMAT_DOUBLE,
        Format::String => MPV_FORMAT_STRING,
        Format::Node => MPV_FORMAT_NODE,
    };
    // SAFETY: valid handle and name.
    let code = unsafe { mpv_observe_property(handle.0, 0, c_name.as_ptr(), format) };
    if code < 0 {
        say(&format!("cannot observe {name}: {}", error_text(code)));
    }
}

// ---- mpv event loop and rendering -----------------------------------------

fn event_loop(shared: &Shared, context: *mut mpv_render_context) {
    let mut seq: u64 = 0;
    let mut quit_at: Option<Instant> = None;
    loop {
        // SAFETY: valid handle; the returned event is read before the next wait.
        let event = unsafe { mpv_wait_event(shared.handle.0, 0.25) };
        // SAFETY: mpv_wait_event never returns null and the event is valid until the next call.
        let event = unsafe { &*event };
        if event.event_id == MPV_EVENT_SHUTDOWN {
            shared.send(&Message::Bye);
            return;
        }
        // SAFETY: the event is valid and its data matches its id.
        unsafe { forward_event(shared, event) };
        pump_frames(shared, context, &mut seq);
        if lock(&shared.state).quit {
            let started = *quit_at.get_or_insert_with(Instant::now);
            if started.elapsed() > Duration::from_secs(3) {
                return;
            }
        }
    }
}

/// Send one mpv event to the player.
///
/// # Safety
///
/// `event` is a live event from `mpv_wait_event`.
unsafe fn forward_event(shared: &Shared, event: &mpv_event) {
    match event.event_id {
        MPV_EVENT_NONE => {}
        MPV_EVENT_FILE_LOADED => {
            shared.send(&Message::FileLoaded);
        }
        MPV_EVENT_END_FILE if !event.data.is_null() => {
            // SAFETY: END_FILE data is an `mpv_event_end_file`.
            let end = unsafe { &*event.data.cast::<mpv_event_end_file>() };
            shared.send(&Message::EndFile {
                reason: u8::try_from(end.reason).unwrap_or(u8::MAX),
                error: end.error,
            });
        }
        MPV_EVENT_PROPERTY_CHANGE if !event.data.is_null() => {
            // SAFETY: PROPERTY_CHANGE data is an `mpv_event_property`.
            let property = unsafe { &*event.data.cast::<mpv_event_property>() };
            // SAFETY: the name is a valid C string and `data` matches `format`.
            let (name, value) = unsafe {
                (
                    convert::text(property.name),
                    convert::property(property.format, property.data),
                )
            };
            shared.send(&Message::Property { name, value });
        }
        MPV_EVENT_LOG_MESSAGE if !event.data.is_null() => {
            // SAFETY: LOG_MESSAGE data is an `mpv_event_log_message`.
            let log = unsafe { &*event.data.cast::<mpv_event_log_message>() };
            // SAFETY: both are valid C strings.
            let (prefix, text) = unsafe { (convert::text(log.prefix), convert::text(log.text)) };
            shared.send(&Message::Log {
                level: u8::try_from(log.log_level).unwrap_or(u8::MAX),
                prefix,
                text,
            });
        }
        MPV_EVENT_COMMAND_REPLY => {
            let value = if event.error >= 0 && !event.data.is_null() {
                // SAFETY: COMMAND_REPLY data is an `mpv_event_command`.
                let reply = unsafe { &*event.data.cast::<mpv_event_command>() };
                // SAFETY: the node is valid for this event.
                unsafe { convert::node(&reply.result) }
            } else {
                Value::None
            };
            shared.send(&Message::Reply {
                id: u32::try_from(event.reply_userdata).unwrap_or(0),
                error: event.error,
                value,
            });
        }
        MPV_EVENT_QUEUE_OVERFLOW => say("mpv dropped events: the plugin was too slow"),
        _ => {}
    }
}

/// Report swaps the player acknowledged, then render if a draw is owed.
fn pump_frames(shared: &Shared, context: *mut mpv_render_context, seq: &mut u64) {
    let swaps = std::mem::take(&mut lock(&shared.state).swaps);
    for _ in 0..swaps {
        // SAFETY: the context is live for the whole event loop.
        unsafe { mpv_render_context_report_swap(context) };
    }

    let frame = if shared.update.swap(false, Ordering::AcqRel) {
        // SAFETY: the context is live.
        let flags = unsafe { mpv_render_context_update(context) };
        if flags & MPV_RENDER_UPDATE_FRAME == 0 {
            Frame::Absent
        } else {
            next_frame(context)
        }
    } else {
        Frame::Absent
    };

    let action = {
        let mut state = lock(&shared.state);
        // Nothing can be owed before a ring exists, except the swap of a repeat.
        let action = state.schedule.plan(frame);
        if state.ring.is_none() && action == Action::Render {
            Action::Nothing
        } else {
            action
        }
    };
    match action {
        Action::Nothing => {}
        Action::SwapOnly => {
            // SAFETY: the context is live.
            unsafe { mpv_render_context_report_swap(context) };
        }
        Action::Render => render(shared, context, seq),
    }
    report_drops(shared);
}

fn next_frame(context: *mut mpv_render_context) -> Frame {
    let mut info = mpv_render_frame_info::default();
    // SAFETY: the parameter points to an `mpv_render_frame_info` as the type says.
    let code = unsafe {
        mpv_render_context_get_info(
            context,
            mpv_render_param {
                kind: MPV_RENDER_PARAM_NEXT_FRAME_INFO,
                data: (&raw mut info).cast(),
            },
        )
    };
    if code < 0 || info.flags & MPV_RENDER_FRAME_INFO_PRESENT == 0 {
        Frame::Absent
    } else if info.flags & MPV_RENDER_FRAME_INFO_REPEAT != 0 {
        Frame::Repeat
    } else if info.flags & MPV_RENDER_FRAME_INFO_REDRAW != 0 {
        Frame::Redraw
    } else {
        Frame::New
    }
}

fn render(shared: &Shared, context: *mut mpv_render_context, seq: &mut u64) {
    let (ring, slot, generation) = {
        let mut state = lock(&shared.state);
        let Some(ring) = state.ring.clone() else {
            return;
        };
        let Some(slot) = state.slots.acquire() else {
            return;
        };
        (ring, slot, state.slots.generation())
    };
    let layout = ring.layout();
    let Some(pointer) = ring.slot_ptr(slot) else {
        return;
    };
    let mut size: [c_int; 2] = [
        c_int::try_from(layout.width()).unwrap_or(0),
        c_int::try_from(layout.height()).unwrap_or(0),
    ];
    let mut stride = layout.stride() as usize;
    let mut params = [
        mpv_render_param {
            kind: MPV_RENDER_PARAM_SW_SIZE,
            data: size.as_mut_ptr().cast(),
        },
        mpv_render_param {
            kind: MPV_RENDER_PARAM_SW_FORMAT,
            data: c"rgb0".as_ptr().cast_mut().cast(),
        },
        mpv_render_param {
            kind: MPV_RENDER_PARAM_SW_STRIDE,
            data: (&raw mut stride).cast(),
        },
        mpv_render_param {
            kind: MPV_RENDER_PARAM_SW_POINTER,
            data: pointer.as_ptr().cast(),
        },
        mpv_render_param {
            kind: 0,
            data: ptr::null_mut(),
        },
    ];
    // SAFETY: the context is live; the pointer is a slot of `slot_len` bytes
    // (`stride * height`) inside a mapping `ring` keeps alive for this call,
    // and the slot is ours until the player returns it.
    let code = unsafe { mpv_render_context_render(context, params.as_mut_ptr()) };
    if code < 0 {
        say(&format!("render failed: {}", error_text(code)));
        let mut state = lock(&shared.state);
        state.slots.release(generation, slot);
        state.schedule.rendered();
        return;
    }
    let pts = property_double(shared.handle, c"time-pos");
    *seq += 1;
    lock(&shared.state).schedule.rendered();
    shared.send(&Message::Frame(FrameMessage {
        generation,
        slot,
        seq: *seq,
        width: layout.width(),
        height: layout.height(),
        stride: layout.stride(),
        pts,
        sent_ns: monotonic_ns(),
    }));
}

fn report_drops(shared: &Shared) {
    let dropped = lock(&shared.state).schedule.dropped();
    let mut sent = lock(&shared.sent_drops);
    if dropped != *sent {
        *sent = dropped;
        drop(sent);
        shared.send(&Message::Dropped { count: dropped });
    }
}
