//! The app's small building blocks, drawn with the theme's tokens.

use nsh_base::{button, dialog, theme, tooltip};
use nsh_core::prelude::*;

use super::icons::{Icon, icon};
use super::state::{AppState, ToastKind};

/// A theme color the app adds (`theme.json`), or `fallback`.
pub fn token(name: &str, fallback: fn() -> AdaptiveColor) -> AdaptiveColor {
    theme().color(name).unwrap_or_else(fallback)
}

pub fn star_color() -> AdaptiveColor {
    token("star", || theme().primary)
}

pub fn copied_color() -> AdaptiveColor {
    token("copied", || theme().accent)
}

pub fn success_color() -> AdaptiveColor {
    token("success", || theme().primary)
}

/// A small hint above an element, on hover.
pub fn hint(label: &'static str) -> nsh_base::TooltipElement {
    tooltip()
        .rounded(px(6.0))
        .bg(move || theme().foreground)
        .px(px(8.0))
        .py(px(4.0))
        .child(text(label).text_xs().text_color(move || theme().background))
}

/// A square button with an icon; `tint` colors the icon.
pub fn icon_button(
    glyph: Icon,
    label: &'static str,
    tint: impl Fn() -> AdaptiveColor + 'static,
    on_press: impl Fn() + 'static,
) -> impl IntoElement {
    view().tooltip(hint(label)).child(
        button()
            .on_press(on_press)
            .accessibility_label(label)
            .items_center()
            .justify_center()
            .size(px(28.0))
            .rounded(px(7.0))
            .cursor_pointer()
            .hover(|s| s.bg(move || theme().accent))
            .active(|s| s.opacity(0.6))
            .child(icon(glyph, 16.0).text_color(tint)),
    )
}

/// `color` in the appearance shown now (for props that take one color).
pub fn resolved(color: AdaptiveColor) -> Hsla {
    if dark() { color.dark } else { color.light }
}

/// The muted color of an idle icon.
pub fn muted() -> AdaptiveColor {
    theme().muted_foreground
}

