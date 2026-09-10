//! Scrollbars.
//!
//! GPUI scrolls a container but draws nothing to say that it did. A table
//! taller than the pane looks exactly like a table that ends there, so the
//! only way to learn there is more is to try the wheel and see. This is the
//! missing half: a thumb over the right or bottom edge that says how much of
//! the content is on screen, where in it you are, and can be dragged.
//!
//! It has to be a real [`Element`] rather than a `div` sized at render time,
//! because the numbers it draws -- viewport, content size, offset -- are only
//! written onto the [`ScrollHandle`] during the scrollable element's
//! *prepaint*. A div built in `render` would be one frame stale, which on the
//! frame a table first arrives means no scrollbar at all until something else
//! happens to repaint the window. Painting after our sibling, in the same
//! frame, is what keeps it honest.

use crate::theme::{metrics, Theme};
use gpui::{
    div, point, prelude::*, px, relative, App, Bounds, DispatchPhase, Div, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, ScrollHandle, Style, Window,
};
use std::cell::Cell;
use std::rc::Rc;

/// A thumb shorter than this is one the pointer cannot catch, so a very long
/// table stops shrinking it here and lets it lie about its own scale instead.
const MIN_THUMB: f32 = 28.;

/// Breathing room between the thumb and the edges of its track.
const THUMB_INSET: f32 = 2.;

/// The bar down the right-hand edge of `parent`.
///
/// The caller's container has to be `relative()`, and this belongs *beside*
/// the scrolling element rather than inside it -- a child of the scroller
/// would scroll away with everything else. Put it after that element: siblings
/// prepaint in order, and this one reads what the other one just measured.
pub(crate) fn vertical_scrollbar(
    id: impl Into<ElementId>,
    handle: ScrollHandle,
    theme: &Theme,
) -> Div {
    div()
        .absolute()
        .top_0()
        .right_0()
        .bottom_0()
        .w(metrics::scrollbar_thickness())
        .child(Scrollbar::new(id, Axis::Vertical, handle, theme))
}

/// The bar along the bottom edge. See [`vertical_scrollbar`] for the rules.
pub(crate) fn horizontal_scrollbar(
    id: impl Into<ElementId>,
    handle: ScrollHandle,
    theme: &Theme,
) -> Div {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom_0()
        .h(metrics::scrollbar_thickness())
        .child(Scrollbar::new(id, Axis::Horizontal, handle, theme))
}

#[derive(Clone, Copy, PartialEq)]
enum Axis {
    Vertical,
    Horizontal,
}

impl Axis {
    fn of(self, at: Point<Pixels>) -> f32 {
        match self {
            Axis::Vertical => f32::from(at.y),
            Axis::Horizontal => f32::from(at.x),
        }
    }

    fn start(self, bounds: Bounds<Pixels>) -> f32 {
        self.of(bounds.origin)
    }

    fn length(self, bounds: Bounds<Pixels>) -> f32 {
        match self {
            Axis::Vertical => f32::from(bounds.size.height),
            Axis::Horizontal => f32::from(bounds.size.width),
        }
    }

    /// The viewport and the scrollable remainder, as the scrolled element
    /// measured them on this frame.
    fn extents(self, handle: &ScrollHandle) -> (f32, f32) {
        let bounds = handle.bounds();
        let max = handle.max_offset();
        match self {
            Axis::Vertical => (f32::from(bounds.size.height), f32::from(max.height)),
            Axis::Horizontal => (f32::from(bounds.size.width), f32::from(max.width)),
        }
    }

    /// How far the content is scrolled, as a positive distance. GPUI counts
    /// the offset the other way -- it is where the first child sits relative
    /// to the container, so scrolling down makes it more negative.
    fn scrolled(self, handle: &ScrollHandle) -> f32 {
        -self.of(handle.offset())
    }

    fn scroll_to(self, handle: &ScrollHandle, distance: f32) {
        let offset = handle.offset();
        let moved = match self {
            Axis::Vertical => point(offset.x, px(-distance)),
            Axis::Horizontal => point(px(-distance), offset.y),
        };
        handle.set_offset(moved);
    }
}

/// Where the pointer caught the thumb, kept across frames so a drag does not
/// snap the thumb's middle under the cursor on its first move.
type Grab = Rc<Cell<Option<f32>>>;

struct Scrollbar {
    id: ElementId,
    axis: Axis,
    handle: ScrollHandle,
    thumb: Rgba,
    thumb_active: Rgba,
    track: Rgba,
}

impl Scrollbar {
    fn new(id: impl Into<ElementId>, axis: Axis, handle: ScrollHandle, theme: &Theme) -> Self {
        Self {
            id: id.into(),
            axis,
            handle,
            // Muted rather than faint: a bar nobody can see is the thing
            // being fixed here.
            thumb: Rgba {
                a: 0.7,
                ..theme.text_muted
            },
            thumb_active: Rgba {
                a: 0.95,
                ..theme.text
            },
            track: Rgba {
                a: 0.55,
                ..theme.divider
            },
        }
    }
}

