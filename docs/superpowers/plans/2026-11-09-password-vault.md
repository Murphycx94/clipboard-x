# 密码本 Implementation Plan

> **Status (2026-11-09):** implemented. After a security review the code deviates from the snippets below in these places; the code is authoritative: `VaultService::verify` (one backoff-gated password check for unlock, change password, enable Touch ID and export); a lock generation counter (`open_session` refuses after a lock); `change_password` rotates the data key; `vault_meta.key_check`; `export(master, backup_password)`; atomic imports comparing the encrypted `updated_at`; TOTP period/digit limits; secret auto-clear keyed to the latest copy.

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an encrypted password vault (accounts, passwords, URLs, TOTP, notes; generator; encrypted backup) to ClipboardX, per `docs/superpowers/specs/2026-11-09-password-vault-design.md`.

**Architecture:** A `src/vault/` module owns everything secret: pure crypto (Argon2id KEK wrapping a random XChaCha20-Poly1305 DEK), SQL over ciphertext only, a `VaultService` holding the unlocked key in memory, TOTP, the generator, `.cxvault` backups and (macOS) a Touch ID–gated Keychain copy of the DEK. `Services` owns one `VaultService`; the UI shows summaries (never passwords) and asks the service to copy secrets, which go to the clipboard marked concealed and are cleared after 30 s. The macOS host locks the vault when the panel hides (if enabled), on sleep and on screen lock.

**Tech Stack:** Rust 2024, NSH 0.4 (nsh-core / nsh-base), rusqlite, argon2 0.6, chacha20poly1305 0.11, zeroize 1.9 (derive), getrandom 0.4, totp-rs 6, uuid 1, arboard 3.7; macOS: security-framework 3.7, objc2-local-authentication 0.3, block2 0.6.

**Conventions for this repo**
- Run everything from `/Users/murphy/Desktop/dev/ClipboardX`.
- Unit tests live in `#[cfg(test)] mod tests` in the same file; UI tests in `tests/`.
- After each task: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test` must be clean.
- The branch's NSH rewrite is not committed yet; the user decides when to commit. "Checkpoint" steps below are where a commit would go: run the checks and stop for review instead of committing.
- Never log, `Debug`-print or put into an error message any vault plaintext.

---

## File map

| File | Responsibility |
|---|---|
| `src/vault/mod.rs` | `VaultService`: setup, unlock/lock, backoff, CRUD, copy, password change, export/import, Touch ID |
| `src/vault/crypto.rs` | `KdfParams`, `SecretKey`, `derive_key`, `seal`/`open`, `wrap_key`/`unwrap_key` (pure) |
| `src/vault/entry.rs` | `VaultEntry` (zeroized), `EntrySummary`, `host_of`, `matches` |
| `src/vault/store.rs` | `vault_meta` / `vault_entries` SQL (ciphertext only) |
| `src/vault/generator.rs` | Password generator |
| `src/vault/totp.rs` | TOTP parse / validate / current code |
| `src/vault/backup.rs` | `.cxvault` encode / decode |
| `src/vault/keychain.rs` | `KeyStore` trait, `MemoryKeyStore` (tests), `NoKeyStore`, macOS `MacKeyStore` |
| `src/vault/error.rs` | `VaultError` with user-facing Chinese messages |
| `src/db.rs` | schema hook, `with_conn`, three new settings |
| `src/model.rs` | `Settings` fields `vault_lock_minutes`, `vault_lock_on_hide`, `vault_touch_id` |
| `src/capture.rs` | `copy_concealed`, concealed-content skip, 30 s auto-clear |
| `src/services.rs` | owns `VaultService`, idle/hide lock hooks |
| `src/macos.rs` | lock on hide / sleep / screen lock; concealed pasteboard check |
| `src/ui/vault/{mod,lock,list,editor}.rs` | vault page |
| `src/ui/{state,header,keys,settings,dialogs}.rs` | page entry, keys, settings group, vault dialogs |
| `tests/vault_ui.rs` | headless UI scenario |

---

### Task 1: Dependencies and `vault::crypto`

**Files:**
- Modify: `Cargo.toml`
- Create: `src/vault/mod.rs`, `src/vault/crypto.rs`
- Modify: `src/lib.rs` (add `pub mod vault;`)

- [ ] **Step 1: Dependencies**

Run:
```bash
cargo add zeroize --features derive
cargo add getrandom@0.4 argon2@0.6 chacha20poly1305@0.11 uuid --features uuid/v4
cargo add totp-rs@6 --features otpauth
cargo add --target 'cfg(target_os = "macos")' security-framework@3 objc2-local-authentication@0.3 block2@0.6
```
Expected: `Cargo.toml` lists all of them; no `rand`.

- [ ] **Step 2: Module skeleton**

`src/vault/mod.rs`:
```rust
//! The password vault: everything secret lives under this module.

pub mod crypto;
```
`src/lib.rs`: add `pub mod vault;` after `pub mod updater;`.

- [ ] **Step 3: Write the failing tests** (bottom of `src/vault/crypto.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const P: KdfParams = KdfParams::INSECURE_TEST;

    #[test]
    fn same_password_and_salt_derive_the_same_key() {
        let a = derive_key("correct horse", b"0123456789abcdef", P).unwrap();
        let b = derive_key("correct horse", b"0123456789abcdef", P).unwrap();
        let c = derive_key("correct horsf", b"0123456789abcdef", P).unwrap();
        assert_eq!(a.as_bytes(), b.as_bytes());
        assert_ne!(a.as_bytes(), c.as_bytes());
    }

    #[test]
    fn sealed_data_opens_with_the_key_and_aad() {
        let key = SecretKey::random().unwrap();
        let sealed = seal(&key, b"secret", b"id-1").unwrap();
        assert_eq!(&*open(&key, &sealed, b"id-1").unwrap(), b"secret");
    }

    #[test]
    fn tampering_wrong_key_or_wrong_aad_fail() {
        let key = SecretKey::random().unwrap();
        let sealed = seal(&key, b"secret", b"id-1").unwrap();
        assert_eq!(open(&key, &sealed, b"id-2").unwrap_err(), CryptoError::Decrypt);
        let other = SecretKey::random().unwrap();
        assert_eq!(open(&other, &sealed, b"id-1").unwrap_err(), CryptoError::Decrypt);
        let mut bytes = sealed.to_bytes();
        for at in [0, NONCE_LEN, bytes.len() - 1] {
            bytes[at] ^= 1;
            let broken = Sealed::from_bytes(&bytes).unwrap();
            assert_eq!(open(&key, &broken, b"id-1").unwrap_err(), CryptoError::Decrypt);
            bytes[at] ^= 1;
        }
    }

    #[test]
    fn nonces_are_fresh() {
        let key = SecretKey::random().unwrap();
        let a = seal(&key, b"x", b"").unwrap();
        let b = seal(&key, b"x", b"").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn keys_wrap_and_rewrap() {
        let dek = SecretKey::random().unwrap();
        let kek1 = derive_key("one", b"salt-salt-salt-1", P).unwrap();
        let kek2 = derive_key("two", b"salt-salt-salt-2", P).unwrap();
        let wrapped = wrap_key(&kek1, &dek).unwrap();
        let back = unwrap_key(&kek1, &wrapped).unwrap();
        assert_eq!(back.as_bytes(), dek.as_bytes());
        assert_eq!(unwrap_key(&kek2, &wrapped).unwrap_err(), CryptoError::Decrypt);
        let rewrapped = wrap_key(&kek2, &back).unwrap();
        assert_eq!(unwrap_key(&kek2, &rewrapped).unwrap().as_bytes(), dek.as_bytes());
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert_eq!(Sealed::from_bytes(&[0; 10]).unwrap_err(), CryptoError::Malformed);
        assert_eq!(SecretKey::from_slice(&[0; 31]).unwrap_err(), CryptoError::Malformed);
    }

    #[test]
    fn debug_never_shows_key_bytes() {
        let key = SecretKey::from_slice(&[7; KEY_LEN]).unwrap();
        assert_eq!(format!("{key:?}"), "SecretKey(..)");
    }
}
```

- [ ] **Step 4: Run to see them fail**

Run: `cargo test --lib vault::crypto`
Expected: compile errors (items not defined).

- [ ] **Step 5: Implement** (top of `src/vault/crypto.rs`)

```rust
//! The vault's cryptography, pure: Argon2id derives a key-encryption key
//! from the master password; a random data key encrypts the entries with
//! XChaCha20-Poly1305 and is itself stored wrapped by the derived key.

use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::XNonce;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub const KEY_LEN: usize = 32;
pub const SALT_LEN: usize = 16;
pub const NONCE_LEN: usize = 24;
/// Poly1305's tag.
const TAG_LEN: usize = 16;
/// Binds a wrapped data key to its purpose.
const DEK_AAD: &[u8] = b"clipboardx/vault/dek/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// The OS gave no randomness.
    Random,
    /// Bad KDF parameters.
    Kdf,
    Encrypt,
    /// Wrong key, wrong associated data, or tampered data.
    Decrypt,
    Malformed,
}

impl fmt::Display for CryptoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CryptoError::Random => "no system randomness",
            CryptoError::Kdf => "invalid key derivation parameters",
            CryptoError::Encrypt => "encryption failed",
            CryptoError::Decrypt => "decryption failed",
            CryptoError::Malformed => "malformed encrypted data",
        })
    }
}

impl std::error::Error for CryptoError {}

/// Argon2id costs: memory in KiB, iterations, lanes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub m: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfParams {
    /// 64 MiB, 3 passes, 1 lane.
    pub const DEFAULT: KdfParams = KdfParams { m: 64 * 1024, t: 3, p: 1 };
    /// Argon2's minimum: for tests only.
    pub const INSECURE_TEST: KdfParams = KdfParams { m: 8, t: 1, p: 1 };
}

/// 256 secret bits, wiped when dropped.
pub struct SecretKey(Zeroizing<[u8; KEY_LEN]>);

impl SecretKey {
    pub fn random() -> Result<SecretKey, CryptoError> {
        Ok(SecretKey(Zeroizing::new(random_bytes::<KEY_LEN>()?)))
    }

    pub fn from_slice(bytes: &[u8]) -> Result<SecretKey, CryptoError> {
        let array: [u8; KEY_LEN] = bytes.try_into().map_err(|_| CryptoError::Malformed)?;
        Ok(SecretKey(Zeroizing::new(array)))
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        // The length is KEY_LEN by construction.
        XChaCha20Poly1305::new_from_slice(&self.0[..])
            .unwrap_or_else(|_| unreachable!("a key is {KEY_LEN} bytes"))
    }
}

impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey(..)")
    }
}

/// `N` bytes from the OS's CSPRNG.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).map_err(|_| CryptoError::Random)?;
    Ok(bytes)
}

/// The key-encryption key of `password`.
pub fn derive_key(password: &str, salt: &[u8], params: KdfParams) -> Result<SecretKey, CryptoError> {
    let costs = Params::new(params.m, params.t, params.p, Some(KEY_LEN)).map_err(|_| CryptoError::Kdf)?;
    let mut out = Zeroizing::new([0_u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, costs)
        .hash_password_into(password.as_bytes(), salt, out.as_mut())
        .map_err(|_| CryptoError::Kdf)?;
    Ok(SecretKey(out))
}

/// A nonce and the ciphertext (with its tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

impl Sealed {
    /// `nonce || ciphertext`.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(NONCE_LEN + self.ciphertext.len());
        bytes.extend_from_slice(&self.nonce);
        bytes.extend_from_slice(&self.ciphertext);
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Sealed, CryptoError> {
        Sealed::from_parts(bytes.get(..NONCE_LEN).unwrap_or_default(), bytes.get(NONCE_LEN..).unwrap_or_default())
    }

    pub fn from_parts(nonce: &[u8], ciphertext: &[u8]) -> Result<Sealed, CryptoError> {
        let nonce: [u8; NONCE_LEN] = nonce.try_into().map_err(|_| CryptoError::Malformed)?;
        if ciphertext.len() < TAG_LEN {
            return Err(CryptoError::Malformed);
        }
        Ok(Sealed { nonce, ciphertext: ciphertext.to_vec() })
    }
}

/// Encrypts `plaintext` bound to `aad`, under a fresh random nonce.
pub fn seal(key: &SecretKey, plaintext: &[u8], aad: &[u8]) -> Result<Sealed, CryptoError> {
    let nonce = random_bytes::<NONCE_LEN>()?;
    let ciphertext = key
        .cipher()
        .encrypt(&XNonce::from(nonce), Payload { msg: plaintext, aad })
        .map_err(|_| CryptoError::Encrypt)?;
    Ok(Sealed { nonce, ciphertext })
}

/// Decrypts what [`seal`] made with the same key and `aad`.
pub fn open(key: &SecretKey, sealed: &Sealed, aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    key.cipher()
        .decrypt(&XNonce::from(sealed.nonce), Payload { msg: &sealed.ciphertext, aad })
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::Decrypt)
}

/// The data key encrypted by the key-encryption key, as stored.
pub fn wrap_key(kek: &SecretKey, dek: &SecretKey) -> Result<Vec<u8>, CryptoError> {
    Ok(seal(kek, dek.as_bytes(), DEK_AAD)?.to_bytes())
}

pub fn unwrap_key(kek: &SecretKey, wrapped: &[u8]) -> Result<SecretKey, CryptoError> {
    let plain = open(kek, &Sealed::from_bytes(wrapped)?, DEK_AAD)?;
    SecretKey::from_slice(&plain)
}
```

- [ ] **Step 6: Run the tests**

Run: `cargo test --lib vault::crypto`
Expected: 7 passed.

- [ ] **Step 7: Checkpoint** — fmt, clippy, full `cargo test` clean.

---

### Task 2: Entries and the password generator

**Files:**
- Create: `src/vault/entry.rs`, `src/vault/generator.rs`
- Modify: `src/vault/mod.rs` (add `pub mod entry; pub mod generator;`)

- [ ] **Step 1: Failing tests**

`src/vault/entry.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> VaultEntry {
        VaultEntry {
            title: "GitHub".into(),
            username: "octo@example.com".into(),
            password: "hunter2-secret".into(),
            url: "https://www.github.com/login?x=1".into(),
            totp: "JBSWY3DPEHPK3PXP".into(),
            note: "work".into(),
            ..VaultEntry::default()
        }
    }

    #[test]
    fn summary_has_no_secrets() {
        let summary = entry().summary("id-1");
        assert_eq!(summary.title, "GitHub");
        assert_eq!(summary.host, "github.com");
        assert!(summary.has_totp);
        assert!(!format!("{summary:?}").contains("hunter2"));
    }

    #[test]
    fn debug_hides_secrets() {
        let shown = format!("{:?}", entry());
        assert!(!shown.contains("hunter2") && !shown.contains("JBSWY3DP"));
    }

    #[test]
    fn hosts() {
        assert_eq!(host_of("https://user:pw@www.example.com:8443/a"), "example.com:8443");
        assert_eq!(host_of("example.com/path"), "example.com");
        assert_eq!(host_of(""), "");
    }

    #[test]
    fn search_matches_title_username_and_host() {
        let summary = entry().summary("id-1");
        assert!(summary.matches("git"));
        assert!(summary.matches("OCTO"));
        assert!(summary.matches("github.com"));
        assert!(summary.matches("  "));
        assert!(!summary.matches("hunter2"));
    }

    #[test]
    fn json_round_trip_and_unknown_fields_default() {
        let json = serde_json::to_vec(&entry()).unwrap();
        let back: VaultEntry = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, entry());
        let partial: VaultEntry = serde_json::from_str(r#"{"title":"x"}"#).unwrap();
        assert_eq!(partial.title, "x");
        assert!(partial.password.is_empty());
    }
}
```

`src/vault/generator.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_password_has_every_class() {
        let pw = generate(&GeneratorOptions::default()).unwrap();
        assert_eq!(pw.chars().count(), 20);
        assert!(pw.chars().any(|c| c.is_ascii_uppercase()));
        assert!(pw.chars().any(|c| c.is_ascii_lowercase()));
        assert!(pw.chars().any(|c| c.is_ascii_digit()));
        assert!(pw.chars().any(|c| SYMBOLS.contains(c)));
        assert!(!pw.chars().any(|c| AMBIGUOUS.contains(c)));
    }

    #[test]
    fn length_is_clamped() {
        let short = generate(&GeneratorOptions { length: 2, ..Default::default() }).unwrap();
        assert_eq!(short.len(), MIN_LENGTH);
        let long = generate(&GeneratorOptions { length: 500, ..Default::default() }).unwrap();
        assert_eq!(long.len(), MAX_LENGTH);
    }

    #[test]
    fn single_class_and_no_class() {
        let opts = GeneratorOptions { upper: false, lower: false, symbols: false, ..Default::default() };
        assert!(generate(&opts).unwrap().chars().all(|c| c.is_ascii_digit()));
        let none = GeneratorOptions { upper: false, lower: false, digits: false, symbols: false, ..Default::default() };
        assert_eq!(generate(&none).unwrap_err(), GeneratorError::NoClasses);
    }

    #[test]
    fn random_below_is_in_range_and_roughly_uniform() {
        let mut counts = [0_u32; 7];
        for _ in 0..7000 {
            counts[random_below(7).unwrap() as usize] += 1;
        }
        assert!(counts.iter().all(|&n| (700..1300).contains(&n)), "{counts:?}");
    }
}
```

- [ ] **Step 2: Run to fail** — `cargo test --lib vault::` → compile errors.

- [ ] **Step 3: Implement `entry.rs`**

```rust
//! A vault entry (secret, zeroized) and its summary (no secrets) for lists.

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// One account, as encrypted in the database.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
#[serde(default)]
pub struct VaultEntry {
    pub title: String,
    pub username: String,
    pub password: String,
    pub url: String,
    /// An `otpauth://` URI or a Base32 secret; empty without one.
    pub totp: String,
    pub note: String,
    pub created_at: String,
    pub updated_at: String,
}

impl fmt::Debug for VaultEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VaultEntry").field("title", &self.title).finish_non_exhaustive()
    }
}

impl VaultEntry {
    pub fn summary(&self, id: &str) -> EntrySummary {
        EntrySummary {
            id: id.to_owned(),
            title: self.title.clone(),
            username: self.username.clone(),
            host: host_of(&self.url),
            has_totp: !self.totp.trim().is_empty(),
            corrupt: false,
        }
    }
}

/// What a list shows of an entry: never its password, TOTP secret or note.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySummary {
    pub id: String,
    pub title: String,
    pub username: String,
    pub host: String,
    pub has_totp: bool,
    /// It failed to decrypt.
    pub corrupt: bool,
}

impl EntrySummary {
    pub fn corrupt(id: &str) -> EntrySummary {
        EntrySummary {
            id: id.to_owned(),
            title: "（已损坏）".to_owned(),
            username: String::new(),
            host: String::new(),
            has_totp: false,
            corrupt: true,
        }
    }

    /// Whether `query` is in the title, the username or the host.
    pub fn matches(&self, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        query.is_empty()
            || [&self.title, &self.username, &self.host]
                .iter()
                .any(|field| field.to_lowercase().contains(&query))
    }
}

/// `example.com` of `https://user@www.example.com/path`.
pub fn host_of(url: &str) -> String {
    let rest = url.trim();
    let rest = rest.split_once("://").map_or(rest, |(_, r)| r);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    host.strip_prefix("www.").unwrap_or(host).to_lowercase()
}
```

- [ ] **Step 4: Implement `generator.rs`**

```rust
//! Strong random passwords.

use super::crypto::{CryptoError, random_bytes};

pub const MIN_LENGTH: usize = 8;
pub const MAX_LENGTH: usize = 64;
const UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
const DIGITS: &str = "0123456789";
pub(crate) const SYMBOLS: &str = "!@#$%^&*()-_=+[]{};:,.?/";
pub(crate) const AMBIGUOUS: &str = "0O1lI";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratorOptions {
    pub length: usize,
    pub upper: bool,
    pub lower: bool,
    pub digits: bool,
    pub symbols: bool,
    pub exclude_ambiguous: bool,
}

