//! One clipboard item in a list: its time, its content and its actions
//! (shown on hover on the desktops; a long press opens them on a phone).

use nsh_base::{context_menu, menu_item, theme};
use nsh_core::prelude::*;

use crate::model::{ClipboardItem, ContentType, shortcut_label, time_label};

use super::icons::{Icon, icon};
use super::mobile::is_touch;
use super::state::AppState;
use super::widgets::{copied_color, icon_button, muted, star_color};

/// Where a row sits: in the main list (keyboard focus, shortcuts) or in
/// the archive.
#[derive(Clone, Copy)]
pub enum RowPlace {
    Main(Memo<usize>),
    Archive,
}

/// The row of `item` (`None` while a recycled cell has none).
pub fn item_row(
    state: AppState,
    item: Memo<Option<ClipboardItem>>,
    place: RowPlace,
) -> impl IntoElement {
    let id = move || item.with(|i| i.as_ref().map_or(-1, |i| i.id));
    let index = move || match place {
        RowPlace::Main(index) => Some(index.get()),
        RowPlace::Archive => None,
    };
    let touch = is_touch();
    // The keyboard's selection: a phone has none to show.
    let focused = move || !touch && index().is_some_and(|i| i == state.focused.get());
    let copied = move || state.copied.get() == Some(id());
    let hovered = move || state.hovered.get() == Some(id());

    let row = view()
        .flex_row()
        .items_center()
        .gap(px(12.0))
        .min_h(px(50.0))
        .px(px(16.0))
        .py(px(8.0))
        .cursor_pointer()
        .bg(move || {
            if copied() {
                copied_color()
            } else if focused() || hovered() {
                theme().accent
            } else {
                theme().background
            }
        })
        .transition_colors()
        .on_press(move || state.copy(id(), false))
        .on_mouse_enter(move |_: &MouseEvent| {
            state.hovered.set(Some(id()));
            if let Some(i) = index() {
                state.focused.set(i);
            }
        })
        .on_mouse_leave(move |_: &MouseEvent| {
            if state.hovered.get() == Some(id()) {
                state.hovered.set(None);
            }
        })
        .child(meta_column(state, item, index, touch))
        .child(content(state, item))
        .child(actions(state, item, copied, hovered));
    if touch {
        item_menu(state, item, row).into_element()
    } else {
        row.into_element()
    }
}

/// `row` as the trigger of the item's actions, which a long press opens in
/// a sheet at the bottom of the screen.
fn item_menu(
    state: AppState,
    item: Memo<Option<ClipboardItem>>,
    row: impl IntoElement + 'static,
) -> impl IntoElement {
    let action = move |label: fn(&ClipboardItem) -> &'static str,
                       run: fn(AppState, &ClipboardItem)| {
        menu_item()
            .px(px(20.0))
            .py(px(15.0))
            .on_select(move || {
                if let Some(i) = item.get() {
                    run(state, &i);
                }
            })
            .child(
                text(move || item.with(|i| i.as_ref().map(label).unwrap_or_default()))
                    .text_base()
                    .text_color(move || theme().foreground),
            )
    };
    context_menu()
        .trigger(row)
        .popup_class("bg-popover rounded-t-2xl pt-2 pb-8")
        .backdrop_class("bg-black/30")
        .child(action(|_| "复制", |s, i| s.copy(i.id, false)))
        .child(action(|_| "备注", |s, i| s.open_note(i)))
        .child(action(
            |i| {
                if i.is_favorite {
                    "取消收藏"
                } else {
                    "收藏"
                }
            },
            |s, i| s.toggle_favorite(i),
        ))
        .child(action(|_| "删除", |s, i| s.delete(i.id)))
}

/// The time, and the shortcut of the first nine rows (desktops).
fn meta_column(
    _state: AppState,
    item: Memo<Option<ClipboardItem>>,
    index: impl Fn() -> Option<usize> + Copy + 'static,
    touch: bool,
) -> impl IntoElement {
    view()
        .w(px(40.0))
        .items_center()
        .gap(px(2.0))
        .child(
            text(move || {
                item.with(|i| {
                    i.as_ref()
                        .map(|i| time_label(&i.created_at).to_owned())
                        .unwrap_or_default()
                })
            })
            .text_xs()
            .font_weight(FontWeight::MEDIUM)
            .text_color(muted),
        )
        .child(
            text(move || match index() {
                Some(i) if i < 9 && !touch => shortcut_label(i + 1, apple_keys()),
                _ => String::new(),
            })
            .text_size(px(10.0))
            .text_color(move || theme().muted_foreground.opacity(0.7)),
        )
}