/// What prepaint worked out, or `None` when the content fits and there is
/// nothing to say.
struct Layout {
    track: Bounds<Pixels>,
    thumb: Bounds<Pixels>,
    /// Pixels of content the thumb has left to travel over.
    travel: f32,
    /// Pixels of content still to scroll through.
    max_offset: f32,
    hitbox: Hitbox,
    grab: Grab,
}

impl IntoElement for Scrollbar {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Scrollbar {
    type RequestLayoutState = ();
    type PrepaintState = Option<Layout>;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Option<Layout> {
        let (viewport, max_offset) = self.axis.extents(&self.handle);
        let track_length = self.axis.length(bounds);
        if max_offset <= 0. || viewport <= 0. || track_length <= 0. {
            return None;
        }

        let content = viewport + max_offset;
        let thumb_length = (track_length * viewport / content)
            .max(MIN_THUMB)
            .min(track_length);
        let travel = track_length - thumb_length;
        let scrolled = self.axis.scrolled(&self.handle).clamp(0., max_offset);
        let start = self.axis.start(bounds) + travel * (scrolled / max_offset);

        let thumb = match self.axis {
            Axis::Vertical => Bounds::new(
                point(bounds.origin.x, px(start)),
                gpui::size(bounds.size.width, px(thumb_length)),
            ),
            Axis::Horizontal => Bounds::new(
                point(px(start), bounds.origin.y),
                gpui::size(px(thumb_length), bounds.size.height),
            ),
        };

        let grab = window.with_element_state::<Grab, _>(id?, |state, _| {
            let grab = state.unwrap_or_default();
            (grab.clone(), grab)
        });

        Some(Layout {
            track: bounds,
            thumb,
            travel,
            max_offset,
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
            grab,
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut (),
        prepaint: &mut Option<Layout>,
        window: &mut Window,
        _cx: &mut App,
    ) {
        let Some(layout) = prepaint.take() else {
            return;
        };

        let held = layout.grab.get().is_some();
        let lit = held || layout.hitbox.is_hovered(window);
        // The groove only appears under the pointer. Drawn always, it would
        // be a permanent stripe over the right-hand column of every table.
        if lit {
            window.paint_quad(gpui::fill(layout.track, self.track));
        }
        let painted = layout.thumb.inset(px(THUMB_INSET));
        let radius = f32::from(painted.size.width).min(f32::from(painted.size.height)) / 2.;
        window.paint_quad(gpui::quad(
            painted,
            px(radius),
            if lit { self.thumb_active } else { self.thumb },
            px(0.),
            gpui::transparent_black(),
            gpui::BorderStyle::default(),
        ));

        let axis = self.axis;
        let handle = self.handle.clone();
        let Layout {
            track,
            thumb,
            travel,
            max_offset,
            hitbox,
            grab,
        } = layout;

        // Dragging the thumb: the offset is wherever the thumb would land,
        // read back as a fraction of the travel it has.
        let scroll_to_thumb = move |handle: &ScrollHandle, thumb_start: f32| {
            if travel <= 0. {
                return;
            }
            let fraction = ((thumb_start - axis.start(track)) / travel).clamp(0., 1.);
            axis.scroll_to(handle, max_offset * fraction);
        };

        {
            let grab = grab.clone();
            let handle = handle.clone();
            let hitbox = hitbox.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                let pointer = axis.of(event.position);
                let thumb_start = axis.start(thumb);
                let thumb_length = axis.length(thumb);
                if pointer >= thumb_start && pointer < thumb_start + thumb_length {
                    grab.set(Some(pointer - thumb_start));
                } else {
                    // A press on bare track jumps there, and then goes on
                    // dragging from the middle of the thumb it just moved --
                    // which is where the pointer now is.
                    grab.set(Some(thumb_length / 2.));
                    scroll_to_thumb(&handle, pointer - thumb_length / 2.);
                }
                cx.stop_propagation();
                window.refresh();
            });
        }

        {
            let grab = grab.clone();
            let handle = handle.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let Some(offset) = grab.get() else {
                    return;
                };
                // The button coming up outside the window is a mouse-up we
                // never see; a move with nothing pressed says the drag is over.
                if event.pressed_button != Some(MouseButton::Left) {
                    grab.set(None);
                    window.refresh();
                    return;
                }
                scroll_to_thumb(&handle, axis.of(event.position) - offset);
                cx.stop_propagation();
                window.refresh();
            });
        }

        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, _cx| {
            if phase == DispatchPhase::Bubble && grab.take().is_some() {
                window.refresh();
            }
        });
    }
}