impl Default for GeneratorOptions {
    fn default() -> Self {
        GeneratorOptions { length: 20, upper: true, lower: true, digits: true, symbols: true, exclude_ambiguous: true }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratorError {
    NoClasses,
    Random,
}

impl From<CryptoError> for GeneratorError {
    fn from(_: CryptoError) -> Self {
        GeneratorError::Random
    }
}

/// A uniform random number below `n` (> 0), without modulo bias.
pub(crate) fn random_below(n: u32) -> Result<u32, GeneratorError> {
    let zone = u32::MAX - (u32::MAX % n);
    loop {
        let r = u32::from_le_bytes(random_bytes::<4>()?);
        if r < zone {
            return Ok(r % n);
        }
    }
}

fn pick(chars: &[char]) -> Result<char, GeneratorError> {
    Ok(chars[random_below(chars.len() as u32)? as usize])
}

/// A password with at least one character of each chosen class.
pub fn generate(options: &GeneratorOptions) -> Result<String, GeneratorError> {
    let classes: Vec<Vec<char>> = [(options.upper, UPPER), (options.lower, LOWER), (options.digits, DIGITS), (options.symbols, SYMBOLS)]
        .into_iter()
        .filter(|(on, _)| *on)
        .map(|(_, set)| set.chars().filter(|c| !(options.exclude_ambiguous && AMBIGUOUS.contains(*c))).collect())
        .collect();
    if classes.is_empty() {
        return Err(GeneratorError::NoClasses);
    }
    let length = options.length.clamp(MIN_LENGTH, MAX_LENGTH);
    let all: Vec<char> = classes.concat();
    let mut chars: Vec<char> = classes.iter().map(|class| pick(class)).collect::<Result<_, _>>()?;
    while chars.len() < length {
        chars.push(pick(&all)?);
    }
    for i in (1..chars.len()).rev() {
        let j = random_below(i as u32 + 1)? as usize;
        chars.swap(i, j);
    }
    Ok(chars.into_iter().collect())
}
```

- [ ] **Step 5: Run** — `cargo test --lib vault::` → all pass.
- [ ] **Step 6: Checkpoint.**

---

### Task 3: TOTP

**Files:** Create `src/vault/totp.rs`; `src/vault/mod.rs` add `pub mod totp;`.

- [ ] **Step 1: Failing tests** (RFC 6238 Appendix B; secrets are the ASCII strings in the RFC)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 seeds: "12345678901234567890" (SHA1), 32 bytes (SHA256), 64 bytes (SHA512).
    const SHA1: &str = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&algorithm=SHA1&digits=8&period=30";
    const SHA256: &str = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA&algorithm=SHA256&digits=8&period=30";
    const SHA512: &str = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNA&algorithm=SHA512&digits=8&period=30";

    #[test]
    fn rfc6238_vectors() {
        for (time, sha1, sha256, sha512) in [
            (59, "94287082", "46119246", "90693936"),
            (1_111_111_109, "07081804", "68084774", "25091201"),
            (2_000_000_000, "69279037", "90698825", "38618901"),
        ] {
            assert_eq!(code_at(SHA1, time).unwrap().code, sha1);
            assert_eq!(code_at(SHA256, time).unwrap().code, sha256);
            assert_eq!(code_at(SHA512, time).unwrap().code, sha512);
        }
    }

    #[test]
    fn plain_base32_uses_defaults_and_accepts_short_secrets() {
        // 80-bit secret, lowercase with spaces, as sites show them.
        let code = code_at("jbsw y3dp ehpk 3pxp", 59).unwrap();
        assert_eq!(code.code.len(), 6);
        assert_eq!(code.period, 30);
        assert_eq!(code.remaining, 1);
    }

    #[test]
    fn invalid_input_is_rejected() {
        assert!(validate("").is_err());
        assert!(validate("not base32 !!").is_err());
        assert!(validate("otpauth://hotp/x?secret=JBSWY3DPEHPK3PXP").is_err());
        assert!(validate("JBSWY3DPEHPK3PXP").is_ok());
    }
}
```

- [ ] **Step 2: Run to fail.**

- [ ] **Step 3: Implement**

```rust
//! Time-based one-time codes (RFC 6238) from an `otpauth://` URI or a
//! Base32 secret.

use totp_rs::{Builder, Secret, Totp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpCode {
    pub code: String,
    /// Seconds the code stays valid.
    pub remaining: u64,
    pub period: u64,
}

fn parse(input: &str) -> Result<Totp, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("验证码密钥为空".into());
    }
    if input.to_ascii_lowercase().starts_with("otpauth://") {
        return Totp::from_url_unchecked(input).map_err(|_| "无效的 otpauth 链接".to_owned());
    }
    let cleaned: String = input.chars().filter(|c| !c.is_whitespace() && *c != '-').collect::<String>().to_ascii_uppercase();
    let secret = Secret::try_from_base32(cleaned.trim_end_matches('=')).map_err(|_| "无效的 Base32 密钥".to_owned())?;
    if secret.is_empty() {
        return Err("无效的 Base32 密钥".into());
    }
    Ok(Builder::new().with_secret(secret).build_noncompliant())
}

/// Whether `input` is a usable TOTP secret.
pub fn validate(input: &str) -> Result<(), String> {
    parse(input).map(|_| ())
}

/// The code at `unix_secs`.
pub fn code_at(input: &str, unix_secs: u64) -> Result<TotpCode, String> {
    let totp = parse(input)?;
    let period = totp.step();
    Ok(TotpCode { code: totp.generate(unix_secs).to_string(), remaining: period - unix_secs % period, period })
}

/// The current code.
pub fn current(input: &str) -> Result<TotpCode, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    code_at(input, now)
}
```

Note: if `Secret` has no `is_empty`, use `secret.as_ref().is_empty()` (it derefs to `[u8]`).

- [ ] **Step 4: Run** → 3 passed. If `Secret::try_from_base32` rejects padding-less 8-digit groups, keep `trim_end_matches('=')` and pad to a multiple of 8 with `=` before parsing.
- [ ] **Step 5: Checkpoint.**

---

### Task 4: Storage and settings

**Files:** Create `src/vault/store.rs`; modify `src/db.rs`, `src/model.rs`, `src/vault/mod.rs` (`pub mod store;`).

- [ ] **Step 1: Failing tests** (`src/vault/store.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    fn meta() -> MetaRow {
        MetaRow { version: 1, params: crate::vault::crypto::KdfParams::INSECURE_TEST, salt: vec![1; 16], wrapped_dek: vec![2; 72], failed_tries: 0, locked_until_ms: 0 }
    }

    #[test]
    fn meta_round_trips_and_counts_failures() {
        let db = Database::in_memory().unwrap();
        db.with_conn(|c| {
            assert_eq!(load_meta(c).unwrap(), None);
            save_meta(c, &meta()).unwrap();
            assert_eq!(load_meta(c).unwrap(), Some(meta()));
            set_failures(c, 4, 1234).unwrap();
            let m = load_meta(c).unwrap().unwrap();
            assert_eq!((m.failed_tries, m.locked_until_ms), (4, 1234));
        });
    }

    #[test]
    fn entries_upsert_delete_replace() {
        let db = Database::in_memory().unwrap();
        db.with_conn(|c| {
            let row = |id: &str, t: &str| EntryRow { id: id.into(), nonce: vec![0; 24], ciphertext: vec![9; 20], updated_at: t.into() };
            upsert_entry(c, &row("a", "1")).unwrap();
            upsert_entry(c, &row("b", "1")).unwrap();
            upsert_entry(c, &row("a", "2")).unwrap();
            assert_eq!(load_entries(c).unwrap().len(), 2);
            assert_eq!(load_entry(c, "a").unwrap().unwrap().updated_at, "2");
            delete_entry(c, "a").unwrap();
            assert!(load_entry(c, "a").unwrap().is_none());
            replace_entries(c, &[row("c", "3")]).unwrap();
            assert_eq!(load_entries(c).unwrap().iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), vec!["c"]);
            wipe(c).unwrap();
            assert!(load_meta(c).unwrap().is_none() && load_entries(c).unwrap().is_empty());
        });
    }
}
```
In `src/db.rs` tests add:
```rust
    #[test]
    fn vault_settings_have_defaults_and_save() {
        let db = db();
        let s = db.settings().unwrap();
        assert_eq!((s.vault_lock_minutes, s.vault_lock_on_hide, s.vault_touch_id), (5, true, false));
        db.set_vault_lock_minutes(15).unwrap();
        db.set_vault_lock_on_hide(false).unwrap();
        db.set_vault_touch_id(true).unwrap();
        let s = db.settings().unwrap();
        assert_eq!((s.vault_lock_minutes, s.vault_lock_on_hide, s.vault_touch_id), (15, false, true));
    }
```

- [ ] **Step 2: Run to fail.**

- [ ] **Step 3: Implement `store.rs`**

```rust
//! The vault's tables. Only ciphertext and non-secret metadata pass here.

use rusqlite::{Connection, OptionalExtension, Result, params};

use super::crypto::KdfParams;

pub const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS vault_meta (
        id           INTEGER PRIMARY KEY CHECK (id = 1),
        version      INTEGER NOT NULL,
        kdf_params   TEXT NOT NULL,
        salt         BLOB NOT NULL,
        wrapped_dek  BLOB NOT NULL,
        failed_tries INTEGER NOT NULL DEFAULT 0,
        locked_until INTEGER NOT NULL DEFAULT 0
    );
    CREATE TABLE IF NOT EXISTS vault_entries (
        id         TEXT PRIMARY KEY,
        nonce      BLOB NOT NULL,
        ciphertext BLOB NOT NULL,
        updated_at TEXT NOT NULL
    );
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaRow {
    pub version: i64,
    pub params: KdfParams,
    pub salt: Vec<u8>,
    pub wrapped_dek: Vec<u8>,
    pub failed_tries: u32,
    /// Unix ms before which no unlock is tried.
    pub locked_until_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryRow {
    pub id: String,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub updated_at: String,
}

fn bad_params(e: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
}

pub fn load_meta(conn: &Connection) -> Result<Option<MetaRow>> {
    conn.query_row(
        "SELECT version, kdf_params, salt, wrapped_dek, failed_tries, locked_until FROM vault_meta WHERE id = 1",
        [],
        |r| {
            let params: String = r.get(1)?;
            Ok(MetaRow {
                version: r.get(0)?,
                params: serde_json::from_str(&params).map_err(bad_params)?,
                salt: r.get(2)?,
                wrapped_dek: r.get(3)?,
                failed_tries: r.get(4)?,
                locked_until_ms: r.get::<_, i64>(5)?.max(0) as u64,
            })
        },
    )
    .optional()
}

pub fn save_meta(conn: &Connection, meta: &MetaRow) -> Result<()> {
    let params = serde_json::to_string(&meta.params).map_err(bad_params)?;
    conn.execute(
        "INSERT OR REPLACE INTO vault_meta (id, version, kdf_params, salt, wrapped_dek, failed_tries, locked_until)
         VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![meta.version, params, meta.salt, meta.wrapped_dek, meta.failed_tries, meta.locked_until_ms as i64],
    )?;
    Ok(())
}

pub fn set_failures(conn: &Connection, tries: u32, locked_until_ms: u64) -> Result<()> {
    conn.execute(
        "UPDATE vault_meta SET failed_tries = ?1, locked_until = ?2 WHERE id = 1",
        params![tries, locked_until_ms as i64],
    )?;
    Ok(())
}

fn row(r: &rusqlite::Row) -> Result<EntryRow> {
    Ok(EntryRow { id: r.get(0)?, nonce: r.get(1)?, ciphertext: r.get(2)?, updated_at: r.get(3)? })
}

pub fn load_entries(conn: &Connection) -> Result<Vec<EntryRow>> {
    let mut stmt = conn.prepare("SELECT id, nonce, ciphertext, updated_at FROM vault_entries")?;
    let rows = stmt.query_map([], row)?;
    rows.collect()
}

pub fn load_entry(conn: &Connection, id: &str) -> Result<Option<EntryRow>> {
    conn.query_row("SELECT id, nonce, ciphertext, updated_at FROM vault_entries WHERE id = ?1", params![id], row)
        .optional()
}

pub fn upsert_entry(conn: &Connection, entry: &EntryRow) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO vault_entries (id, nonce, ciphertext, updated_at) VALUES (?1, ?2, ?3, ?4)",
        params![entry.id, entry.nonce, entry.ciphertext, entry.updated_at],
    )?;
    Ok(())
}

