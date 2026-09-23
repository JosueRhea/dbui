//! The app's motion, all of it.
//!
//! Deliberately small: a fade and a few pixels of travel, over a fraction of
//! a second, and only when something *appears* or *happens*. Nothing waits on
//! an animation to finish before it can be clicked -- every surface is live
//! from its first frame -- and nothing loops except the one thing that is
//! genuinely ongoing, a wait the user is sitting through.
//!
//! Two kinds, with two clocks:
//!
//! - **Entrances** ride gpui's own animation element, which keys its clock to
//!   the element's id and drops it the first frame the element is not drawn.
//!   Each replays whenever its surface appears, with no state of ours to
//!   reset -- and keying the id on *content* (a result, a message) makes it
//!   replay when that content changes, too.
//! - **Flashes** -- a copied cell, a committed row -- are clocked from an
//!   `Instant` the app stores when the thing happened. The grid only draws the
//!   rows on screen, so an element-keyed clock would restart every time a
//!   flashed row scrolled back into view.
//!
//! Travel is skipped under the system's "Reduce motion" setting. Colour is
//! not: a flash is how a copy or a commit says it landed, and fading one is
//! not the kind of motion that setting is about.

use gpui::{
    div, prelude::*, px, relative, Animation, AnimationExt, ElementId, Pixels, Rgba, Window,
};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Menus are the smallest surface and the most frequent: they should read as
/// "there" rather than "arriving".
const MENU: Duration = Duration::from_millis(120);
/// Dialogs and the palette get a touch longer, because they carry more weight.
const DIALOG: Duration = Duration::from_millis(170);
/// How far a surface travels on its way in.
const TRAVEL: f32 = 6.;
/// How long a wait has to last before the busy bar admits to it.
const BUSY_GRACE: Duration = Duration::from_millis(150);

/// How long the tab indicator takes to reach a newly chosen tab.
const SLIDE: Duration = Duration::from_millis(280);

/// How long a copied cell or row stays lit.
pub(crate) const COPY_FLASH: Duration = Duration::from_millis(650);
/// How long a just-staged cell stays lit.
pub(crate) const EDIT_FLASH: Duration = Duration::from_millis(700);
/// How long committed rows glow. Longest, because the reload that brings them
/// back eats into it.
pub(crate) const COMMIT_FLASH: Duration = Duration::from_millis(1200);

/// Fast out, slow settle. Anything that eases *in* feels laggy on a surface
/// the user just summoned.
fn ease_out_cubic(t: f32) -> f32 {
    1. - (1. - t).powi(3)
}

fn animation(duration: Duration) -> Animation {
    Animation::new(duration).with_easing(ease_out_cubic)
}

/// A dropdown or context menu: fades in while dropping the last few pixels
/// onto `rest`, the top margin it sits at once settled.
pub(crate) fn menu<E: Styled + IntoElement + 'static>(
    id: impl Into<ElementId>,
    surface: E,
    rest: Pixels,
) -> impl IntoElement {
    if reduce_motion() {
        return surface.mt(rest).into_any_element();
    }
    surface
        .with_animation(id, animation(MENU), move |surface, t| {
            surface.opacity(t).mt(rest - px(TRAVEL * 0.66 * (1. - t)))
        })
        .into_any_element()
}

/// A modal: the scrim fades in, and the panel inside it rises onto `top`, the
/// scrim's top padding once settled. A scrim that centres its panel instead
/// passes zero, and the padding pushes the centre down for the same effect.
pub(crate) fn dialog<E: Styled + IntoElement + 'static>(
    id: impl Into<ElementId>,
    scrim: E,
    top: Pixels,
) -> impl IntoElement {
    if reduce_motion() {
        return scrim.pt(top).into_any_element();
    }
    scrim
        .with_animation(id, animation(DIALOG), move |scrim, t| {
            scrim.opacity(t).pt(top + px(TRAVEL * 2. * (1. - t)))
        })
        .into_any_element()
}

/// A surface docked into the layout, where travel would shove its neighbours:
/// a fade and nothing else.
pub(crate) fn fade<E: Styled + IntoElement + 'static>(
    id: impl Into<ElementId>,
    surface: E,
) -> impl IntoElement {
    surface
        .with_animation(id, animation(MENU), |surface, t| surface.opacity(t))
        .into_any_element()
}

