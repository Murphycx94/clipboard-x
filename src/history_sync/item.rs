//! One history item as devices share it, and how two states of it become
//! one. The merge takes the later of each part, so devices that see the
//! same states in any order, any number of times, end up alike.

use serde::{Deserialize, Serialize};

/// An item's state. Times are Unix milliseconds, each device's own clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemState {
    /// The SHA-256 of the text, in hex: what names the item everywhere.
    pub hash: String,
    /// The text while the item is alive; a deleted item has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// When it was copied last.
    pub copied_at: i64,
    /// When the favorite mark or the note last changed (a deletion clears
    /// both, so it counts).
    pub meta_at: i64,
    pub favorite: bool,
    pub note: String,
    /// When it was deleted last; 0 when never.
    pub deleted_at: i64,
}

fn alive_at(copied_at: i64, meta_at: i64, deleted_at: i64) -> bool {
    copied_at.max(meta_at) > deleted_at
}

/// What decides whose mark and note win: the later change, then the larger.
fn mark_order(s: &ItemState) -> (i64, bool, &str) {
    (s.meta_at, s.favorite, s.note.as_str())
}

impl ItemState {
    /// Whether something happened to it after its last deletion: a copy, or
    /// a change of its mark or note.
    pub fn alive(&self) -> bool {
        alive_at(self.copied_at, self.meta_at, self.deleted_at)
    }

    /// The two states of one item (the same hash) as one: the latest copy,
    /// the latest mark and note (ties go to the larger, so every device
    /// picks the same), the latest deletion. Alive, it keeps the text
    /// either has.
    pub fn merge(&self, other: &ItemState) -> ItemState {
        debug_assert_eq!(self.hash, other.hash);
        let marked = if mark_order(other) > mark_order(self) {
            other
        } else {
            self
        };
        let copied_at = self.copied_at.max(other.copied_at);
        let deleted_at = self.deleted_at.max(other.deleted_at);
        let text = if alive_at(copied_at, marked.meta_at, deleted_at) {
            self.text.clone().or_else(|| other.text.clone())
        } else {
            None
        };
        ItemState {
            hash: self.hash.clone(),
            text,
            copied_at,
            meta_at: marked.meta_at,
            favorite: marked.favorite,
            note: marked.note.clone(),
            deleted_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alive(copied_at: i64) -> ItemState {
        ItemState {
            hash: "h".into(),
            text: Some("text".into()),
            copied_at,
            meta_at: 0,
            favorite: false,
            note: String::new(),
            deleted_at: 0,
        }
    }

    fn marked(copied_at: i64, meta_at: i64, favorite: bool, note: &str) -> ItemState {
        ItemState {
            meta_at,
            favorite,
            note: note.into(),
            ..alive(copied_at)
        }
    }

    /// As a device deleting it leaves it: no text, mark and note cleared.
    fn deleted(copied_at: i64, at: i64) -> ItemState {
        ItemState {
            text: None,
            meta_at: at,
            deleted_at: at,
            ..alive(copied_at)
        }
    }

    fn states() -> Vec<ItemState> {
        vec![
            alive(10),
            alive(30),
            marked(10, 20, true, "x"),
            marked(10, 20, false, "y"),
            marked(10, 20, true, "a"),
            deleted(10, 15),
            deleted(10, 25),
            marked(5, 40, true, ""),
        ]
    }

    #[test]
    fn the_merge_ends_alike_in_any_order_and_any_number_of_times() {
        let states = states();
        for a in &states {
            assert_eq!(&a.merge(a), a, "idempotent");
            for b in &states {
                assert_eq!(a.merge(b), b.merge(a), "commutative");
                for c in &states {
                    assert_eq!(
                        a.merge(b).merge(c),
                        a.merge(&b.merge(c)),
                        "associative"
                    );
                }
            }
        }
    }

    #[test]
    fn the_latest_copy_and_the_latest_mark_win() {
        let merged = alive(30).merge(&marked(10, 20, true, "note"));
        assert_eq!(merged.copied_at, 30);
        assert!(merged.favorite);
        assert_eq!(merged.note, "note");
        assert_eq!(merged.text.as_deref(), Some("text"));
    }

    #[test]
    fn a_deletion_wins_over_what_came_before_it() {
        let merged = marked(10, 12, true, "note").merge(&deleted(10, 15));
        assert!(!merged.alive());
        assert_eq!(merged.text, None);
        assert!(!merged.favorite);
        assert_eq!(merged.note, "");
    }

    #[test]
    fn copying_it_again_after_a_deletion_brings_it_back_unmarked() {
        let merged = deleted(10, 15).merge(&alive(20));
        assert!(merged.alive());
        assert_eq!(merged.text.as_deref(), Some("text"));
        assert!(!merged.favorite);
    }

    #[test]
    fn marking_it_after_a_deletion_brings_it_back() {
        let merged = deleted(10, 15).merge(&marked(10, 18, true, ""));
        assert!(merged.alive());
        assert!(merged.favorite);
        assert_eq!(merged.text.as_deref(), Some("text"));
    }

    #[test]
    fn notes_changed_at_the_same_moment_settle_the_same_everywhere() {
        let a = marked(10, 20, false, "from the phone");
        let b = marked(10, 20, false, "from the Mac");
        assert_eq!(a.merge(&b).note, "from the phone");
        assert_eq!(b.merge(&a).note, "from the phone");
    }

    #[test]
    fn a_deleted_item_goes_out_without_its_text() {
        let json = serde_json::to_string(&deleted(10, 15)).unwrap();
        assert!(!json.contains("text"));
        let back: ItemState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, deleted(10, 15));
    }
}