pub fn delete_entry(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM vault_entries WHERE id = ?1", params![id])?;
    Ok(())
}

/// Replaces every entry, atomically.
pub fn replace_entries(conn: &Connection, entries: &[EntryRow]) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    tx.execute("DELETE FROM vault_entries", [])?;
    for entry in entries {
        upsert_entry(&tx, entry)?;
    }
    tx.commit()
}

/// Deletes the vault: its key and every entry.
pub fn wipe(conn: &Connection) -> Result<()> {
    conn.execute_batch("DELETE FROM vault_entries; DELETE FROM vault_meta;")
}
```

- [ ] **Step 4: `db.rs` changes**
  - In `Database::init`, after `conn.execute_batch(SCHEMA)?;` add `conn.execute_batch(crate::vault::store::SCHEMA)?;`.
  - In `mod key` add `VAULT_LOCK_MINUTES = "vault_lock_minutes"`, `VAULT_LOCK_ON_HIDE = "vault_lock_on_hide"`, `VAULT_TOUCH_ID = "vault_touch_id"`; seed them in `init`'s defaults list from `Settings::default()`.
  - Add:
```rust
    /// Runs `f` with the connection (the vault's store).
    pub(crate) fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> R) -> R {
        f(&self.conn())
    }

    pub fn set_vault_lock_minutes(&self, minutes: i64) -> Result<()> {
        self.set_setting(key::VAULT_LOCK_MINUTES, &minutes.to_string())
    }

    pub fn set_vault_lock_on_hide(&self, on: bool) -> Result<()> {
        self.set_setting(key::VAULT_LOCK_ON_HIDE, &on.to_string())
    }

    pub fn set_vault_touch_id(&self, on: bool) -> Result<()> {
        self.set_setting(key::VAULT_TOUCH_ID, &on.to_string())
    }
```
  - In `settings()` read them: minutes parsed with default; booleans `!= "false"` for lock_on_hide, `== "true"` for touch_id.
- [ ] **Step 5: `model.rs`** — add to `Settings` (and its `Default`, and its `Debug`): `pub vault_lock_minutes: i64` (5), `pub vault_lock_on_hide: bool` (true), `pub vault_touch_id: bool` (false); add `pub const VAULT_LOCK_OPTIONS: [(i64, &str); 4] = [(1, "1 分钟"), (5, "5 分钟"), (15, "15 分钟"), (30, "30 分钟")];`.
- [ ] **Step 6: Run** → new tests pass; `new_database_has_default_settings` still passes.
- [ ] **Step 7: Checkpoint.**

---

### Task 5: Errors, key store trait and `VaultService` core

**Files:** Create `src/vault/error.rs`, `src/vault/keychain.rs` (trait + memory/none impls only); rewrite `src/vault/mod.rs`.

- [ ] **Step 1: Failing tests** (`src/vault/mod.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;
    use crate::vault::keychain::MemoryKeyStore;

    pub(crate) fn service() -> VaultService {
        VaultService::with_params(Arc::new(Database::in_memory().unwrap()), Box::new(MemoryKeyStore::default()), KdfParams::INSECURE_TEST)
    }

    fn entry(title: &str, password: &str) -> VaultEntry {
        VaultEntry { title: title.into(), username: "me".into(), password: password.into(), ..VaultEntry::default() }
    }

    #[test]
    fn setup_unlock_lock() {
        let v = service();
        assert_eq!(v.status(), VaultStatus::NotSetUp);
        assert_eq!(v.setup("short").unwrap_err(), VaultError::WeakPassword);
        v.setup("correct horse battery").unwrap();
        assert_eq!(v.status(), VaultStatus::Unlocked);
        assert_eq!(v.setup("correct horse battery").unwrap_err(), VaultError::AlreadySetUp);
        v.lock();
        assert_eq!(v.status(), VaultStatus::Locked);
        assert_eq!(v.list().unwrap_err(), VaultError::Locked);
        v.unlock("correct horse battery", 0).unwrap();
        assert_eq!(v.status(), VaultStatus::Unlocked);
    }

    #[test]
    fn crud_and_no_plaintext_on_disk() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let id = v.save(None, entry("GitHub", "hunter2-PLAIN")).unwrap();
        let other = v.save(None, entry("Mail", "x")).unwrap();
        assert_eq!(v.list().unwrap().len(), 2);
        let mut e = v.get(&id).unwrap();
        e.password = "changed-PLAIN".into();
        v.save(Some(&id), e).unwrap();
        assert_eq!(v.get(&id).unwrap().password, "changed-PLAIN");
        v.delete(&other).unwrap();
        assert_eq!(v.list().unwrap().len(), 1);
        let raw: Vec<Vec<u8>> = v.db.with_conn(|c| {
            let mut s = c.prepare("SELECT ciphertext FROM vault_entries").unwrap();
            s.query_map([], |r| r.get(0)).unwrap().map(|r| r.unwrap()).collect()
        });
        for blob in raw {
            let text = String::from_utf8_lossy(&blob);
            assert!(!text.contains("PLAIN") && !text.contains("GitHub"));
        }
    }

    #[test]
    fn wrong_passwords_back_off_and_persist() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.lock();
        assert_eq!(v.unlock("nope-nope-1", 1_000).unwrap_err(), VaultError::WrongPassword);
        assert_eq!(v.unlock("nope-nope-2", 1_000).unwrap_err(), VaultError::WrongPassword);
        assert_eq!(v.unlock("nope-nope-3", 1_000).unwrap_err(), VaultError::WrongPassword);
        // Third failure: wait 1 s, even with the right password.
        assert_eq!(v.unlock("correct horse battery", 1_500).unwrap_err(), VaultError::TooManyAttempts { wait_ms: 500 });
        v.unlock("correct horse battery", 2_000).unwrap();
        assert_eq!(v.db.with_conn(|c| store::load_meta(c).unwrap().unwrap().failed_tries), 0);
    }

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff_ms(2), 0);
        assert_eq!(backoff_ms(3), 1_000);
        assert_eq!(backoff_ms(5), 4_000);
        assert_eq!(backoff_ms(40), 60_000);
    }

    #[test]
    fn idle_lock() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.touch(10_000);
        assert!(!v.lock_if_idle(10_000 + 59_000, 1));
        assert!(v.lock_if_idle(10_000 + 61_000, 1));
        assert_eq!(v.status(), VaultStatus::Locked);
    }

    #[test]
    fn change_password_keeps_entries() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let id = v.save(None, entry("a", "pw")).unwrap();
        assert_eq!(v.change_password("wrong-old-pass", "new pass phrase").unwrap_err(), VaultError::WrongPassword);
        v.change_password("correct horse battery", "new pass phrase").unwrap();
        v.lock();
        assert_eq!(v.unlock("correct horse battery", 0).unwrap_err(), VaultError::WrongPassword);
        v.unlock("new pass phrase", 0).unwrap();
        assert_eq!(v.get(&id).unwrap().password, "pw");
    }

    #[test]
    fn corrupt_entries_are_listed_not_fatal() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let id = v.save(None, entry("a", "pw")).unwrap();
        v.db.with_conn(|c| c.execute("UPDATE vault_entries SET ciphertext = zeroblob(40)", []).unwrap());
        let list = v.list().unwrap();
        assert!(list[0].corrupt);
        assert_eq!(v.get(&id).unwrap_err(), VaultError::Corrupt);
        v.delete(&id).unwrap();
    }

    #[test]
    fn invalid_totp_is_not_saved() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        let bad = VaultEntry { title: "x".into(), totp: "not base32!".into(), ..VaultEntry::default() };
        assert!(matches!(v.save(None, bad).unwrap_err(), VaultError::InvalidTotp(_)));
    }

    #[test]
    fn reset_wipes_everything() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        v.save(None, entry("a", "pw")).unwrap();
        v.reset().unwrap();
        assert_eq!(v.status(), VaultStatus::NotSetUp);
    }
}
```

- [ ] **Step 2: Run to fail.**

- [ ] **Step 3: `error.rs`**

```rust
//! What vault operations fail with; messages are shown to the user and never
//! contain vault data.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    NotSetUp,
    AlreadySetUp,
    Locked,
    WeakPassword,
    WrongPassword,
    TooManyAttempts { wait_ms: u64 },
    Corrupt,
    NotFound,
    InvalidTotp(String),
    InvalidBackup(String),
    Keychain(String),
    Storage(String),
    Crypto,
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::NotSetUp => f.write_str("密码本尚未设置"),
            VaultError::AlreadySetUp => f.write_str("密码本已经设置过"),
            VaultError::Locked => f.write_str("密码本已锁定"),
            VaultError::WeakPassword => write!(f, "主密码至少需要 {} 个字符", super::MIN_PASSWORD_CHARS),
            VaultError::WrongPassword => f.write_str("密码错误"),
            VaultError::TooManyAttempts { wait_ms } => write!(f, "尝试次数过多，请 {} 秒后再试", wait_ms.div_ceil(1000)),
            VaultError::Corrupt => f.write_str("该条目已损坏，无法解密"),
            VaultError::NotFound => f.write_str("条目不存在"),
            VaultError::InvalidTotp(why) | VaultError::InvalidBackup(why) | VaultError::Keychain(why) => f.write_str(why),
            VaultError::Storage(_) => f.write_str("读写密码本失败"),
            VaultError::Crypto => f.write_str("加密操作失败"),
        }
    }
}

