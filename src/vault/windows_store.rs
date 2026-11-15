//! Windows' key store: each item a file in the data directory, sealed by
//! the system for this Windows account (DPAPI). The secret key and the
//! device token are read silently; the data key after Windows Hello
//! confirmed the user (a face, a fingerprint or the PIN), as the Mac's
//! after Touch ID.
//!
//! The seal is the account's, not the app's: another program of the same
//! user could open the files without asking. Windows Hello here is the
//! user's consent, not a lock on the key.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use windows::Security::Credentials::UI::{
    UserConsentVerificationResult, UserConsentVerifier, UserConsentVerifierAvailability,
};
use windows::Win32::Foundation::{HLOCAL, HWND, LocalFree};
use windows::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
use windows::core::{HSTRING, PCWSTR, factory};
use windows_future::IAsyncOperation;
use zeroize::{Zeroize, Zeroizing};

use super::keychain::{KeyItem, KeyStore};

/// Mixed into the seal, so nothing else the account sealed opens as ours.
const ENTROPY: &[u8] = b"com.clipboardx.app.vault/v1";
/// How long the Windows Hello prompt may stay up.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(120);
/// How long an answer about Windows Hello being set up is trusted: the UI
/// asks often, and the user may set it up while the app runs.
const AVAILABILITY_TTL: Duration = Duration::from_secs(10);

const CANCELED: &str = "已取消验证";

/// The window the Windows Hello prompt belongs to; 0 for none.
static PROMPT_OWNER: AtomicIsize = AtomicIsize::new(0);

/// Tells the store which window its prompts belong to (the host's panel),
/// so they open over it.
pub fn set_prompt_owner(window: isize) {
    PROMPT_OWNER.store(window, Ordering::Release);
}

/// Items in `dir`, one sealed file each.
pub struct WindowsKeyStore {
    dir: PathBuf,
}

impl WindowsKeyStore {
    pub fn new(dir: impl Into<PathBuf>) -> WindowsKeyStore {
        WindowsKeyStore { dir: dir.into() }
    }

    fn path(&self, item: KeyItem) -> PathBuf {
        self.dir.join(format!("{}.sealed", item.account()))
    }
}

/// Writes `bytes` next to `path` and moves them over it: a crash, or the
/// power going, leaves the old file or the new one. Flushed to the disk
/// before the move: the move alone is journaled, the bytes are not.
fn write_replacing(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let partial = path.with_extension("partial");
    let written = fs::File::create(&partial).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let moved = written.and_then(|()| fs::rename(&partial, path));
    if moved.is_err() {
        let _ = fs::remove_file(&partial);
    }
    moved
}

impl KeyStore for WindowsKeyStore {
    fn available(&self) -> bool {
        hello_available()
    }

    fn put(&self, item: KeyItem, bytes: &[u8]) -> Result<(), String> {
        let sealed = protect(bytes).map_err(|e| format!("系统数据保护出错：{e}"))?;
        fs::create_dir_all(&self.dir)
            .and_then(|()| write_replacing(&self.path(item), &sealed))
            .map_err(|e| format!("无法保存密钥：{e}"))
    }

    /// Any item, without asking the user. A copy of the data key the
    /// system no longer opens (the account's password was reset, the file
    /// came from elsewhere) is removed: it opens nothing and must not
    /// stay behind once quick unlocking is reported off. The secret key
    /// and the device token stay for the system to open another time.
    fn get(&self, item: KeyItem) -> Option<Zeroizing<Vec<u8>>> {
        let sealed = fs::read(self.path(item))
            .inspect_err(|e| {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::error!("cannot read {item:?}: {e}");
                }
            })
            .ok()?;
        unprotect(&sealed)
            .inspect_err(|e| {
                log::error!("cannot open {item:?}: {e}");
                if item == KeyItem::DataKey {
                    self.delete(item);
                }
            })
            .ok()
    }

    /// Whether its file is there, opened or not: a file that cannot be
    /// read right now is still an item.
    fn contains(&self, item: KeyItem) -> bool {
        self.path(item).exists()
    }

    fn get_with_auth(&self, item: KeyItem, reason: &str) -> Result<Zeroizing<Vec<u8>>, String> {
        confirm_user(reason)?;
        self.get(item).ok_or_else(|| "没有保存的密钥".to_owned())
    }

    fn delete(&self, item: KeyItem) {
        let path = self.path(item);
        if let Err(e) = fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            log::warn!("cannot delete {}: {e}", path.display());
        }
    }
}

fn blob(bytes: &[u8]) -> windows::core::Result<CRYPT_INTEGER_BLOB> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(bytes.len())
            .map_err(|_| windows::core::Error::from(windows::Win32::Foundation::E_INVALIDARG))?,
        pbData: bytes.as_ptr().cast_mut(),
    })
}