/// A new tab: fades in while sliding the last few pixels into its slot.
///
/// Travel is a left margin, so the tabs after it shift by as much -- which is
/// what sliding into place is, and a new tab is almost always the last one.
pub(crate) fn tab<E: Styled + IntoElement + 'static>(
    id: impl Into<ElementId>,
    tab: E,
) -> impl IntoElement {
    if reduce_motion() {
        return fade(id, tab).into_any_element();
    }
    tab.with_animation(id, animation(DIALOG), |tab, t| {
        tab.opacity(t).ml(px(TRAVEL * 1.5 * (1. - t)))
    })
    .into_any_element()
}

/// One of a run of rows arriving together -- a schema's tables unfolding.
///
/// Each waits a beat longer than the one above it, so the run reads as
/// pouring down rather than blinking on. The delay stops growing after a
/// handful of rows: a schema of four hundred tables should not take seconds.
pub(crate) fn cascade<E: Styled + IntoElement + 'static>(
    id: impl Into<ElementId>,
    row: E,
    index: usize,
) -> impl IntoElement {
    // Never zero: gpui divides the elapsed time by the duration.
    let delay = Duration::from_millis(1 + 14 * index.min(10) as u64);
    row.with_animations(
        id,
        vec![Animation::new(delay), animation(MENU)],
        |row, step, t| row.opacity(if step == 0 { 0. } else { t }),
    )
    .into_any_element()
}

/// A reload icon turning once per press. Nothing before the first press: a
/// spin on launch would announce a reload nobody asked for.
pub(crate) fn spin(
    id: &'static str,
    icon: super::icons::RefreshIcon,
    presses: usize,
) -> impl IntoElement {
    if presses == 0 || reduce_motion() {
        return icon.into_any_element();
    }
    icon.with_animation(
        (id, presses),
        Animation::new(Duration::from_millis(550)).with_easing(ease_out_cubic),
        |icon, t| icon.turn(t),
    )
    .into_any_element()
}

/// Something the app is waiting on, breathing: the status light while busy.
pub(crate) fn pulse<E: Styled + IntoElement + 'static>(
    id: impl Into<ElementId>,
    light: E,
) -> impl IntoElement {
    light
        .with_animation(
            id,
            Animation::new(Duration::from_millis(1400))
                .repeat()
                .with_easing(gpui::pulsating_between(0.35, 1.)),
            |light, t| light.opacity(t),
        )
        .into_any_element()
}

/// A hairline across the top of its parent with a segment running along it,
/// for as long as the parent draws it. The parent must be `relative`.
///
/// It holds off for a moment before appearing: most waits are over in a few
/// milliseconds, and a bar that flickers on and off for each of them is noise
/// rather than news.
pub(crate) fn busy_bar(color: Rgba) -> impl IntoElement {
    const SEGMENT: f32 = 0.3;
    let track = div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(2.))
        .overflow_hidden();
    if reduce_motion() {
        // Still there, still late to arrive -- just not moving.
        return track
            .bg(Rgba { a: 0.6, ..color })
            .with_animation("busy-bar", Animation::new(BUSY_GRACE), |track, t| {
                track.opacity(if t < 1. { 0. } else { 1. })
            })
            .into_any_element();
    }
    track
        .child(
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .w(relative(SEGMENT))
                .rounded_full()
                .bg(color)
                .with_animations(
                    "busy-bar",
                    vec![
                        Animation::new(BUSY_GRACE),
                        Animation::new(Duration::from_millis(1100))
                            .repeat()
                            .with_easing(gpui::ease_in_out),
                    ],
                    |segment, step, t| {
                        if step == 0 {
                            return segment.opacity(0.).left(relative(-SEGMENT));
                        }
                        segment.left(relative(-SEGMENT + (1. + SEGMENT) * t))
                    },
                ),
        )
        .into_any_element()
}

/// The horizontal extent of something that slides: its left and right edges.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct Span {
    pub left: f32,
    pub right: f32,
}

/// A marker that travels between the things it can mark -- the active tab's
/// underline -- rather than blinking off one and on at the next.
///
/// It is always heading for a `to`, from wherever it was when it was last
/// re-aimed: aimed again mid-flight, it turns around from where it is instead
/// of jumping back to the start.
pub(crate) struct Slide {
    from: Span,
    to: Span,
    started: Instant,
    /// Bumped on every re-aim, so the animation keyed on it starts over.
    generation: usize,
}