impl std::error::Error for VaultError {}

impl From<rusqlite::Error> for VaultError {
    fn from(e: rusqlite::Error) -> Self {
        log::error!("vault storage: {e}");
        VaultError::Storage(e.to_string())
    }
}

impl From<super::crypto::CryptoError> for VaultError {
    fn from(e: super::crypto::CryptoError) -> Self {
        log::error!("vault crypto: {e}");
        VaultError::Crypto
    }
}
```

- [ ] **Step 4: `keychain.rs` (trait part)**

```rust
//! Where the Touch ID copy of the data key lives.

use std::sync::Mutex;

use super::crypto::SecretKey;

pub trait KeyStore: Send + Sync {
    /// Whether this machine can unlock with Touch ID or the system password.
    fn available(&self) -> bool;
    fn save(&self, key: &SecretKey) -> Result<(), String>;
    /// Asks the user (`reason` shown) and reads the key. Blocking.
    fn load(&self, reason: &str) -> Result<SecretKey, String>;
    fn delete(&self);
}

/// No Touch ID (other platforms).
pub struct NoKeyStore;

impl KeyStore for NoKeyStore {
    fn available(&self) -> bool {
        false
    }
    fn save(&self, _: &SecretKey) -> Result<(), String> {
        Err("此平台不支持 Touch ID".into())
    }
    fn load(&self, _: &str) -> Result<SecretKey, String> {
        Err("此平台不支持 Touch ID".into())
    }
    fn delete(&self) {}
}

/// In memory, always approving (tests).
#[derive(Default)]
pub struct MemoryKeyStore(Mutex<Option<[u8; super::crypto::KEY_LEN]>>);

impl KeyStore for MemoryKeyStore {
    fn available(&self) -> bool {
        true
    }
    fn save(&self, key: &SecretKey) -> Result<(), String> {
        *self.0.lock().map_err(|e| e.to_string())? = Some(*key.as_bytes());
        Ok(())
    }
    fn load(&self, _: &str) -> Result<SecretKey, String> {
        let stored = *self.0.lock().map_err(|e| e.to_string())?;
        stored.ok_or_else(|| "没有保存的密钥".to_owned()).and_then(|b| SecretKey::from_slice(&b).map_err(|e| e.to_string()))
    }
    fn delete(&self) {
        if let Ok(mut stored) = self.0.lock() {
            *stored = None;
        }
    }
}

/// The platform's store.
pub fn platform() -> Box<dyn KeyStore> {
    Box::new(NoKeyStore)
}
```

- [ ] **Step 5: `mod.rs` service**

```rust
//! The password vault: everything secret lives under this module.

pub mod backup;
pub mod crypto;
pub mod entry;
pub mod error;
pub mod generator;
pub mod keychain;
pub mod store;
pub mod totp;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::db::Database;
pub use crypto::KdfParams;
use crypto::{SALT_LEN, Sealed, SecretKey, derive_key, open, random_bytes, seal, unwrap_key, wrap_key};
pub use entry::{EntrySummary, VaultEntry};
pub use error::VaultError;
use keychain::KeyStore;
use store::{EntryRow, MetaRow};

pub const MIN_PASSWORD_CHARS: usize = 10;
const FORMAT_VERSION: i64 = 1;
/// Failures before waiting starts.
const FREE_TRIES: u32 = 3;
const MAX_WAIT_MS: u64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VaultStatus {
    NotSetUp,
    Locked,
    Unlocked,
}

/// Which secret to copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Username,
    Password,
    Totp,
}

/// How long to wait after `failed_tries` wrong passwords.
pub fn backoff_ms(failed_tries: u32) -> u64 {
    if failed_tries < FREE_TRIES {
        return 0;
    }
    1_000_u64.checked_shl(failed_tries - FREE_TRIES).unwrap_or(MAX_WAIT_MS).min(MAX_WAIT_MS)
}

/// Unix time in ms.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f").to_string()
}

pub struct VaultService {
    db: Arc<Database>,
    keys: Box<dyn KeyStore>,
    params: KdfParams,
    /// The data key while unlocked.
    dek: Mutex<Option<SecretKey>>,
    last_activity_ms: AtomicU64,
}

impl VaultService {
    pub fn new(db: Arc<Database>, keys: Box<dyn KeyStore>) -> VaultService {
        VaultService::with_params(db, keys, KdfParams::DEFAULT)
    }

    pub fn with_params(db: Arc<Database>, keys: Box<dyn KeyStore>, params: KdfParams) -> VaultService {
        VaultService { db, keys, params, dek: Mutex::new(None), last_activity_ms: AtomicU64::new(0) }
    }

    fn key(&self) -> MutexGuard<'_, Option<SecretKey>> {
        self.dek.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn meta(&self) -> Result<Option<MetaRow>, VaultError> {
        Ok(self.db.with_conn(store::load_meta)?)
    }

    pub fn status(&self) -> VaultStatus {
        match self.meta() {
            Ok(None) => VaultStatus::NotSetUp,
            _ if self.key().is_some() => VaultStatus::Unlocked,
            _ => VaultStatus::Locked,
        }
    }

    /// Creates the vault with `password` and leaves it unlocked.
    pub fn setup(&self, password: &str) -> Result<(), VaultError> {
        if self.meta()?.is_some() {
            return Err(VaultError::AlreadySetUp);
        }
        check_strength(password)?;
        let dek = SecretKey::random()?;
        self.save_meta_for(password, &dek, 0, 0)?;
        self.open_session(dek, now_ms());
        Ok(())
    }

    fn save_meta_for(&self, password: &str, dek: &SecretKey, failed_tries: u32, locked_until_ms: u64) -> Result<(), VaultError> {
        let salt = random_bytes::<SALT_LEN>()?;
        let kek = derive_key(password, &salt, self.params)?;
        let meta = MetaRow { version: FORMAT_VERSION, params: self.params, salt: salt.to_vec(), wrapped_dek: wrap_key(&kek, dek)?, failed_tries, locked_until_ms };
        Ok(self.db.with_conn(|c| store::save_meta(c, &meta))?)
    }

    fn open_session(&self, dek: SecretKey, now: u64) {
        *self.key() = Some(dek);
        self.touch(now);
    }

    /// The data key `password` unwraps, without the backoff.
    fn check_password(&self, meta: &MetaRow, password: &str) -> Result<SecretKey, VaultError> {
        let kek = derive_key(password, &meta.salt, meta.params)?;
        unwrap_key(&kek, &meta.wrapped_dek).map_err(|_| VaultError::WrongPassword)
    }

    pub fn unlock(&self, password: &str, now: u64) -> Result<(), VaultError> {
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        if now < meta.locked_until_ms {
            return Err(VaultError::TooManyAttempts { wait_ms: meta.locked_until_ms - now });
        }
        match self.check_password(&meta, password) {
            Ok(dek) => {
                self.db.with_conn(|c| store::set_failures(c, 0, 0))?;
                self.open_session(dek, now);
                Ok(())
            }
            Err(VaultError::WrongPassword) => {
                let tries = meta.failed_tries.saturating_add(1);
                self.db.with_conn(|c| store::set_failures(c, tries, now + backoff_ms(tries)))?;
                Err(VaultError::WrongPassword)
            }
            Err(e) => Err(e),
        }
    }

    pub fn lock(&self) {
        *self.key() = None;
    }

    /// Records activity (resets the idle timer).
    pub fn touch(&self, now: u64) {
        self.last_activity_ms.store(now, Ordering::Relaxed);
    }

    /// Locks after `minutes` without activity; whether it locked now.
    pub fn lock_if_idle(&self, now: u64, minutes: i64) -> bool {
        let idle = now.saturating_sub(self.last_activity_ms.load(Ordering::Relaxed));
        let limit = u64::try_from(minutes.max(1)).unwrap_or(1) * 60_000;
        if idle < limit || self.key().is_none() {
            return false;
        }
        self.lock();
        true
    }

    fn with_key<R>(&self, f: impl FnOnce(&SecretKey) -> Result<R, VaultError>) -> Result<R, VaultError> {
        let key = self.key();
        let key = key.as_ref().ok_or(VaultError::Locked)?;
        self.touch(now_ms());
        f(key)
    }

    fn decrypt(key: &SecretKey, row: &EntryRow) -> Result<VaultEntry, VaultError> {
        let sealed = Sealed::from_parts(&row.nonce, &row.ciphertext).map_err(|_| VaultError::Corrupt)?;
        let plain = open(key, &sealed, row.id.as_bytes()).map_err(|_| VaultError::Corrupt)?;
        serde_json::from_slice(&plain).map_err(|_| VaultError::Corrupt)
    }

    fn encrypt(key: &SecretKey, id: &str, entry: &VaultEntry) -> Result<EntryRow, VaultError> {
        let json = zeroize::Zeroizing::new(serde_json::to_vec(entry).map_err(|_| VaultError::Crypto)?);
        let sealed = seal(key, &json, id.as_bytes())?;
        Ok(EntryRow { id: id.to_owned(), nonce: sealed.nonce.to_vec(), ciphertext: sealed.ciphertext, updated_at: entry.updated_at.clone() })
    }

    /// Every entry's summary, sorted by title.
    pub fn list(&self) -> Result<Vec<EntrySummary>, VaultError> {
        self.with_key(|key| {
            let rows = self.db.with_conn(store::load_entries)?;
            let mut list: Vec<EntrySummary> = rows
                .iter()
                .map(|row| Self::decrypt(key, row).map_or_else(|_| EntrySummary::corrupt(&row.id), |e| e.summary(&row.id)))
                .collect();
            list.sort_by_key(|s| s.title.to_lowercase());
            Ok(list)
        })
    }

    pub fn get(&self, id: &str) -> Result<VaultEntry, VaultError> {
        self.with_key(|key| {
            let row = self.db.with_conn(|c| store::load_entry(c, id))?.ok_or(VaultError::NotFound)?;
            Self::decrypt(key, &row)
        })
    }

