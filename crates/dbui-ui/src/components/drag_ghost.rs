//! What the pointer is carrying.
//!
//! Reordering a tab or a column used to happen entirely in place: the strip
//! shuffled a slot at a time, and nothing under the pointer said a tab was in
//! hand. This draws a lifted copy of the thing being dragged that follows the
//! pointer, while the original stays in the strip, dimmed, marking the slot it
//! will land in.
//!
//! It only appears once the press has travelled past the drag slop, so a
//! plain click never flashes one up -- and it takes no pointer events, so the
//! headings and tabs underneath keep hearing the moves that reorder them.

use super::icons::{sql_icon, table_icon};
use super::{motion, type_badge};
use crate::root::DbUi;
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use gpui::{div, prelude::*, AnyElement, SharedString};

impl DbUi {
    pub(crate) fn render_drag_ghost(&self) -> Option<AnyElement> {
        let pointer = self.drag_pointer?;
        let theme = &self.theme;

        let lifted = |ghost: gpui::Div| {
            ghost
                .absolute()
                .flex()
                .items_center()
                .gap_2()
                .h(metrics::control_height())
                .px_2p5()
                .rounded_md()
                .bg(theme.elevated)
                .border_1()
                .border_color(theme.accent)
                .shadow_lg()
                .text_color(theme.text)
                .text_size(metrics::text_size_small())
                .opacity(0.94)
        };

        if let Some(drag) = self.tab_drag.as_ref().filter(|drag| drag.moved) {
            let tab = self.tabs.items.iter().find(|tab| tab.id() == drag.id)?;
            let icon = match tab {
                WorkspaceTab::Table { .. } => table_icon(theme.text_muted).into_any_element(),
                WorkspaceTab::Sql { .. } => sql_icon(theme.text_muted).into_any_element(),
            };
            // Held near its left end, the way a tab is usually picked up.
            let ghost = lifted(div())
                .left(pointer.x - metrics::scaled(22.))
                .top(pointer.y - metrics::control_height() / 2.)
                .max_w(metrics::scaled(200.))
                .child(icon)
                .child(div().truncate().child(SharedString::from(tab.label())));
            return Some(motion::fade("drag-ghost-in", ghost).into_any_element());
        }

        if let Some(drag) = self.column_move.filter(|drag| drag.moved) {
            let view = self.tabs.active()?.result()?;
            let column = view.set.columns.get(drag.column)?;
            let ghost = lifted(div())
                .left(pointer.x - metrics::scaled(18.))
                .top(pointer.y - metrics::control_height() / 2.)
                .max_w(metrics::scaled(260.))
                .font_family(metrics::MONO_FONT)
                .child(
                    div()
                        .truncate()
                        .text_size(metrics::text_size())
                        .child(SharedString::from(column.name.clone())),
                )
                .child(type_badge(column.type_name.clone(), theme));
            return Some(motion::fade("drag-ghost-in", ghost).into_any_element());
        }

        None
    }
}
