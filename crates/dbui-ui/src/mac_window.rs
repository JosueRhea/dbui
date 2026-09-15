//! macOS helpers for custom titlebar window chrome.
//!
//! Two things AppKit does for a native titlebar that it will not do for one we
//! draw ourselves, and one thing it does that we have to stop it doing.
//!
//! With a transparent titlebar our chrome sits inside the region AppKit still
//! treats as the title bar, and AppKit drags the whole window from it. It does
//! not ask the content view first -- overriding `mouseDownCanMoveWindow` on
//! gpui's `GPUIView` was tried and changed nothing -- and it decides at
//! mouse-down, before any listener of ours has run, so there is no moment at
//! which we could decline on a per-press basis. The result was that pressing a
//! tab and pulling sideways moved the window: the strip travelled with the
//! pointer, the tab stayed exactly under the cursor, and drag-to-reorder looked
//! dead while in fact never being asked for.
//!
//! So [`disable_native_window_drag`] turns `isMovable` off once and for all,
//! and the window is moved by [`begin_window_drag`] / [`drag_window`] instead
//! -- from the one strip of the bar that is meant to move it. That also
//! replaces `performWindowDragWithEvent:`, which `isMovable` governs too.

#![cfg(target_os = "macos")]

use cocoa::appkit::{NSApplication, NSWindow};
use cocoa::base::{id, nil};
use cocoa::foundation::NSPoint;
use objc::runtime::NO;
use objc::{class, msg_send, sel, sel_impl};
use std::cell::Cell;

/// Where the pointer and the window were when a titlebar drag began, in screen
/// coordinates. The window is placed from these on every move rather than
/// nudged by a delta, so a frame dropped mid-drag cannot leave it adrift.
#[derive(Clone, Copy)]
struct DragAnchor {
    window: NSPoint,
    mouse: NSPoint,
}

thread_local! {
    static ANCHOR: Cell<Option<DragAnchor>> = const { Cell::new(None) };
}

// `msg_send!` expands to a `cfg(feature = "cargo-clippy")` test from inside the
// `objc` crate, which this crate has no such feature for. Nothing here can fix
// that; the alternative is a warning on every call.
#[allow(unexpected_cfgs)]
unsafe fn key_window() -> id {
    let app = NSApplication::sharedApplication(nil);
    if app == nil {
        return nil;
    }
    let window: id = msg_send![app, keyWindow];
    if window != nil {
        return window;
    }
    // Before the window is key -- which is the case at startup, when
    // `disable_native_window_drag` runs.
    let windows: id = msg_send![app, windows];
    let count: usize = msg_send![windows, count];
    if count == 0 {
        return nil;
    }
    msg_send![windows, objectAtIndex: 0]
}

#[allow(unexpected_cfgs)]
unsafe fn pointer_location() -> NSPoint {
    msg_send![class!(NSEvent), mouseLocation]
}

/// Take window movement away from AppKit, for the reasons in the module note.
///
/// Call once the first window exists; before that there is nothing to set it
/// on. Programmatic moves (`setFrameOrigin:`, and so the drag below) are
/// unaffected -- `isMovable` only governs the user-initiated ones.
#[allow(unexpected_cfgs)]
pub fn disable_native_window_drag() {
    unsafe {
        let window = key_window();
        if window == nil {
            return;
        }
        let _: () = msg_send![window, setMovable: NO];
    }
}

/// Remember where the window and the pointer are, ahead of a titlebar drag.
pub fn begin_window_drag() {
    unsafe {
        let window = key_window();
        if window == nil {
            return;
        }
        ANCHOR.with(|anchor| {
            anchor.set(Some(DragAnchor {
                window: window.frame().origin,
                mouse: pointer_location(),
            }))
        });
    }
}

/// Put the window where the pointer has carried it. No-op unless a drag is on.
pub fn drag_window() {
    let Some(anchor) = ANCHOR.with(|anchor| anchor.get()) else {
        return;
    };
    unsafe {
        let window = key_window();
        if window == nil {
            return;
        }
        let now = pointer_location();
        window.setFrameOrigin_(NSPoint::new(
            anchor.window.x + (now.x - anchor.mouse.x),
            anchor.window.y + (now.y - anchor.mouse.y),
        ));
    }
}

/// Let go. Safe to call when no drag is in progress.
pub fn end_window_drag() {
    ANCHOR.with(|anchor| anchor.set(None));
}
