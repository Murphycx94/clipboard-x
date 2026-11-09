//! The archive: earlier days, each collapsed under its date.

use nsh_base::theme;
use nsh_core::prelude::*;

use crate::model::DateGroup;

use super::icons::{Icon, icon};
use super::item_row::{RowPlace, item_row};
use super::state::AppState;
use super::widgets::{empty_state, muted};

pub fn archive_page(state: AppState) -> impl IntoElement {
    when(
        move || state.archive.with(Vec::is_empty),
        move || empty_state(Icon::Archive, || "暂无历史记录".to_owned()),
    )
    .otherwise(move || {
        scroll().flex_1().child(each(
            move || state.archive.get(),
            |group: &DateGroup| group.date.clone(),
            move |group, _| day(state, group),
        ))
    })
}

fn day(state: AppState, group: ReadSignal<DateGroup>) -> impl IntoElement {
    let date = move || group.with(|g| g.date.clone());
    let open = move || state.open_dates.with(|dates| dates.contains(&date()));
    view()
        .border_b_1()
        .border_color(move || theme().border)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .px(px(16.0))
                .h(px(40.0))
                .cursor_pointer()
                .hover(|s| s.bg(move || theme().accent))
                .on_press(move || state.toggle_date(&date()))
                .child(
                    icon(Icon::ChevronRight, 14.0)
                        .text_color(muted)
                        .rotate(move || deg(if open() { 90.0 } else { 0.0 }))
                        .transition_transform(),
                )
                .child(
                    text(date)
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || theme().foreground),
                )
                .child(
                    text(move || format!("{} 条", group.with(|g| g.items.len())))
                        .text_xs()
                        .text_color(muted),
                ),
        )
        .child(when(open, move || {
            view().pb(px(4.0)).child(each(
                move || group.with(|g| g.items.clone()),
                |item| item.id,
                move |item, _| {
                    let item = computed(move || Some(item.get()));
                    item_row(state, item, RowPlace::Archive)
                },
            ))
        }))
}
