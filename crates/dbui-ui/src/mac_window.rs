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
use cocoa::foundation::{NSAutoreleasePool, NSPoint, NSRect, NSString};
use objc::runtime::{BOOL, NO, YES};
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

/// Tags our effect view so a later call can find it again.
const VIBRANCY_ID: &str = "dbui.vibrancy";

/// Put AppKit's sidebar material behind the window's content, or take it away.
///
/// gpui's own `Blurred` background strips the material's tint and saturation
/// and leaves a bare blur, which over a light desktop reads as mud rather than
/// glass. This is the material Finder's sidebar uses, tinted by AppKit to the
/// window's appearance -- which is pinned to the theme's, not the system's, so
/// a dark theme gets dark glass on a light-mode Mac. The window must already be
/// non-opaque (`WindowBackgroundAppearance::Transparent`) for it to show.
///
/// `light` is `None` to remove the material.
#[allow(unexpected_cfgs)]
pub fn set_vibrancy(light: Option<bool>) {
    unsafe {
        let window = key_window();
        if window == nil {
            return;
        }
        let content: id = msg_send![window, contentView];
        if content == nil {
            return;
        }
        let identifier = NSString::alloc(nil).init_str(VIBRANCY_ID).autorelease();
        let existing = vibrancy_view(content, identifier);

        let Some(light) = light else {
            if existing != nil {
                let _: () = msg_send![existing, removeFromSuperview];
            }
            let _: () = msg_send![window, setAppearance: nil];
            return;
        };

        let name = NSString::alloc(nil)
            .init_str(if light {
                "NSAppearanceNameAqua"
            } else {
                "NSAppearanceNameDarkAqua"
            })
            .autorelease();
        let appearance: id = msg_send![class!(NSAppearance), appearanceNamed: name];
        let _: () = msg_send![window, setAppearance: appearance];

        if existing != nil {
            return;
        }
        let frame: NSRect = msg_send![content, bounds];
        let view: id = msg_send![class!(NSVisualEffectView), alloc];
        let view: id = msg_send![view, initWithFrame: frame];
        // NSVisualEffectMaterialSidebar, blended with what is behind the
        // window, and kept lit when the window is not key so the chrome does
        // not flash grey every time focus moves to another app.
        let _: () = msg_send![view, setMaterial: 7_isize];
        let _: () = msg_send![view, setBlendingMode: 0_isize];
        let _: () = msg_send![view, setState: 1_isize];
        // NSViewWidthSizable | NSViewHeightSizable
        let _: () = msg_send![view, setAutoresizingMask: 2_usize | 16_usize];
        let _: () = msg_send![view, setIdentifier: identifier];
        // Below gpui's view, which is the one that draws everything else.
        let _: () = msg_send![content, addSubview: view positioned: -1_isize relativeTo: nil];
        let _: () = msg_send![view, release];
    }
}

/// Our effect view, if the window has one.
#[allow(unexpected_cfgs)]
unsafe fn vibrancy_view(content: id, identifier: id) -> id {
    let subviews: id = msg_send![content, subviews];
    let count: usize = msg_send![subviews, count];
    for index in 0..count {
        let view: id = msg_send![subviews, objectAtIndex: index];
        let view_id: id = msg_send![view, identifier];
        if view_id != nil {
            let same: BOOL = msg_send![view_id, isEqualToString: identifier];
            if same == YES {
                return view;
            }
        }
    }
    nil
}

/// Set how far the material blurs what is behind the window, in points.
///
/// AppKit has no public knob for this. The material draws through a
/// `CABackdropLayer` whose filters include one named `gaussianBlur`, and its
/// `inputRadius` is what this sets. AppKit rebuilds the filters when it
/// restyles the view -- a change of appearance, say -- so this is cheap to
/// call every frame: it only writes when the radius has drifted. No-op when
/// there is no material, and if a future macOS names things differently the
/// material simply keeps its own blur.
#[allow(unexpected_cfgs)]
pub fn set_vibrancy_blur(radius: f64) {
    unsafe {
        let window = key_window();
        if window == nil {
            return;
        }
        let content: id = msg_send![window, contentView];
        if content == nil {
            return;
        }
        let identifier = NSString::alloc(nil).init_str(VIBRANCY_ID).autorelease();
        let view = vibrancy_view(content, identifier);
        if view == nil {
            return;
        }
        let layer: id = msg_send![view, layer];
        let backdrop = find_backdrop(layer);
        if backdrop == nil {
            return;
        }
        let key_path = NSString::alloc(nil)
            .init_str("filters.gaussianBlur.inputRadius")
            .autorelease();
        let current: id = msg_send![backdrop, valueForKeyPath: key_path];
        if current != nil {
            let value: f64 = msg_send![current, doubleValue];
            if (value - radius).abs() < 0.01 {
                return;
            }
        }
        let number: id = msg_send![class!(NSNumber), numberWithDouble: radius];
        let _: () = msg_send![backdrop, setValue: number forKeyPath: key_path];
    }
}

/// The first `CABackdropLayer` under `layer`, depth first.
#[allow(unexpected_cfgs)]
unsafe fn find_backdrop(layer: id) -> id {
    if layer == nil {
        return nil;
    }
    let Some(backdrop_class) = objc::runtime::Class::get("CABackdropLayer") else {
        return nil;
    };
    let is_backdrop: BOOL = msg_send![layer, isKindOfClass: backdrop_class];
    if is_backdrop == YES {
        return layer;
    }
    let sublayers: id = msg_send![layer, sublayers];
    if sublayers == nil {
        return nil;
    }
    let count: usize = msg_send![sublayers, count];
    for index in 0..count {
        let found = find_backdrop(msg_send![sublayers, objectAtIndex: index]);
        if found != nil {
            return found;
        }
    }
    nil
}