/// A row of options, one of them chosen.
pub fn segmented<T: Copy + PartialEq + 'static>(
    options: Vec<(T, &'static str)>,
    value: impl Fn() -> T + Copy + 'static,
    on_change: impl Fn(T) + Copy + 'static,
) -> impl IntoElement {
    view()
        .flex_row()
        .p(px(2.0))
        .gap(px(2.0))
        .rounded(px(8.0))
        .bg(move || theme().muted)
        .child(
            options
                .into_iter()
                .map(move |(option, label)| {
                    let selected = move || value() == option;
                    button()
                        .on_press(move || on_change(option))
                        .accessibility_label(label)
                        .px(px(10.0))
                        .py(px(4.0))
                        .rounded(px(6.0))
                        .cursor_pointer()
                        .bg(move || {
                            if selected() {
                                theme().background
                            } else {
                                theme().muted
                            }
                        })
                        .child(
                            text(label)
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(move || {
                                    if selected() {
                                        theme().foreground
                                    } else {
                                        theme().muted_foreground
                                    }
                                }),
                        )
                })
                .collect::<Vec<_>>(),
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Primary,
    Plain,
    Danger,
}

/// A text button.
pub fn text_button(
    label: impl IntoReactive<SharedString>,
    tone: Tone,
    on_press: impl Fn() + 'static,
) -> ButtonBuilder {
    button()
        .on_press(on_press)
        .items_center()
        .justify_center()
        .px(px(12.0))
        .h(px(28.0))
        .rounded(px(7.0))
        .cursor_pointer()
        .bg(move || match tone {
            Tone::Primary => theme().primary,
            Tone::Plain => theme().muted,
            Tone::Danger => theme().destructive.opacity(0.12),
        })
        .hover(|s| s.opacity(0.88))
        .active(|s| s.opacity(0.7))
        .disabled_style(|s| s.opacity(0.5))
        .child(
            text(label)
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(move || match tone {
                    Tone::Primary => theme().primary_foreground,
                    Tone::Plain => theme().foreground,
                    Tone::Danger => theme().destructive,
                }),
        )
}

/// The element `nsh_base::button()` makes.
pub type ButtonBuilder = nsh_base::ButtonElement;

/// A text field in the app's look.
/// Submits the open form with ⌘↩ (Ctrl+Enter elsewhere), from any field,
/// a multi-line one too: bound while the calling component lives.
pub fn submit_with_mod_enter(submit: impl Fn() + 'static) {
    let options = KeyOptions {
        when: nsh_core::keymap::When::Always,
        repeat: false,
        pass_through: false,
    };
    on_key_with("mod+enter", options, move |_| submit());
}

pub fn field() -> Input {
    nsh_base::input()
        .h(px(30.0))
        .px(px(10.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(move || theme().input)
        .bg(move || theme().background)
        .text_sm()
        .text_color(move || theme().foreground)
        .placeholder_color(move || resolved(theme().muted_foreground))
        .caret_color(move || resolved(theme().primary))
        .focus(|s| s.border_color(move || theme().ring))
        .auto_correct(false)
        .auto_capitalize(AutoCapitalize::None)
}

/// A dialog over the window: a dimmed backdrop that closes it and a card
/// with `title` and `content`.
pub fn modal(
    state: AppState,
    title: impl Into<SharedString> + 'static,
    width: f32,
    content: impl IntoElement,
) -> impl IntoElement {
    let close = move || state.dialog.set(None);
    let title: SharedString = title.into();
    dialog()
        .animation(ModalAnimation::Fade)
        .on_cancel(close)
        .flex_1()
        // Centered in what the software keyboard leaves of the window.
        .pb(move || px(keyboard_height().0))
        .child(
            view()
                .absolute()
                .inset_0()
                .bg(rgba(0x0000_0059))
                .focusable(false)
                .on_press(close),
        )
        .child(
            view()
                .m(auto())
                .w(px(width))
                .max_h(relative(0.86))
                .rounded(px(14.0))
                .bg(move || theme().popover)
                .border_1()
                .border_color(move || theme().border)
                .shadow_xl()
                .overflow_hidden()
                .child(
                    view()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .pl(px(16.0))
                        .pr(px(8.0))
                        .pt(px(10.0))
                        .child(
                            text(title)
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(move || theme().popover_foreground),
                        )
                        .child(icon_button(Icon::Close, "关闭", muted, close)),
                )
                .child(view().px(px(16.0)).pt(px(6.0)).pb(px(16.0)).child(content)),
        )
}

/// The toast at the bottom of the window.
pub fn toast_layer(state: AppState) -> impl IntoElement {
    view()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(18.0))
        .items_center()
        .child(when(
            move || state.toast.with(Option::is_some),
            move || {
                let kind = move || state.toast.with(|t| t.as_ref().map(|t| t.kind));
                view()
                    .flex_row()
                    .items_center()
                    .gap(px(8.0))
                    .max_w(px(340.0))
                    .px(px(14.0))
                    .py(px(8.0))
                    .rounded_full()
                    .bg(move || theme().foreground)
                    .shadow_lg()
                    .child(
                        view()
                            .size(px(6.0))
                            .rounded_full()
                            .bg(move || match kind() {
                                Some(ToastKind::Error) => theme().destructive,
                                _ => success_color(),
                            }),
                    )
                    .child(
                        text(move || {
                            state
                                .toast
                                .with(|t| t.as_ref().map(|t| t.message.clone()).unwrap_or_default())
                        })
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM)
                        .number_of_lines(2)
                        .text_color(move || theme().background),
                    )
            },
        ))
}

/// A centered message for an empty list.
pub fn empty_state(glyph: Icon, message: impl Fn() -> String + 'static) -> impl IntoElement {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .py(px(48.0))
        .child(
            view()
                .size(px(44.0))
                .rounded_full()
                .items_center()
                .justify_center()
                .bg(move || theme().muted)
                .child(icon(glyph, 20.0).text_color(muted)),
        )
        .child(text(message).text_sm().text_color(muted))
}
