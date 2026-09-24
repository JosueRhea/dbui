//! A query plan, drawn as a tree with the expensive step picked out.
//!
//! The grid shows `EXPLAIN` as a column of strings, which is the engine's
//! answer but not a readable one: the nesting is spaces, the numbers are
//! inclusive of everything underneath, and nothing says where the time goes.
//! This draws the same plan with the nesting as indentation, each step's own
//! share of the work as a bar, and the largest share in the warning colour.

use super::caption;
use crate::root::DbUi;
use crate::tabs::WorkspaceTab;
use crate::theme::metrics;
use dbui_app::Plan;
use gpui::{div, prelude::*, px, AnyElement, Context, SharedString};

/// A figure short enough for a narrow column: 12, 3.4k, 1.2M.
fn compact(value: f64) -> String {
    if value >= 1_000_000.0 {
        format!("{:.1}M", value / 1_000_000.0)
    } else if value >= 10_000.0 {
        format!("{:.0}k", value / 1_000.0)
    } else if value >= 1_000.0 {
        format!("{:.1}k", value / 1_000.0)
    } else if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        format!("{value:.2}")
    }
}

fn milliseconds(value: f64) -> String {
    if value >= 1_000.0 {
        format!("{:.2} s", value / 1_000.0)
    } else if value >= 10.0 {
        format!("{value:.0} ms")
    } else {
        format!("{value:.2} ms")
    }
}

impl DbUi {
    /// The plan for the result in front, if it has one and the rows were
    /// not asked for instead.
    pub(crate) fn active_plan(&self) -> Option<&Plan> {
        let Some(WorkspaceTab::Sql {
            results,
            active_result,
            ..
        }) = self.tabs.active()
        else {
            return None;
        };
        let statement = results.get(*active_result)?;
        if statement.show_rows {
            return None;
        }
        statement.plan.as_ref()
    }

    /// Switch the result in front between its plan and its raw rows.
    pub(crate) fn toggle_plan_rows(&mut self, cx: &mut Context<Self>) {
        if let Some(WorkspaceTab::Sql {
            results,
            active_result,
            ..
        }) = self.tabs.active_mut()
        {
            if let Some(statement) = results.get_mut(*active_result) {
                if statement.plan.is_some() {
                    statement.show_rows = !statement.show_rows;
                }
            }
        }
        cx.notify();
    }

    /// Whether the result in front reads as a plan, drawn as one or not.
    pub(crate) fn active_result_has_plan(&self) -> bool {
        matches!(
            self.tabs.active(),
            Some(WorkspaceTab::Sql { results, active_result, .. })
                if results.get(*active_result).is_some_and(|s| s.plan.is_some())
        )
    }

