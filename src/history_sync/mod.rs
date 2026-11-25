//! Syncing the clipboard history's text items between devices, through the
//! vault's sync server: each item's state sealed with a key derived from
//! the vault's data key, so only while the vault is unlocked.

pub mod item;
pub(crate) mod ledger;
pub mod seal;