    /// Saves `entry` as a new entry (`id` `None`) or over `id`; its id.
    pub fn save(&self, id: Option<&str>, mut entry: VaultEntry) -> Result<String, VaultError> {
        if !entry.totp.trim().is_empty() {
            totp::validate(&entry.totp).map_err(VaultError::InvalidTotp)?;
        }
        self.with_key(|key| {
            let id = id.map_or_else(|| uuid::Uuid::new_v4().to_string(), str::to_owned);
            let now = timestamp();
            if entry.created_at.is_empty() {
                entry.created_at = now.clone();
            }
            entry.updated_at = now;
            let row = Self::encrypt(key, &id, &entry)?;
            self.db.with_conn(|c| store::upsert_entry(c, &row))?;
            Ok(id)
        })
    }

    pub fn delete(&self, id: &str) -> Result<(), VaultError> {
        self.with_key(|_| Ok(self.db.with_conn(|c| store::delete_entry(c, id))?))
    }

    /// The text of `field` of entry `id`, for the clipboard.
    pub fn secret(&self, id: &str, field: Field) -> Result<zeroize::Zeroizing<String>, VaultError> {
        let entry = self.get(id)?;
        let text = match field {
            Field::Username => entry.username.clone(),
            Field::Password => entry.password.clone(),
            Field::Totp => totp::current(&entry.totp).map_err(VaultError::InvalidTotp)?.code,
        };
        Ok(zeroize::Zeroizing::new(text))
    }

    /// The current code of entry `id`'s TOTP.
    pub fn totp_code(&self, id: &str) -> Result<totp::TotpCode, VaultError> {
        totp::current(&self.get(id)?.totp).map_err(VaultError::InvalidTotp)
    }

    pub fn change_password(&self, old: &str, new: &str) -> Result<(), VaultError> {
        check_strength(new)?;
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        let dek = self.check_password(&meta, old)?;
        self.save_meta_for(new, &dek, 0, 0)?;
        self.open_session(dek, now_ms());
        Ok(())
    }

    /// Deletes the vault and its Touch ID key.
    pub fn reset(&self) -> Result<(), VaultError> {
        self.lock();
        self.keys.delete();
        Ok(self.db.with_conn(store::wipe)?)
    }
}

fn check_strength(password: &str) -> Result<(), VaultError> {
    if password.chars().count() < MIN_PASSWORD_CHARS {
        return Err(VaultError::WeakPassword);
    }
    Ok(())
}
```
Until Task 6 exists, create `src/vault/backup.rs` with just `//! Encrypted backups (Task 6).` so `pub mod backup;` compiles.

- [ ] **Step 6: Run** → `cargo test --lib vault::` all pass.
- [ ] **Step 7: Checkpoint.**

---

### Task 6: Encrypted backup

**Files:** `src/vault/backup.rs`; add `export`/`import` to `VaultService`.

- [ ] **Step 1: Failing tests** (in `src/vault/mod.rs` tests)

```rust
    #[test]
    fn export_import_merge_and_replace() {
        let a = service();
        a.setup("correct horse battery").unwrap();
        let id = a.save(None, entry("GitHub", "pw-1")).unwrap();
        let file = a.export("backup pass phrase").unwrap();
        assert!(!file.contains("pw-1") && !file.contains("GitHub"));

        let b = service();
        b.setup("another pass phrase").unwrap();
        b.save(None, entry("Local", "x")).unwrap();
        assert_eq!(b.import(&file, "wrong pass phrase", ImportMode::Merge).unwrap_err(), VaultError::WrongPassword);
        assert_eq!(b.import(&file, "backup pass phrase", ImportMode::Merge).unwrap(), 1);
        assert_eq!(b.list().unwrap().len(), 2);
        assert_eq!(b.get(&id).unwrap().password, "pw-1");
        assert_eq!(b.import(&file, "backup pass phrase", ImportMode::Replace).unwrap(), 1);
        assert_eq!(b.list().unwrap().len(), 1);
    }

    #[test]
    fn merge_keeps_the_newer_copy() {
        let a = service();
        a.setup("correct horse battery").unwrap();
        let id = a.save(None, entry("x", "old")).unwrap();
        let file = a.export("backup pass phrase").unwrap();
        let mut e = a.get(&id).unwrap();
        e.password = "new".into();
        a.save(Some(&id), e).unwrap();
        assert_eq!(a.import(&file, "backup pass phrase", ImportMode::Merge).unwrap(), 0);
        assert_eq!(a.get(&id).unwrap().password, "new");
    }

    #[test]
    fn bad_backups_are_rejected() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        assert!(matches!(v.import("{}", "x", ImportMode::Merge).unwrap_err(), VaultError::InvalidBackup(_)));
        let file = v.export("backup pass phrase").unwrap();
        let tampered = file.replacen("\"ciphertext\": \"", "\"ciphertext\": \"AAAA", 1);
        assert_ne!(tampered, file);
        assert!(v.import(&tampered, "backup pass phrase", ImportMode::Merge).is_err());
    }
```

- [ ] **Step 2: Run to fail.**

- [ ] **Step 3: `backup.rs`**

```rust
//! `.cxvault` files: every entry, encrypted with a key derived from a backup
//! password and a fresh salt.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::crypto::{KdfParams, SALT_LEN, Sealed, derive_key, open, random_bytes, seal};
use super::entry::VaultEntry;
use super::error::VaultError;

const FORMAT: &str = "clipboardx-vault";
const VERSION: u32 = 1;
const AAD: &[u8] = b"clipboardx/vault/backup/v1";

#[derive(Serialize, Deserialize)]
struct Kdf {
    alg: String,
    m: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
struct File {
    format: String,
    version: u32,
    kdf: Kdf,
    nonce: String,
    ciphertext: String,
}

/// One entry in a backup.
#[derive(Serialize, Deserialize)]
pub struct BackupEntry {
    pub id: String,
    pub updated_at: String,
    pub entry: VaultEntry,
}

pub fn encode(entries: &[BackupEntry], password: &str, params: KdfParams) -> Result<String, VaultError> {
    let salt = random_bytes::<SALT_LEN>()?;
    let key = derive_key(password, &salt, params)?;
    let json = Zeroizing::new(serde_json::to_vec(entries).map_err(|_| VaultError::Crypto)?);
    let sealed = seal(&key, &json, AAD)?;
    let file = File {
        format: FORMAT.into(),
        version: VERSION,
        kdf: Kdf { alg: "argon2id".into(), m: params.m, t: params.t, p: params.p, salt: B64.encode(salt) },
        nonce: B64.encode(sealed.nonce),
        ciphertext: B64.encode(&sealed.ciphertext),
    };
    serde_json::to_string_pretty(&file).map_err(|_| VaultError::Crypto)
}

pub fn decode(text: &str, password: &str) -> Result<Vec<BackupEntry>, VaultError> {
    let invalid = |why: &str| VaultError::InvalidBackup(why.to_owned());
    let file: File = serde_json::from_str(text).map_err(|_| invalid("不是有效的 ClipboardX 备份文件"))?;
    if file.format != FORMAT || file.kdf.alg != "argon2id" {
        return Err(invalid("不是有效的 ClipboardX 备份文件"));
    }
    if file.version != VERSION {
        return Err(invalid("不支持的备份版本"));
    }
    let b64 = |s: &str| B64.decode(s).map_err(|_| invalid("备份文件已损坏"));
    let params = KdfParams { m: file.kdf.m, t: file.kdf.t, p: file.kdf.p };
    let key = derive_key(password, &b64(&file.kdf.salt)?, params).map_err(|_| invalid("备份文件已损坏"))?;
    let sealed = Sealed::from_parts(&b64(&file.nonce)?, &b64(&file.ciphertext)?).map_err(|_| invalid("备份文件已损坏"))?;
    let plain = open(&key, &sealed, AAD).map_err(|_| VaultError::WrongPassword)?;
    serde_json::from_slice(&plain).map_err(|_| invalid("备份文件已损坏"))
}
```
Note: `File`'s fields are public data; a hostile file could set huge `m`. Before deriving, reject `m > 1_048_576` (1 GiB) or `t > 10` or `p > 8` with `invalid("备份文件参数异常")`.

- [ ] **Step 4: Service methods** (add to `impl VaultService` and `pub enum ImportMode { Merge, Replace }` near `Field`)

```rust
    /// Every entry, encrypted with `password`.
    pub fn export(&self, password: &str) -> Result<String, VaultError> {
        check_strength(password)?;
        let entries = self.with_key(|key| {
            let rows = self.db.with_conn(store::load_entries)?;
            rows.iter()
                .map(|row| Ok(backup::BackupEntry { id: row.id.clone(), updated_at: row.updated_at.clone(), entry: Self::decrypt(key, row)? }))
                .collect::<Result<Vec<_>, VaultError>>()
        })?;
        backup::encode(&entries, password, self.params)
    }

    /// Imports a backup; how many entries it added or changed.
    pub fn import(&self, text: &str, password: &str, mode: ImportMode) -> Result<usize, VaultError> {
        if self.key().is_none() {
            return Err(VaultError::Locked);
        }
        let entries = backup::decode(text, password)?;
        self.with_key(|key| {
            let existing = self.db.with_conn(store::load_entries)?;
            let rows: Vec<EntryRow> = entries
                .iter()
                .filter(|b| match mode {
                    ImportMode::Replace => true,
                    ImportMode::Merge => existing.iter().find(|r| r.id == b.id).is_none_or(|r| r.updated_at < b.updated_at),
                })
                .map(|b| Self::encrypt(key, &b.id, &b.entry))
                .collect::<Result<_, _>>()?;
            match mode {
                ImportMode::Replace => self.db.with_conn(|c| store::replace_entries(c, &rows))?,
                ImportMode::Merge => {
                    for row in &rows {
                        self.db.with_conn(|c| store::upsert_entry(c, row))?;
                    }
                }
            }
            Ok(rows.len())
        })
    }
```
`encrypt` uses `entry.updated_at` for the row, so imported entries keep their timestamps.

- [ ] **Step 5: Run** → pass. **Step 6: Checkpoint.**

---

### Task 7: Touch ID (macOS key store)

**Files:** `src/vault/keychain.rs` (add `MacKeyStore`); `VaultService` methods.

- [ ] **Step 1: Failing tests** (`src/vault/mod.rs`)