/// The text or the thumbnail, and the note.
fn content(state: AppState, item: Memo<Option<ClipboardItem>>) -> impl IntoElement {
    let kind = move || item.with(|i| i.as_ref().map(|i| i.content_type));
    let note = move || {
        item.with(|i| {
            i.as_ref()
                .and_then(|i| i.note_text().map(str::to_owned))
                .unwrap_or_default()
        })
    };
    view()
        .flex_1()
        .min_w(px(0.0))
        .gap(px(3.0))
        .child(
            when(
                move || kind() == Some(ContentType::Image),
                move || {
                    let thumbnail =
                        move || item.with(|i| i.as_ref().and_then(|i| i.thumbnail.clone()));
                    view()
                        .flex_row()
                        .items_center()
                        .gap(px(8.0))
                        .child(
                            image(move || thumbnail().unwrap_or_default())
                                .w(px(64.0))
                                .h(px(40.0))
                                .rounded(px(6.0))
                                .border_1()
                                .border_color(move || theme().border)
                                .object_fit(ResizeMode::Cover)
                                .cursor(Cursor::ZoomIn)
                                .tooltip(super::widgets::hint("查看大图"))
                                .on_press(move || {
                                    if let Some(i) = item.get() {
                                        state.preview_image(i.id, i.thumbnail);
                                    }
                                }),
                        )
                        .child(text("图片").text_xs().text_color(muted))
                },
            )
            .otherwise(move || {
                text(move || {
                    item.with(|i| {
                        i.as_ref()
                            .and_then(|i| i.text_preview.as_deref())
                            .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "))
                            .unwrap_or_default()
                    })
                })
                .text_sm()
                .number_of_lines(1)
                .text_color(move || theme().foreground)
            }),
        )
        .child(when(
            move || !note().is_empty(),
            move || {
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(4.0))
                    .child(icon(Icon::Edit, 10.0).text_color(move || theme().primary))
                    .child(
                        text(note)
                            .text_xs()
                            .number_of_lines(1)
                            .text_color(move || theme().primary),
                    )
            },
        ))
}

/// Edit, favorite and delete while hovered; a check once copied; a star on
/// a favorite otherwise.
fn actions(
    state: AppState,
    item: Memo<Option<ClipboardItem>>,
    copied: impl Fn() -> bool + Copy + 'static,
    hovered: impl Fn() -> bool + Copy + 'static,
) -> impl IntoElement {
    let favorite = move || item.with(|i| i.as_ref().is_some_and(|i| i.is_favorite));
    let has_note = move || item.with(|i| i.as_ref().is_some_and(|i| i.note_text().is_some()));
    let with_item = move |f: fn(AppState, &ClipboardItem)| {
        move || {
            if let Some(i) = item.get() {
                f(state, &i);
            }
        }
    };
    view()
        .w(px(92.0))
        .flex_row()
        .items_center()
        .justify_end()
        .gap(px(2.0))
        .child(
            when(copied, move || {
                icon(Icon::Check, 18.0).text_color(move || theme().primary)
            })
            .otherwise(move || {
                when(hovered, move || {
                    view()
                        .flex_row()
                        .gap(px(2.0))
                        .child(icon_button(
                            Icon::Edit,
                            "备注",
                            move || if has_note() { theme().primary } else { muted() },
                            with_item(|s, i| s.open_note(i)),
                        ))
                        .child(
                            when(favorite, move || {
                                icon_button(
                                    Icon::StarFilled,
                                    "取消收藏",
                                    star_color,
                                    with_item(|s, i| s.toggle_favorite(i)),
                                )
                            })
                            .otherwise(move || {
                                icon_button(
                                    Icon::Star,
                                    "收藏",
                                    muted,
                                    with_item(|s, i| s.toggle_favorite(i)),
                                )
                            }),
                        )
                        .child(icon_button(
                            Icon::Trash,
                            "删除",
                            muted,
                            with_item(|s, i| s.delete(i.id)),
                        ))
                })
                .otherwise(move || {
                    when(favorite, move || {
                        view()
                            .pr(px(6.0))
                            .child(icon(Icon::StarFilled, 14.0).text_color(star_color))
                    })
                })
            }),
        )
}
