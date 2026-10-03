//! The slice of mpv's client and render API this plugin calls.
//!
//! These are declared without `#[link]`. The plugin is loaded into an `mpv`
//! process that exports the symbols, so the dynamic linker binds them there.
//! The layouts follow `mpv/client.h` and `mpv/render.h` (client API 2.x).

#![allow(non_camel_case_types, dead_code)]

use std::ffi::{c_char, c_int, c_ulong, c_void};

pub(crate) enum mpv_handle {}
pub(crate) enum mpv_render_context {}

pub(crate) const MPV_FORMAT_NONE: c_int = 0;
pub(crate) const MPV_FORMAT_STRING: c_int = 1;
pub(crate) const MPV_FORMAT_FLAG: c_int = 3;
pub(crate) const MPV_FORMAT_INT64: c_int = 4;
pub(crate) const MPV_FORMAT_DOUBLE: c_int = 5;
pub(crate) const MPV_FORMAT_NODE: c_int = 6;
pub(crate) const MPV_FORMAT_NODE_ARRAY: c_int = 7;
pub(crate) const MPV_FORMAT_NODE_MAP: c_int = 8;

pub(crate) const MPV_EVENT_NONE: c_int = 0;
pub(crate) const MPV_EVENT_SHUTDOWN: c_int = 1;
pub(crate) const MPV_EVENT_LOG_MESSAGE: c_int = 2;
pub(crate) const MPV_EVENT_COMMAND_REPLY: c_int = 5;
pub(crate) const MPV_EVENT_END_FILE: c_int = 7;
pub(crate) const MPV_EVENT_FILE_LOADED: c_int = 8;
pub(crate) const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;
pub(crate) const MPV_EVENT_QUEUE_OVERFLOW: c_int = 24;

pub(crate) const MPV_RENDER_PARAM_API_TYPE: c_int = 1;
pub(crate) const MPV_RENDER_PARAM_NEXT_FRAME_INFO: c_int = 11;
pub(crate) const MPV_RENDER_PARAM_SW_SIZE: c_int = 17;
pub(crate) const MPV_RENDER_PARAM_SW_FORMAT: c_int = 18;
pub(crate) const MPV_RENDER_PARAM_SW_STRIDE: c_int = 19;
pub(crate) const MPV_RENDER_PARAM_SW_POINTER: c_int = 20;

pub(crate) const MPV_RENDER_UPDATE_FRAME: u64 = 1 << 0;

pub(crate) const MPV_RENDER_FRAME_INFO_PRESENT: u64 = 1 << 0;
pub(crate) const MPV_RENDER_FRAME_INFO_REDRAW: u64 = 1 << 1;
pub(crate) const MPV_RENDER_FRAME_INFO_REPEAT: u64 = 1 << 2;

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) union mpv_node_union {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: f64,
    pub list: *mut mpv_node_list,
    pub ba: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(crate) struct mpv_node {
    pub u: mpv_node_union,
    pub format: c_int,
}

#[repr(C)]
pub(crate) struct mpv_node_list {
    pub num: c_int,
    pub values: *mut mpv_node,
    pub keys: *mut *mut c_char,
}

#[repr(C)]
pub(crate) struct mpv_event {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

#[repr(C)]
pub(crate) struct mpv_event_property {
    pub name: *const c_char,
    pub format: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
pub(crate) struct mpv_event_end_file {
    pub reason: c_int,
    pub error: c_int,
}

#[repr(C)]
pub(crate) struct mpv_event_log_message {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

#[repr(C)]
pub(crate) struct mpv_event_command {
    pub result: mpv_node,
}

#[repr(C)]
pub(crate) struct mpv_render_param {
    pub kind: c_int,
    pub data: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
pub(crate) struct mpv_render_frame_info {
    pub flags: u64,
    pub target_time: i64,
}

pub(crate) type mpv_render_update_fn = unsafe extern "C" fn(*mut c_void);

unsafe extern "C" {
    pub(crate) fn mpv_client_api_version() -> c_ulong;
    pub(crate) fn mpv_error_string(error: c_int) -> *const c_char;
    pub(crate) fn mpv_free(data: *mut c_void);
    pub(crate) fn mpv_free_node_contents(node: *mut mpv_node);
    pub(crate) fn mpv_wait_event(ctx: *mut mpv_handle, timeout: f64) -> *mut mpv_event;
    pub(crate) fn mpv_wakeup(ctx: *mut mpv_handle);
    pub(crate) fn mpv_command_async(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        args: *mut *const c_char,
    ) -> c_int;
    pub(crate) fn mpv_set_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    pub(crate) fn mpv_get_property(
        ctx: *mut mpv_handle,
        name: *const c_char,
        format: c_int,
        data: *mut c_void,
    ) -> c_int;
    pub(crate) fn mpv_observe_property(
        ctx: *mut mpv_handle,
        reply_userdata: u64,
        name: *const c_char,
        format: c_int,
    ) -> c_int;
    pub(crate) fn mpv_request_log_messages(ctx: *mut mpv_handle, min_level: *const c_char)
    -> c_int;
    pub(crate) fn mpv_render_context_create(
        res: *mut *mut mpv_render_context,
        mpv: *mut mpv_handle,
        params: *mut mpv_render_param,
    ) -> c_int;
    pub(crate) fn mpv_render_context_get_info(
        ctx: *mut mpv_render_context,
        param: mpv_render_param,
    ) -> c_int;
    pub(crate) fn mpv_render_context_set_update_callback(
        ctx: *mut mpv_render_context,
        callback: Option<mpv_render_update_fn>,
        callback_ctx: *mut c_void,
    );
    pub(crate) fn mpv_render_context_update(ctx: *mut mpv_render_context) -> u64;
    pub(crate) fn mpv_render_context_render(
        ctx: *mut mpv_render_context,
        params: *mut mpv_render_param,
    ) -> c_int;
    pub(crate) fn mpv_render_context_report_swap(ctx: *mut mpv_render_context);
    pub(crate) fn mpv_render_context_free(ctx: *mut mpv_render_context);
}
