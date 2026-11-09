//! The bar at the top: the app's name and its pages.

use nsh_base::theme;
use nsh_core::prelude::*;

use super::icons::{Icon, icon};
use super::state::{AppState, Page};
use super::widgets::{icon_button, muted};

pub fn header(state: AppState) -> impl IntoElement {
    let page_tint = move |page: Page| {
        move || {
            if state.page.get() == page {
                theme().primary
            } else {
                muted()
            }
        }
    };
    view()
        .flex_row()
        .items_center()
        .justify_between()
        .h(px(46.0))
        .pl(px(16.0))
        .pr(px(10.0))
        .border_b_1()
        .border_color(move || theme().border)
        .window_drag(true)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(8.0))
                .cursor_pointer()
                .on_press(move || state.page.set(Page::Main))
                .child(
                    view()
                        .size(px(22.0))
                        .rounded(px(6.0))
                        .items_center()
                        .justify_center()
                        .bg(move || theme().primary)
                        .child(
                            icon(Icon::Logo, 14.0).text_color(move || theme().primary_foreground),
                        ),
                )
                .child(
                    text("ClipboardX")
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(move || theme().foreground),
                )
                .child(
                    text(move || match state.page.get() {
                        Page::Main => "",
                        Page::Archive => "· 历史归档",
                        Page::Settings => "· 设置",
                    })
                    .text_sm()
                    .text_color(muted),
                ),
        )
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(2.0))
                .window_drag(false)
                .child(icon_button(
                    Icon::Archive,
                    "历史归档",
                    page_tint(Page::Archive),
                    move || state.toggle_page(Page::Archive),
                ))
                .child(icon_button(
                    Icon::Settings,
                    "设置",
                    page_tint(Page::Settings),
                    move || state.toggle_page(Page::Settings),
                ))
                .child(icon_button(Icon::Close, "关闭 (Esc)", muted, move || {
                    state.close_window()
                })),
        )
}
