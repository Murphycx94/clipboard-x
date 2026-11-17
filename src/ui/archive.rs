//! The archive: earlier days, each collapsed under its date.
//!
//! One virtualized list of the day headers and the items of the open days
//! ([`archive_rows`]): like the main list, only the rows of the screen in
//! view and of one screen above and below it are rendered, however many
//! days are open.

use nsh_base::theme;
use nsh_core::prelude::*;

use crate::model::{ArchiveRow, archive_row_key, archive_rows};

use super::icons::{Icon, icon};
use super::item_row::{RowPlace, item_row};
use super::state::AppState;
use super::widgets::{empty_state, muted};

/// The size of rows not measured yet: the item rows' 50 points and the
/// 40-point day headers, which are most rows while the days are closed.
const ESTIMATED_ROW: f32 = 46.0;

pub fn archive_page(state: AppState) -> impl IntoElement {
    let rows = computed(move || {
        state
            .archive
            .with(|groups| state.open_dates.with(|open| archive_rows(groups, open)))
    });
    virtual_list(
        move || rows.with(Vec::len),
        move |at| {
            let index = computed(move || at.index());
            let row = computed(move || rows.with(|r| r.get(index.get()).copied()));
            cell(state, row)
        },
    )
    .keys(move || {
        state.archive.with(|groups| {
            rows.with(|r| {
                r.iter()
                    .map(|&row| archive_row_key(groups, row))
                    .collect::<Vec<_>>()
            })
        })
    })
    .empty(empty_state(Icon::Archive, || "暂无历史记录".to_owned()))
    .estimated_item_size(px(ESTIMATED_ROW))
    .test_id("archive")
    .flex_1()
}

/// A recycled cell: a day's header or an item, as its row is now.
fn cell(state: AppState, row: Memo<Option<ArchiveRow>>) -> impl IntoElement {
    let day = computed(move || match row.get() {
        Some(ArchiveRow::Day { group }) => Some(group),
        _ => None,
    });
    when(move || day.get().is_some(), move || day_header(state, day)).otherwise(move || {
        let item = computed(move || match row.get() {
            Some(ArchiveRow::Item { group, index }) => state
                .archive
                .with(|groups| groups.get(group)?.items.get(index).cloned()),
            _ => None,
        });
        item_row(state, item, RowPlace::Archive)
    })
}

/// The header of the day at `group`: its date and count, pressed to open
/// or close it. A line above it parts it from the day before.
fn day_header(state: AppState, group: Memo<Option<usize>>) -> impl IntoElement {
    let date = move || {
        let group = group.get();
        state.archive.with(|groups| {
            group
                .and_then(|g| groups.get(g))
                .map(|g| g.date.clone())
                .unwrap_or_default()
        })
    };
    let count = move || {
        let group = group.get();
        state.archive.with(|groups| {
            group
                .and_then(|g| groups.get(g))
                .map_or(0, |g| g.items.len())
        })
    };
    let open = move || state.open_dates.with(|dates| dates.contains(&date()));
    let first = move || group.get() == Some(0);
    view()
        .flex_row()
        .items_center()
        .gap(px(8.0))
        .px(px(16.0))
        .h(px(40.0))
        .border_t_1()
        .border_color(move || {
            if first() {
                theme().border.opacity(0.0)
            } else {
                theme().border
            }
        })
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
            text(move || format!("{} 条", count()))
                .text_xs()
                .text_color(muted),
        )
}