```rust
    #[test]
    fn touch_id_unlocks_and_is_dropped_on_reset() {
        let v = service();
        v.setup("correct horse battery").unwrap();
        assert_eq!(v.enable_touch_id("wrong password!").unwrap_err(), VaultError::WrongPassword);
        v.enable_touch_id("correct horse battery").unwrap();
        v.lock();
        v.unlock_with_touch_id("解锁密码本").unwrap();
        assert_eq!(v.status(), VaultStatus::Unlocked);
        v.disable_touch_id();
        v.lock();
        assert!(v.unlock_with_touch_id("解锁密码本").is_err());
    }
```

- [ ] **Step 2: Service methods**

```rust
    pub fn touch_id_available(&self) -> bool {
        self.keys.available()
    }

    /// Keeps a copy of the data key for Touch ID; needs the password.
    pub fn enable_touch_id(&self, password: &str) -> Result<(), VaultError> {
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        let dek = self.check_password(&meta, password)?;
        self.keys.save(&dek).map_err(VaultError::Keychain)
    }

    pub fn disable_touch_id(&self) {
        self.keys.delete();
    }

    /// Unlocks with the stored key after the system asks the user. Blocking.
    pub fn unlock_with_touch_id(&self, reason: &str) -> Result<(), VaultError> {
        let meta = self.meta()?.ok_or(VaultError::NotSetUp)?;
        let dek = self.keys.load(reason).map_err(VaultError::Keychain)?;
        // The stored key must still be this vault's: rewrap-check via the meta salt is impossible
        // without the password, so check it decrypts one entry when there is one.
        if let Some(row) = self.db.with_conn(store::load_entries)?.first() {
            Self::decrypt(&dek, row).map_err(|_| VaultError::Keychain("Touch ID 密钥已失效，请用主密码解锁".into()))?;
        }
        let _ = meta;
        self.open_session(dek, now_ms());
        Ok(())
    }
```
`change_password` keeps the same DEK, so the keychain copy stays valid; `reset` deletes it. Also make `setup` call `self.keys.delete()` first (a stale key from an older vault).

- [ ] **Step 3: `MacKeyStore`** (append to `keychain.rs`, replace `platform()`)

```rust
#[cfg(target_os = "macos")]
mod mac {
    use std::sync::mpsc;
    use std::time::Duration;

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};

    use super::super::crypto::SecretKey;
    use super::KeyStore;

    const SERVICE: &str = "com.clipboardx.app.vault";
    const ACCOUNT: &str = "data-key";
    const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);

    pub struct MacKeyStore;

    fn context() -> Retained<LAContext> {
        // SAFETY: plain allocation and init of LAContext.
        unsafe { LAContext::new() }
    }

    impl KeyStore for MacKeyStore {
        fn available(&self) -> bool {
            // SAFETY: a fresh context; the error out-parameter is optional.
            unsafe { context().canEvaluatePolicy_error(LAPolicy::DeviceOwnerAuthentication).is_ok() }
        }

        fn save(&self, key: &SecretKey) -> Result<(), String> {
            security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, key.as_bytes())
                .map_err(|e| format!("无法写入钥匙串：{e}"))
        }

        fn load(&self, reason: &str) -> Result<SecretKey, String> {
            let (tx, rx) = mpsc::channel::<bool>();
            let reply = RcBlock::new(move |ok: Bool, _error: *mut NSError| {
                let _ = tx.send(ok.as_bool());
            });
            let ctx = context();
            // SAFETY: the block is 'static and only sends on a channel (sendable).
            unsafe {
                ctx.evaluatePolicy_localizedReason_reply(LAPolicy::DeviceOwnerAuthentication, &NSString::from_str(reason), &reply);
            }
            match rx.recv_timeout(PROMPT_TIMEOUT) {
                Ok(true) => {}
                Ok(false) | Err(_) => return Err("已取消验证".into()),
            }
            let bytes = zeroize::Zeroizing::new(
                security_framework::passwords::get_generic_password(SERVICE, ACCOUNT).map_err(|_| "钥匙串中没有密钥".to_owned())?,
            );
            SecretKey::from_slice(&bytes).map_err(|e| e.to_string())
        }

        fn delete(&self) {
            let _ = security_framework::passwords::delete_generic_password(SERVICE, ACCOUNT);
        }
    }
}

/// The platform's store.
pub fn platform() -> Box<dyn KeyStore> {
    #[cfg(target_os = "macos")]
    return Box::new(mac::MacKeyStore);
    #[cfg(not(target_os = "macos"))]
    Box::new(NoKeyStore)
}
```
If `canEvaluatePolicy_error` has a different signature, adapt (it returns `Result<(), Retained<NSError>>` in objc2 0.6 generated code); if `LAContext::new` needs `MainThreadMarker`, it does not (LAContext is not main-thread-only). Requires `objc2-local-authentication` features `LAContext`, `block2`; `objc2-foundation` `NSError`, `NSString`.

- [ ] **Step 4: Run** `cargo test` (memory store) and `cargo build`. **Step 5: Checkpoint.**

---

### Task 8: Concealed copies and the clipboard monitor

**Files:** `src/capture.rs`, `src/macos.rs` (pasteboard check helper lives in `capture.rs` behind `cfg(macos)` using `objc2-app-kit`).

- [ ] **Step 1: Failing tests** (`src/capture.rs`)

```rust
    #[test]
    fn concealed_type_names_are_recognized() {
        assert!(is_concealed_types(&["public.utf8-plain-text", "org.nspasteboard.ConcealedType"]));
        assert!(is_concealed_types(&["org.nspasteboard.TransientType"]));
        assert!(!is_concealed_types(&["public.utf8-plain-text"]));
    }

    #[test]
    fn our_own_secret_copies_are_skipped() {
        remember_secret("s3cret");
        assert!(is_recent_secret(&hash_bytes(b"s3cret")));
        assert!(!is_recent_secret(&hash_bytes(b"other")));
    }
```

- [ ] **Step 2: Implement**

```rust
/// How long a copied secret stays on the clipboard.
pub const SECRET_LIFETIME: Duration = Duration::from_secs(30);

/// Pasteboard types other apps set on secrets (nspasteboard.org).
const SKIP_TYPES: [&str; 2] = ["org.nspasteboard.ConcealedType", "org.nspasteboard.TransientType"];

/// The hash of the secret copied last, which the monitor never records.
static LAST_SECRET: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub(crate) fn is_concealed_types<S: AsRef<str>>(types: &[S]) -> bool {
    types.iter().any(|t| SKIP_TYPES.contains(&t.as_ref()))
}

fn remember_secret(text: &str) {
    if let Ok(mut last) = LAST_SECRET.lock() {
        *last = Some(hash_bytes(text.as_bytes()));
    }
}

fn is_recent_secret(hash: &str) -> bool {
    LAST_SECRET.lock().is_ok_and(|last| last.as_deref() == Some(hash))
}

/// Whether the clipboard holds a secret another app marked (macOS).
#[cfg(target_os = "macos")]
fn clipboard_is_concealed() -> bool {
    use objc2_app_kit::NSPasteboard;
    let types = NSPasteboard::generalPasteboard().types();
    let names: Vec<String> = types.map(|t| t.iter().map(|s| s.to_string()).collect()).unwrap_or_default();
    is_concealed_types(&names)
}

#[cfg(not(target_os = "macos"))]
fn clipboard_is_concealed() -> bool {
    false
}

/// Copies `text` marked as a secret (left out of clipboard histories) and
/// clears it after [`SECRET_LIFETIME`] unless something else was copied.
pub fn copy_secret(text: &str) -> Result<(), String> {
    use arboard::SetExtApple as _; // on macOS; Windows/Linux have their own ext traits with the same method
    remember_secret(text);
    let mut clipboard = Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set().exclude_from_history().text(text.to_owned()).map_err(|e| e.to_string())?;
    let hash = hash_bytes(text.as_bytes());
    thread::Builder::new()
        .name("secret-clear".into())
        .spawn(move || {
            thread::sleep(SECRET_LIFETIME);
            if let Ok(mut clipboard) = Clipboard::new()
                && clipboard.get_text().is_ok_and(|t| hash_bytes(t.as_bytes()) == hash)
            {
                let _ = clipboard.clear();
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}
```
Use the right extension trait per platform: `#[cfg(target_os = "macos")] use arboard::SetExtApple;`, `#[cfg(target_os = "windows")] use arboard::SetExtWindows;`, `#[cfg(target_os = "linux")] use arboard::SetExtLinux;` at the top of `copy_secret`.

In `read()`: first line `if clipboard_is_concealed() { return None; }`. In `spawn_monitor` after computing `hash`: `if is_recent_secret(&hash) { last_hash = hash; continue; }`.

- [ ] **Step 3: Run** → pass. **Step 4: Checkpoint.**

---

### Task 9: Services wiring and host lock triggers

**Files:** `src/services.rs`, `src/macos.rs`.

- [ ] **Step 1: `services.rs`**
  - Field `vault: vault::VaultService`, built in `Services::new` with `VaultService::new(db.clone(), vault::keychain::platform())` (tests: `Services::new` uses `MemoryKeyStore` + `INSECURE_TEST` via a `#[cfg(test)]` constructor `Services::for_tests()` replacing the current test helper).
  - `pub fn vault(&self) -> &VaultService`.
  - `pub fn copy_vault_field(&self, id: &str, field: vault::Field) -> Result<(), String> { let text = self.vault.secret(id, field).map_err(|e| e.to_string())?; capture::copy_secret(&text) }`.
  - `pub fn window_hidden(&self)`: `if self.db.settings().is_ok_and(|s| s.vault_lock_on_hide) { self.vault.lock(); }`.
  - `pub fn lock_vault(&self) { self.vault.lock(); }`.
  - Test: `window_hidden` locks when the setting is on and not when off.
- [ ] **Step 2: `macos.rs`**
  - `Host::hide`: after `orderOut`, call `services().window_hidden()`.
  - In `run`, register NSWorkspace observers that call `services().lock_vault()`: `NSWorkspace::sharedWorkspace().notificationCenter()` for `NSWorkspaceWillSleepNotification`, `NSWorkspaceScreensDidSleepNotification`, `NSWorkspaceSessionDidResignActiveNotification`; and `NSDistributedNotificationCenter::defaultCenter()` for `"com.apple.screenIsLocked"`. Use `addObserverForName_object_queue_usingBlock` with an `RcBlock::new(|_: NonNull<NSNotification>| services().lock_vault())`; keep the returned observer tokens in `Host` (`observers: Vec<Retained<ProtocolObject<dyn NSObjectProtocol>>>`).
- [ ] **Step 3:** `cargo build`, `cargo test`. **Step 4: Checkpoint.**