    pub(crate) fn render_plan(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = &self.theme;
        let Some(plan) = self.active_plan() else {
            return div().into_any_element();
        };
        let shares = plan.self_shares();
        let hottest = shares
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .filter(|(_, share)| **share > 0.0)
            .map(|(index, _)| index);
        let measured = plan.steps.iter().any(|step| step.actual_ms.is_some());
        let has_cost = plan.steps.iter().any(|step| step.cost.is_some());
        let has_rows = plan.steps.iter().any(|step| step.rows.is_some());

        let root = &plan.steps[0];
        let mut headline = format!(
            "{} step{}",
            plan.steps.len(),
            if plan.steps.len() == 1 { "" } else { "s" }
        );
        if let Some(cost) = root.cost {
            headline.push_str(&format!(" · cost {}", compact(cost)));
        }
        if let Some(ms) = root.actual_ms {
            headline.push_str(&format!(" · {}", milliseconds(ms)));
        }

        let column = |text: String, width: f32| {
            div()
                .w(metrics::scaled(width))
                .flex_shrink_0()
                .text_right()
                .child(SharedString::from(text))
        };
        let bar_width = 90.;

        let header = div()
            .flex()
            .items_center()
            .gap_3()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(theme.border)
            .text_size(metrics::scaled(10.))
            .text_color(theme.text_faint)
            .child(div().flex_1().child("STEP"))
            .when(has_rows, |row| row.child(column("ROWS".into(), 56.)))
            .when(has_cost, |row| row.child(column("COST".into(), 64.)))
            .when(measured, |row| row.child(column("TIME".into(), 72.)))
            .child(
                div()
                    .w(metrics::scaled(bar_width + 44.))
                    .flex_shrink_0()
                    .child(if measured {
                        "SHARE OF TIME"
                    } else {
                        "SHARE OF COST"
                    }),
            );

        let rows: Vec<AnyElement> = plan
            .steps
            .iter()
            .enumerate()
            .map(|(index, step)| {
                let share = shares.get(index).copied().unwrap_or(0.0);
                let hot = hottest == Some(index);
                let fill = if hot { theme.warning } else { theme.accent };
                div()
                    .id(("plan-step", index))
                    .flex()
                    .items_start()
                    .gap_3()
                    .px_3()
                    .py_1p5()
                    .border_b_1()
                    .border_color(theme.divider)
                    .when(hot, |row| {
                        row.bg(gpui::Rgba {
                            a: 0.08,
                            ..theme.warning
                        })
                    })
                    .hover(|row| row.bg(theme.hover))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .items_start()
                            .gap_1()
                            .pl(metrics::scaled(18.) * step.depth as f32)
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_color(theme.text_faint)
                                    .child(if step.depth == 0 { "●" } else { "└" }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_color(if hot {
                                                theme.warning
                                            } else {
                                                theme.text
                                            })
                                            .when(hot, |title| {
                                                title.font_weight(gpui::FontWeight::MEDIUM)
                                            })
                                            .child(SharedString::from(step.title.clone())),
                                    )
                                    .children(step.details.iter().map(|detail| {
                                        div()
                                            .text_size(metrics::scaled(11.))
                                            .text_color(theme.text_muted)
                                            .child(SharedString::from(detail.clone()))
                                    })),
                            ),
                    )
                    .when(has_rows, |row| {
                        row.child(
                            column(step.rows.map(compact).unwrap_or_default(), 56.)
                                .text_color(theme.value_number),
                        )
                    })
                    .when(has_cost, |row| {
                        row.child(
                            column(step.cost.map(compact).unwrap_or_default(), 64.)
                                .text_color(theme.text_muted),
                        )
                    })
                    .when(measured, |row| {
                        row.child(
                            column(step.actual_ms.map(milliseconds).unwrap_or_default(), 72.)
                                .text_color(theme.text_muted),
                        )
                    })
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap_2()
                            .h(metrics::scaled(16.))
                            .child(
                                div()
                                    .w(metrics::scaled(bar_width))
                                    .h(metrics::scaled(6.))
                                    .rounded_full()
                                    .bg(theme.stripe)
                                    .child(
                                        div()
                                            .h_full()
                                            .rounded_full()
                                            .w(metrics::scaled(bar_width * share as f32))
                                            .bg(fill),
                                    ),
                            )
                            .child(
                                div()
                                    .w(metrics::scaled(36.))
                                    .text_right()
                                    .text_size(metrics::scaled(11.))
                                    .text_color(if hot { theme.warning } else { theme.text_faint })
                                    .child(SharedString::from(format!("{:.0}%", share * 100.0))),
                            ),
                    )
                    .into_any_element()
            })
            .collect();

        div()
            .id("plan-view")
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .bg(theme.background)
            .font_family(metrics::MONO_FONT)
            .text_size(metrics::text_size_small())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1p5()
                    .bg(theme.panel)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().text_color(theme.text).child("Query plan"))
                    .child(caption(headline, theme))
                    .child(div().flex_1())
                    .child(caption(
                        if measured {
                            "Measured: the statement was run"
                        } else {
                            "Estimated: the statement was not run"
                        },
                        theme,
                    ))
                    .child(
                        super::button("plan-show-rows", "Show rows", theme, false).on_click(
                            cx.listener(|this, _, _window, cx| this.toggle_plan_rows(cx)),
                        ),
                    ),
            )
            .child(header)
            .child(
                div()
                    .id("plan-steps")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .children(rows),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figures_stay_short() {
        assert_eq!(compact(12.0), "12");
        assert_eq!(compact(45.5), "45.50");
        assert_eq!(compact(1_359.0), "1.4k");
        assert_eq!(compact(25_000.0), "25k");
        assert_eq!(compact(3_200_000.0), "3.2M");
        assert_eq!(milliseconds(0.25), "0.25 ms");
        assert_eq!(milliseconds(1_500.0), "1.50 s");
    }
}