impl Slide {
    pub(crate) fn at(to: Span) -> Self {
        Self {
            from: to,
            to,
            started: Instant::now(),
            generation: 0,
        }
    }

    /// Head for `to`. A no-op if it already is.
    pub(crate) fn aim(&mut self, to: Span) {
        if to == self.to {
            return;
        }
        self.from = self.now();
        self.to = to;
        self.started = Instant::now();
        self.generation += 1;
    }

    /// Where it is this instant.
    fn now(&self) -> Span {
        let t = self.started.elapsed().as_secs_f32() / SLIDE.as_secs_f32();
        slide_between(self.from, self.to, t.min(1.))
    }

    /// Draw `bar` wherever the slide has got to. `bar` sets everything but
    /// its left edge and width.
    pub(crate) fn render(&self, id: &'static str, bar: gpui::Div) -> impl IntoElement {
        let (from, to) = (self.from, self.to);
        let place =
            |bar: gpui::Div, span: Span| bar.left(px(span.left)).w(px(span.right - span.left));
        if reduce_motion() {
            return place(bar, to).into_any_element();
        }
        bar.with_animation(
            (id, self.generation),
            Animation::new(SLIDE),
            move |bar, t| place(bar, slide_between(from, to, t)),
        )
        .into_any_element()
    }
}

/// The two edges do not move together: the one in the direction of travel
/// sets off first and the other catches up, so the bar stretches towards
/// where it is going and settles into the new width on arrival.
fn slide_between(from: Span, to: Span, t: f32) -> Span {
    let lead = ease_out_cubic((t / 0.75).min(1.));
    let trail = ease_out_cubic(((t - 0.15) / 0.85).clamp(0., 1.));
    let (left, right) = if to.left > from.left {
        (trail, lead)
    } else {
        (lead, trail)
    };
    let lerp = |a: f32, b: f32, k: f32| a + (b - a) * k;
    Span {
        left: lerp(from.left, to.left, left),
        right: lerp(from.right, to.right, right),
    }
}

/// How lit a flash that began at `started` still is: 1 at first, holding
/// briefly, then easing to nothing over `duration`. `None` once it is over.
///
/// Asks for another frame while it is live, which is all the clock needs --
/// the window is redrawn, reads this again, and the flash dims a step.
pub(crate) fn flash(started: Instant, duration: Duration, window: &mut Window) -> Option<f32> {
    let t = started.elapsed().as_secs_f32() / duration.as_secs_f32();
    if t >= 1. {
        return None;
    }
    window.request_animation_frame();
    // Hold at full for the first fifth, so the eye has time to land on it.
    const HOLD: f32 = 0.2;
    let fading = ((t - HOLD) / (1. - HOLD)).max(0.);
    Some(1. - ease_out_cubic(fading))
}

/// A stable id for "this piece of content", so an entrance keyed on it plays
/// again when the content changes and not otherwise.
pub(crate) fn content_key(content: &impl std::hash::Hash) -> usize {
    use std::hash::{BuildHasher, BuildHasherDefault, DefaultHasher};
    BuildHasherDefault::<DefaultHasher>::default().hash_one(content) as usize
}

/// Read once: the setting rarely changes mid-session, and asking AppKit on
/// every frame of every animation is not free.
///
/// Always on under test: the tests measure layout on the wall clock, and a
/// tab caught mid-slide is a tab in the wrong place. Fades stay, since
/// opacity moves nothing.
fn reduce_motion() -> bool {
    if cfg!(test) {
        return true;
    }
    static REDUCE: OnceLock<bool> = OnceLock::new();
    *REDUCE.get_or_init(system_reduce_motion)
}

#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)]
fn system_reduce_motion() -> bool {
    use cocoa::base::{id, nil, BOOL, NO};
    use objc::{class, msg_send, sel, sel_impl};
    unsafe {
        let workspace: id = msg_send![class!(NSWorkspace), sharedWorkspace];
        if workspace == nil {
            return false;
        }
        let reduce: BOOL = msg_send![workspace, accessibilityDisplayShouldReduceMotion];
        reduce != NO
    }
}

#[cfg(not(target_os = "macos"))]
fn system_reduce_motion() -> bool {
    false
}