/// Copies out what the system allocated, wipes it and frees it.
///
/// # Safety
///
/// `out` is a buffer `CryptProtectData` or `CryptUnprotectData` returned,
/// not freed yet.
unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Zeroizing<Vec<u8>> {
    if out.pbData.is_null() {
        return Zeroizing::new(Vec::new());
    }
    // SAFETY: the system's buffer of `cbData` bytes, ours until freed.
    let buffer = unsafe { std::slice::from_raw_parts_mut(out.pbData, out.cbData as usize) };
    let bytes = Zeroizing::new(buffer.to_vec());
    buffer.zeroize();
    // SAFETY: allocated by the system with `LocalAlloc`, freed once.
    let _ = unsafe { LocalFree(Some(HLOCAL(out.pbData.cast()))) };
    bytes
}

/// `plain` sealed for this Windows account, without any prompt.
fn protect(plain: &[u8]) -> windows::core::Result<Vec<u8>> {
    let (input, entropy) = (blob(plain)?, blob(ENTROPY)?);
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: the blobs borrow slices that outlive the call and are only
    // read; `out` receives a buffer `take` frees.
    let sealed = unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )?;
        take(out)
    };
    Ok(sealed.to_vec())
}

/// What [`protect`] sealed.
fn unprotect(sealed: &[u8]) -> windows::core::Result<Zeroizing<Vec<u8>>> {
    let (input, entropy) = (blob(sealed)?, blob(ENTROPY)?);
    let mut out = CRYPT_INTEGER_BLOB::default();
    // SAFETY: as in `protect`.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )?;
        Ok(take(out))
    }
}

/// Whether Windows Hello can confirm the user now. Blocking, briefly.
fn ask_availability() -> bool {
    UserConsentVerifier::CheckAvailabilityAsync()
        .and_then(|asked| asked.join())
        .map(|answer| {
            answer == UserConsentVerifierAvailability::Available
                || answer == UserConsentVerifierAvailability::DeviceBusy
        })
        .unwrap_or_else(|e| {
            log::warn!("cannot ask about Windows Hello: {e}");
            false
        })
}

/// Whether Windows Hello is set up, from the last answer; an old answer is
/// asked again on a thread of its own, as the UI asks while it draws.
fn hello_available() -> bool {
    static ANSWER: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
    static ASKING: AtomicBool = AtomicBool::new(false);

    let known = ANSWER.lock().ok().and_then(|answer| *answer);
    let Some((at, available)) = known else {
        let available = ask_availability();
        if let Ok(mut answer) = ANSWER.lock() {
            *answer = Some((Instant::now(), available));
        }
        return available;
    };
    if at.elapsed() > AVAILABILITY_TTL && !ASKING.swap(true, Ordering::AcqRel) {
        let asked = std::thread::Builder::new()
            .name("windows-hello".into())
            .spawn(|| {
                let available = ask_availability();
                if let Ok(mut answer) = ANSWER.lock() {
                    *answer = Some((Instant::now(), available));
                }
                ASKING.store(false, Ordering::Release);
            });
        if asked.is_err() {
            ASKING.store(false, Ordering::Release);
        }
    }
    available
}

/// What the prompt's answer means for the caller.
fn verdict(result: UserConsentVerificationResult) -> Result<(), String> {
    match result {
        UserConsentVerificationResult::Verified => Ok(()),
        UserConsentVerificationResult::Canceled => Err(CANCELED.into()),
        UserConsentVerificationResult::DeviceNotPresent => Err("此设备不支持 Windows Hello".into()),
        UserConsentVerificationResult::NotConfiguredForUser => {
            Err("此设备尚未设置 Windows Hello".into())
        }
        UserConsentVerificationResult::DisabledByPolicy => {
            Err("Windows Hello 已被系统策略禁用".into())
        }
        UserConsentVerificationResult::DeviceBusy => Err("Windows Hello 正忙，请稍后再试".into()),
        UserConsentVerificationResult::RetriesExhausted => {
            Err("验证失败次数过多，请用主密码解锁".into())
        }
        _ => Err("Windows Hello 验证失败".into()),
    }
}

/// The Windows Hello prompt, over the host's window when there is one.
fn request_verification(
    reason: &HSTRING,
) -> windows::core::Result<IAsyncOperation<UserConsentVerificationResult>> {
    let owner = PROMPT_OWNER.load(Ordering::Acquire);
    if owner != 0
        && let Ok(interop) = factory::<UserConsentVerifier, IUserConsentVerifierInterop>()
    {
        // SAFETY: `owner` is the host's panel, alive while the app runs; a
        // window that is gone only fails the call.
        return unsafe { interop.RequestVerificationForWindowAsync(HWND(owner as *mut _), reason) };
    }
    UserConsentVerifier::RequestVerificationAsync(reason)
}