---

### Task 10: UI — page, lock screen, state

**Files:** `src/ui/state.rs`, `src/ui/header.rs`, `src/ui/mod.rs`, create `src/ui/vault/mod.rs`, `src/ui/vault/lock.rs`.

- [ ] **Step 1: State** (`state.rs`)
  - `Page::Vault`.
  - Fields: `vault_status: Signal<VaultStatus>`, `vault_items: Signal<Vec<EntrySummary>>`, `vault_query: Signal<String>`, `vault_focused: Signal<usize>`, `vault_editing: Signal<Option<String>>` (entry id, `None` = new), `vault_busy: Signal<bool>`, `vault_tick: Signal<u64>` (unix secs, for TOTP rings).
  - `refresh_vault(self)`: status from `services().vault().status()`; when unlocked `vault_items.set(list)`, else `vault_items.set(vec![])` and close vault dialogs (`DialogKind::VaultEditor | VaultPassword | VaultConfirmDelete | VaultImport`).
  - In `watch_services` tick: `if services().vault().lock_if_idle(now_ms(), settings.vault_lock_minutes) {}`, then if the status differs from `vault_status` call `refresh_vault`; set `vault_tick` to the current second.
  - Actions: `vault_setup(pw, confirm)`, `vault_unlock(pw)`, `vault_unlock_touch_id()` (runs `blocking(...)` in `spawn`, sets `vault_busy`), `vault_lock()`, `vault_copy(id, Field)` → `services().copy_vault_field` → toast "已复制，30 秒后自动清除", `vault_save(id, entry)`, `vault_delete(id)`. Each catches `VaultError` → `self.error(e.to_string())` and calls `refresh_vault`.
  - `on_shown`: when on `Page::Vault` and Touch ID enabled and locked → `vault_unlock_touch_id()`.
- [ ] **Step 2: Header** — icon `Icon::Key` (add to `icons.rs`: `r#"<path d="m15.5 7.5 2.3 2.3a1 1 0 0 0 1.4 0l2.1-2.1a1 1 0 0 0 0-1.4L19 4"/><path d="m21 2-9.6 9.6"/><circle cx="7.5" cy="15.5" r="5.5"/>"#`) and `Icon::Lock` / `Icon::Eye` reuse; button "密码本" toggles `Page::Vault`; title suffix "· 密码本".
- [ ] **Step 3: `ui/vault/mod.rs`** — `vault_page(state)`: three `when`s on `vault_status`: `NotSetUp → lock::setup_view`, `Locked → lock::unlock_view`, `Unlocked → list::list_view` (Task 11).
- [ ] **Step 4: `ui/vault/lock.rs`**
  - `setup_view`: heading "创建密码本", text "主密码无法找回，请务必牢记。", two masked `field()`s (`test_id("vault-new")`, `test_id("vault-confirm")`), a strength line (`强度：弱/中/强` from length + classes), "创建" button → `vault_setup`. Mismatch → error toast "两次输入的密码不一致".
  - `unlock_view`: lock icon, masked field `test_id("vault-password")` with `on_submit` → `vault_unlock`, "解锁" button, and when Touch ID is enabled and available a "使用 Touch ID" button.
  - Both clear their draft signals after submit (`draft.set(String::new())`).
- [ ] **Step 5:** `cargo build`. **Step 6: Checkpoint.**

---

### Task 11: UI — list, editor, generator, TOTP

**Files:** create `src/ui/vault/list.rs`, `src/ui/vault/editor.rs`; `src/ui/keys.rs`; `src/ui/dialogs.rs`.

- [ ] **Step 1: `list.rs`**
  - Toolbar: search `field().test_id("vault-search")`, "新建" button (`vault_editing.set(None)`, `dialog = VaultEditor`), lock button (`Icon::Lock`, "锁定").
  - `virtual_list` over filtered `vault_items` (`summary.matches(query)`), keys by id. Row: title (semibold) / username (muted) / host (muted, small); right side: `icon_button(Icon::User?, "复制账号")` → `vault_copy(id, Username)`, `icon_button(Icon::Key, "复制密码")` → `Password`; when `has_totp`, a pill showing `services().vault().totp_code(id)` read in a closure that also reads `vault_tick` (so it re-computes each second) with `"{code} · {remaining}s"`, press → copy `Totp`. Corrupt rows show "已损坏" and only a delete button. Row press → open editor.
  - Empty: "还没有保存任何账号" / "无匹配内容".
- [ ] **Step 2: `editor.rs`** (`DialogKind::VaultEditor`)
  - Loads `services().vault().get(id)` once (untracked) into draft signals per field; new → empty.
  - Fields: 名称, 账号, 密码 (masked unless `reveal` signal; buttons 显示/隐藏 and 生成), 网址 (+ "打开" → `open_url`), 两步验证 (placeholder "otpauth:// 链接或 Base32 密钥"), 备注 (multiline).
  - Generator panel (toggle under the password): length `segmented` [12, 16, 20, 32], switches 大写/小写/数字/符号/排除易混淆; "生成" fills the password draft and sets `reveal` true.
  - Footer: 删除 (existing only → `DialogKind::VaultConfirmDelete`), 取消, 保存 → `vault_save`; on success close and toast "已保存".
  - On close, set every draft signal to `String::new()`.
- [ ] **Step 3: Keys** (`keys.rs`): when `page == Vault && unlocked && no dialog`: ↑/↓ move `vault_focused`, `enter` copies password, `mod+c` copies username (`When::Always` only when the search field is empty — check `vault_query`), `mod+n` new entry, `mod+l` lock. Existing bindings check `page == Main` already via `list_has_keys`.
- [ ] **Step 4: Dialogs** (`dialogs.rs`): add `VaultEditor`, `VaultConfirmDelete` ("删除后无法恢复。"), wired to the state.
- [ ] **Step 5:** `cargo build`. **Step 6: Checkpoint.**

---

### Task 12: UI — settings group and password dialogs

**Files:** `src/ui/settings.rs`, `src/ui/dialogs.rs`, `src/ui/state.rs`.

- [ ] **Step 1:** Settings section "密码本" (only rows that apply; show "尚未设置" text when `NotSetUp`):
  - 自动锁定: `segmented(VAULT_LOCK_OPTIONS)` → `db.set_vault_lock_minutes`.
  - 收起面板时锁定: switch → `db.set_vault_lock_on_hide`.
  - Touch ID 解锁: switch (hidden when `!touch_id_available()`); turning on opens `DialogKind::VaultPassword(Purpose::EnableTouchId)`; turning off → `disable_touch_id()` + `set_vault_touch_id(false)`.
  - 修改主密码 → `VaultPassword(Purpose::ChangePassword)` (old, new, confirm).
  - 导出备份 → `VaultPassword(Purpose::Export)` (backup password, default hint "可与主密码相同"), then `pick_save_path(SaveDialog::new().default_name("ClipboardX.cxvault"))`, write the file with `std::fs::write`, toast "已导出".
  - 导入备份 → `pick_paths(OpenDialog::files().filter(FileFilter::new("ClipboardX 备份", ["cxvault"])))`, read the file, then `VaultPassword(Purpose::Import)` with password + segmented 合并/替换; 替换 asks once more. Toast "已导入 N 条".
  - 重置密码本 (danger) → confirm dialog requiring the text "重置".
- [ ] **Step 2:** `DialogKind::VaultPassword` uses a `state.vault_purpose: Signal<Purpose>` and draft signals cleared on close. Export/import file paths kept in `state.vault_file: Signal<Option<PathBuf>>` / `vault_import_text: Signal<String>` (cleared on close).
- [ ] **Step 3:** `cargo build`. **Step 4: Checkpoint.**

---

### Task 13: Headless UI test, review, docs

**Files:** create `tests/vault_ui.rs`; update `README.md`.

- [ ] **Step 1: Test** (same setup as `tests/ui.rs`; the vault uses the platform key store, Touch ID is never touched)

```rust
#[test]
fn vault_setup_entries_lock_and_unlock() {
    let dir = tempfile::tempdir().unwrap();
    unsafe {
        std::env::set_var(DATA_DIR_ENV, dir.path());
        std::env::set_var(NO_CAPTURE_ENV, "1");
    }
    let mut app = start();
    app.press("label:密码本").unwrap();
    app.wait_for("创建密码本").unwrap();
    app.type_text("id:vault-new", "correct horse battery").unwrap();
    app.type_text("id:vault-confirm", "correct horse battery").unwrap();
    app.press("创建").unwrap();
    app.wait_for("还没有保存任何账号").unwrap();

    app.press("新建").unwrap();
    app.type_text("id:vault-title", "GitHub").unwrap();
    app.type_text("id:vault-username", "octo@example.com").unwrap();
    app.type_text("id:vault-password-field", "hunter2-secret").unwrap();
    app.press("保存").unwrap();
    app.wait_for("GitHub").unwrap();
    assert!(app.shows("octo@example.com"));
    assert!(!app.shows("hunter2-secret"));

    app.press("label:锁定").unwrap();
    app.wait_for("解锁").unwrap();
    assert!(!app.shows("GitHub"));
    app.type_text("id:vault-password", "wrong password").unwrap();
    app.press("解锁").unwrap();
    app.wait_for("密码错误").unwrap();
    app.clear("id:vault-password").unwrap();
    app.type_text("id:vault-password", "correct horse battery").unwrap();
    app.press("解锁").unwrap();
    app.wait_for("GitHub").unwrap();

    app.type_text("id:vault-search", "nothing-matches").unwrap();
    app.wait_for("无匹配内容").unwrap();
}
```
Because the test uses `KdfParams::DEFAULT` (64 MiB Argon2), each unlock takes ~0.3–1 s in debug; acceptable. If too slow, add `CLIPBOARDX_INSECURE_KDF` honored only under `cfg(debug_assertions)`.

- [ ] **Step 2:** Run all: `cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test`.
- [ ] **Step 3:** Dispatch a security-focused review (rust-reviewer / security-reviewer) of `src/vault/`, `capture.rs` changes, `macos.rs` changes; fix CRITICAL/HIGH.
- [ ] **Step 4:** README: a "密码本" section (features, security model, Touch ID limitation, backup format), keys table rows.
- [ ] **Step 5:** Manual checklist for the user (spec §8): Touch ID unlock, sleep/lock-screen lock, 1Password copy not recorded, clipboard cleared after 30 s.
- [ ] **Step 6: Checkpoint** — stop for the user's review and commit decision.
