//! Phones (Android): the system tab bar in place of the desktop's page
//! buttons and history/favorites switch, and which kind of device the UI
//! is on. Decided by the running OS, not the build target, so the headless
//! tests can run the phone layout.

use nsh_core::mobile::{TabBarItem, tab_bar as system_tab_bar};
use nsh_core::platform::Os;
use nsh_core::prelude::*;

use super::state::{AppState, Page, Tab};

/// Whether the UI is on a touch screen with the system's own navigation
/// (no hover, no global shortcuts, no window to close).
pub fn is_touch() -> bool {
    matches!(os(), Os::Android | Os::Ios)
}

/// What the system's confirmation of the user is called in the UI: Touch ID
/// on the Mac, the fingerprint or face on a phone.
pub fn biometric() -> &'static str {
    if is_touch() {
        "指纹/面容"
    } else {
        "Touch ID"
    }
}

/// What each tab of the bar shows, in order.
const TABS: [MobileTab; 4] = [
    MobileTab::History,
    MobileTab::Favorites,
    MobileTab::Vault,
    MobileTab::Settings,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MobileTab {
    History,
    Favorites,
    Vault,
    Settings,
}

impl MobileTab {
    fn item(self) -> TabBarItem {
        let (title, icon) = match self {
            MobileTab::History => ("历史", "android:ic_menu_recent_history"),
            MobileTab::Favorites => ("收藏", "android:btn_star_big_on"),
            MobileTab::Vault => ("密码本", "android:ic_lock_lock"),
            MobileTab::Settings => ("设置", "android:ic_menu_preferences"),
        };
        TabBarItem::new(title).icon(icon)
    }

    /// The tab showing `page` (and the main page's `tab`): the archive is
    /// part of the history.
    fn of(page: Page, tab: Tab) -> MobileTab {
        match (page, tab) {
            (Page::Main, Tab::Favorites) => MobileTab::Favorites,
            (Page::Main | Page::Archive, _) => MobileTab::History,
            (Page::Vault, _) => MobileTab::Vault,
            (Page::Settings, _) => MobileTab::Settings,
        }
    }

    fn show(self, state: AppState) {
        match self {
            MobileTab::History => {
                state.page.set(Page::Main);
                state.set_tab(Tab::History);
            }
            MobileTab::Favorites => {
                state.page.set(Page::Main);
                state.set_tab(Tab::Favorites);
            }
            MobileTab::Vault => {
                state.page.set(Page::Vault);
                state.vault_shown();
            }
            MobileTab::Settings => state.page.set(Page::Settings),
        }
    }
}

/// The system's tab bar at the bottom.
pub fn tab_bar(state: AppState) -> impl IntoElement {
    let selected = move || {
        let shown = MobileTab::of(state.page.get(), state.tab.get());
        TABS.iter().position(|t| *t == shown).unwrap_or(0) as u32
    };
    system_tab_bar()
        .items(TABS.iter().map(|t| t.item()).collect::<Vec<_>>())
        .selected_index(selected)
        .on_change(move |index: &u32| {
            if let Some(tab) = TABS.get(*index as usize) {
                tab.show(state);
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_page_selects_a_tab_that_shows_it_again() {
        for page in [Page::Main, Page::Archive, Page::Vault, Page::Settings] {
            for tab in [Tab::History, Tab::Favorites] {
                let shown = MobileTab::of(page, tab);
                assert!(TABS.contains(&shown));
            }
        }
        assert_eq!(
            MobileTab::of(Page::Archive, Tab::Favorites),
            MobileTab::History
        );
        assert_eq!(
            MobileTab::of(Page::Main, Tab::Favorites),
            MobileTab::Favorites
        );
    }
}