/// Asks Windows Hello to confirm the user (`reason` shown); blocking. Not
/// for the thread that runs the windows: it would wait for itself.
fn confirm_user(reason: &str) -> Result<(), String> {
    let failed = |e: windows::core::Error| format!("Windows Hello 验证出错：{e}");
    let prompt = request_verification(&HSTRING::from(reason)).map_err(failed)?;
    let (answered, answer) = mpsc::channel();
    prompt
        .when(move |result| {
            let _ = answered.send(result);
        })
        .map_err(failed)?;
    match answer.recv_timeout(PROMPT_TIMEOUT) {
        Ok(Ok(result)) => verdict(result),
        Ok(Err(e)) => Err(failed(e)),
        Err(_) => {
            // Nobody waits for its answer anymore.
            let _ = prompt.Cancel();
            Err(CANCELED.into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_bytes_open_to_what_was_sealed() {
        let sealed = protect(b"a data key").unwrap();

        assert_ne!(sealed.as_slice(), b"a data key");
        assert_eq!(unprotect(&sealed).unwrap().as_slice(), b"a data key");
    }

    #[test]
    fn altered_bytes_do_not_open() {
        let mut sealed = protect(b"a data key").unwrap();
        let last = sealed.len() - 1;
        sealed[last] ^= 1;

        assert!(unprotect(&sealed).is_err());
        assert!(unprotect(b"never sealed").is_err());
    }

    #[test]
    fn items_are_kept_apart_and_survive_a_new_store() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowsKeyStore::new(dir.path().join("keys"));
        store.put(KeyItem::SecretKey, b"sk").unwrap();
        store.put(KeyItem::DataKey, b"dk").unwrap();

        let reopened = WindowsKeyStore::new(dir.path().join("keys"));

        assert_eq!(reopened.get(KeyItem::SecretKey).unwrap().as_slice(), b"sk");
        assert_eq!(reopened.get(KeyItem::DataKey).unwrap().as_slice(), b"dk");
        assert!(reopened.get(KeyItem::DeviceToken).is_none());
        assert!(!reopened.contains(KeyItem::DeviceToken));
    }

    #[test]
    fn a_put_replaces_and_a_delete_removes() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowsKeyStore::new(dir.path());
        store.put(KeyItem::DeviceToken, b"old").unwrap();
        store.put(KeyItem::DeviceToken, b"new").unwrap();
        assert_eq!(store.get(KeyItem::DeviceToken).unwrap().as_slice(), b"new");

        store.delete(KeyItem::DeviceToken);
        store.delete(KeyItem::DeviceToken);

        assert!(store.get(KeyItem::DeviceToken).is_none());
    }

    #[test]
    fn a_secret_key_the_system_cannot_open_is_kept_for_another_time() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowsKeyStore::new(dir.path());
        fs::write(dir.path().join("secret-key.sealed"), b"garbage").unwrap();

        assert!(store.get(KeyItem::SecretKey).is_none());

        assert!(store.contains(KeyItem::SecretKey));
    }

    #[test]
    fn a_data_key_copy_the_system_cannot_open_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowsKeyStore::new(dir.path());
        fs::write(dir.path().join("data-key.sealed"), b"garbage").unwrap();

        assert!(store.get(KeyItem::DataKey).is_none());

        assert!(!store.contains(KeyItem::DataKey));
        assert!(!dir.path().join("data-key.sealed").exists());
    }

    #[test]
    fn replacing_an_item_leaves_no_partial_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = WindowsKeyStore::new(dir.path());
        store.put(KeyItem::SecretKey, b"old").unwrap();
        store.put(KeyItem::SecretKey, b"new").unwrap();

        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();

        assert_eq!(names, ["secret-key.sealed"]);
    }

    #[test]
    fn only_a_verified_answer_confirms_the_user() {
        assert!(verdict(UserConsentVerificationResult::Verified).is_ok());
        assert_eq!(
            verdict(UserConsentVerificationResult::Canceled).unwrap_err(),
            CANCELED
        );
        for refused in [
            UserConsentVerificationResult::DeviceNotPresent,
            UserConsentVerificationResult::NotConfiguredForUser,
            UserConsentVerificationResult::DisabledByPolicy,
            UserConsentVerificationResult::DeviceBusy,
            UserConsentVerificationResult::RetriesExhausted,
            UserConsentVerificationResult(99),
        ] {
            assert!(verdict(refused).is_err(), "{refused:?}");
        }
    }
}
