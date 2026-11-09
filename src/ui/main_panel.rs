//! The main page: today's history and the favorites, searched and filtered.

use nsh_base::{button, theme};
use nsh_core::prelude::*;

use super::icons::{Icon, icon};
use super::item_row::{RowPlace, item_row};
use super::state::{AppState, Tab};
use super::widgets::{empty_state, field, hint, muted, segmented};

pub fn main_panel(state: AppState) -> impl IntoElement {
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(toolbar(state))
        .child(list(state))
}

fn toolbar(state: AppState) -> impl IntoElement {
    view()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .px(px(14.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(move || theme().border)
        .child(segmented(
            vec![(Tab::History, "今日历史"), (Tab::Favorites, "收藏夹")],
            move || state.tab.get(),
            move |tab| state.set_tab(tab),
        ))
        .child(search_field(state))
}

fn search_field(state: AppState) -> impl IntoElement {
    let images_only = move || state.images_only.get();
    view()
        .flex_1()
        .flex_row()
        .items_center()
        .child(
            field()
                .element_ref(&state.search)
                .test_id("search")
                .flex_1()
                .pl(px(28.0))
                .pr(px(52.0))
                .value(move || state.query.get())
                .on_change(move |q: &str| {
                    state.query.set(q.to_owned());
                    state.focused.set(0);
                })
                .placeholder("搜索…")
                .return_key_type(ReturnKeyType::Search)
                .submit_behavior(SubmitBehavior::Submit)
                .auto_focus(true),
        )
        .child(
            view()
                .absolute()
                .left(px(9.0))
                .top(px(8.0))
                .pointer_events(PointerEvents::None)
                .child(icon(Icon::Search, 14.0).text_color(muted)),
        )
        .child(
            view()
                .absolute()
                .right(px(4.0))
                .top(px(3.0))
                .flex_row()
                .items_center()
                .child(when(
                    move || !state.query.with(String::is_empty),
                    move || {
                        small_toggle(
                            Icon::Close,
                            "清空",
                            || false,
                            move || {
                                state.query.set(String::new());
                                state.search.focus();
                            },
                        )
                    },
                ))
                .child(small_toggle(
                    Icon::Image,
                    "只看图片",
                    images_only,
                    move || {
                        state.images_only.update(|on| *on = !*on);
                        state.focused.set(0);
                    },
                )),
        )
}

/// A 24-point icon button inside the search field; `on` tints it.
fn small_toggle(
    glyph: Icon,
    label: &'static str,
    on: impl Fn() -> bool + Copy + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    view().tooltip(hint(label)).child(
        button()
            .on_press(on_press)
            .accessibility_label(label)
            .size(px(24.0))
            .rounded(px(5.0))
            .items_center()
            .justify_center()
            .cursor_pointer()
            .bg(move || {
                if on() {
                    theme().primary.opacity(0.14)
                } else {
                    theme().background
                }
            })
            .hover(|s| s.bg(move || theme().accent))
            .child(
                icon(glyph, 14.0).text_color(move || if on() { theme().primary } else { muted() }),
            ),
    )
}

fn list(state: AppState) -> impl IntoElement {
    let empty_message = move || {
        let searching = !state.query.with(|q| q.trim().is_empty()) || state.images_only.get();
        match (searching, state.tab.get()) {
            (true, _) => "无匹配内容".to_owned(),
            (false, Tab::History) => "今日暂无复制记录".to_owned(),
            (false, Tab::Favorites) => "暂无收藏，悬停条目点击 ☆ 添加".to_owned(),
        }
    };
    virtual_list(
        move || state.visible.with(Vec::len),
        move |at| {
            let index = computed(move || at.index());
            let item =
                computed(move || state.visible.with(|items| items.get(index.get()).cloned()));
            item_row(state, item, RowPlace::Main(index))
        },
    )
    .keys(move || {
        state
            .visible
            .with(|items| items.iter().map(|i| i.id).collect::<Vec<_>>())
    })
    .empty(empty_state(Icon::Logo, empty_message))
    .estimated_item_size(px(52.0))
    .element_ref(&state.list)
    .flex_1()
}
