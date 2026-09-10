//! macOS helpers for custom titlebar window chrome.
//!
//! GPUI 0.2's Mac `start_window_move` is a no-op. We call AppKit's
//! `performWindowDragWithEvent:` ourselves so a transparent titlebar still
//! drags like a native one.

#![cfg(target_os = "macos")]

use cocoa::appkit::NSApplication;
use cocoa::base::{id, nil};
use objc::{msg_send, sel, sel_impl};

/// Begin a native window drag using the current AppKit event.
// `msg_send!` expands to a `cfg(feature = "cargo-clippy")` test from inside
// the `objc` crate, which this crate has no such feature for. Nothing here can
// fix that; the alternative is a warning on every call.
#[allow(unexpected_cfgs)]
pub fn perform_window_drag() {
    unsafe {
        let app = NSApplication::sharedApplication(nil);
        if app == nil {
            return;
        }
        let event: id = msg_send![app, currentEvent];
        if event == nil {
            return;
        }
        let window: id = msg_send![event, window];
        if window == nil {
            return;
        }
        let _: () = msg_send![window, performWindowDragWithEvent: event];
    }
}
