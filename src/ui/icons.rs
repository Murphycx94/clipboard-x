//! Line icons drawn as inline SVG in the text color (Lucide's shapes, ISC
//! license).

use nsh_core::element::ImageSrc;
use nsh_core::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Logo,
    Archive,
    Settings,
    Close,
    Search,
    Image,
    Star,
    StarFilled,
    Edit,
    Trash,
    Check,
    ChevronRight,
    Keyboard,
    Clock,
    Contrast,
    Eye,
    Send,
    Refresh,
    Info,
}

fn shapes(icon: Icon) -> &'static str {
    match icon {
        Icon::Logo => {
            r#"<rect width="8" height="4" x="8" y="2" rx="1"/><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"/><path d="M9 12h6"/><path d="M9 16h4"/>"#
        }
        Icon::Archive => {
            r#"<rect width="20" height="5" x="2" y="3" rx="1"/><path d="M4 8v11a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8"/><path d="M10 12h4"/>"#
        }
        Icon::Settings => {
            r#"<path d="M20 7h-9"/><path d="M14 17H5"/><circle cx="17" cy="17" r="3"/><circle cx="7" cy="7" r="3"/>"#
        }
        Icon::Close => r#"<path d="M18 6 6 18"/><path d="m6 6 12 12"/>"#,
        Icon::Search => r#"<circle cx="11" cy="11" r="7"/><path d="m21 21-4.3-4.3"/>"#,
        Icon::Image => {
            r#"<rect width="18" height="18" x="3" y="3" rx="2"/><circle cx="9" cy="9" r="2"/><path d="m21 15-3.1-3.1a2 2 0 0 0-2.8 0L6 21"/>"#
        }
        Icon::Star | Icon::StarFilled => {
            r#"<path d="M11.5 2.3a.5.5 0 0 1 .9 0l2.3 4.7a2 2 0 0 0 1.6 1.1l5.2.8a.5.5 0 0 1 .3.9l-3.8 3.7a2 2 0 0 0-.6 1.8l.9 5.2a.5.5 0 0 1-.8.6l-4.6-2.5a2 2 0 0 0-1.9 0l-4.6 2.5a.5.5 0 0 1-.8-.6l.9-5.2a2 2 0 0 0-.6-1.8L2.2 9.8a.5.5 0 0 1 .3-.9l5.2-.8a2 2 0 0 0 1.6-1.1z"/>"#
        }
        Icon::Edit => {
            r#"<path d="M21.2 6.8a1 1 0 0 0-4-4L3.8 16.2a2 2 0 0 0-.5.8L2 21.4a.5.5 0 0 0 .6.6l4.4-1.3a2 2 0 0 0 .8-.5z"/>"#
        }
        Icon::Trash => {
            r#"<path d="M3 6h18"/><path d="M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><path d="M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2"/>"#
        }
        Icon::Check => r#"<path d="M20 6 9 17l-5-5"/>"#,
        Icon::ChevronRight => r#"<path d="m9 18 6-6-6-6"/>"#,
        Icon::Keyboard => {
            r#"<rect width="20" height="16" x="2" y="4" rx="2"/><path d="M6 8h.01"/><path d="M10 8h.01"/><path d="M14 8h.01"/><path d="M18 8h.01"/><path d="M7 16h10"/>"#
        }
        Icon::Clock => r#"<circle cx="12" cy="12" r="10"/><path d="M12 6v6l4 2"/>"#,
        Icon::Contrast => {
            r#"<circle cx="12" cy="12" r="10"/><path d="M12 18a6 6 0 0 0 0-12v12z"/>"#
        }
        Icon::Eye => {
            r#"<path d="M2 12s3-7 10-7 10 7 10 7-3 7-10 7-10-7-10-7Z"/><circle cx="12" cy="12" r="3"/>"#
        }
        Icon::Send => r#"<path d="m22 2-7 20-4-9-9-4Z"/><path d="M22 2 11 13"/>"#,
        Icon::Refresh => {
            r#"<path d="M21 12a9 9 0 1 1-9-9c2.5 0 4.9 1 6.7 2.7L21 8"/><path d="M21 3v5h-5"/>"#
        }
        Icon::Info => {
            r#"<circle cx="12" cy="12" r="10"/><path d="M12 16v-4"/><path d="M12 8h.01"/>"#
        }
    }
}

fn markup(icon: Icon) -> String {
    let fill = if icon == Icon::StarFilled {
        "currentColor"
    } else {
        "none"
    };
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="{fill}" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
        shapes(icon)
    )
}

/// `icon`, `size` points square, in the element's text color.
pub fn icon(icon: Icon, size: f32) -> Svg {
    svg(ImageSrc::svg_markup(&markup(icon))).size(px(size))
}
