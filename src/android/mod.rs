//! What only Java reaches on Android — the Keystore, the biometric prompt,
//! the clipboard's sensitive flag, the files directory — through
//! `com.clipboardx.Bridge` (`android/java`). The packaged app carries NSH's
//! classes only, so the bridge's dex (built by `build.rs`) is loaded here at
//! run time, into the VM that loaded us.

mod jvm;

use std::path::PathBuf;

use jni::objects::{JByteArray, JValue};
use zeroize::Zeroizing;

use crate::vault::device_store::{SealError, Sealer, SystemKey};

/// The Keystore keys, by what they seal.
const SILENT_KEY: &str = "clipboardx.silent";
const BIOMETRIC_KEY: &str = "clipboardx.biometric";
/// The bridge's messages for these two: anything else is a failure.
const CANCELED: &str = "canceled";
const INVALIDATED: &str = "invalidated";

/// The app's private files directory.
pub fn files_dir() -> Result<PathBuf, String> {
    jvm::call(|env, bridge| {
        let dir = env
            .call_static_method(bridge, "filesDir", "()Ljava/lang/String;", &[])?
            .l()?;
        let dir: String = env.get_string(&dir.into())?.into();
        Ok(PathBuf::from(dir))
    })
}

/// Copies `text` marked sensitive, as secret copy `id`; see [`clear_sensitive`].
pub fn copy_sensitive(text: &str, id: u64) -> Result<(), String> {
    jvm::call(|env, bridge| {
        let text = env.new_string(text)?;
        env.call_static_method(
            bridge,
            "copySensitive",
            "(Ljava/lang/String;J)V",
            &[JValue::Object(&text), JValue::Long(id as i64)],
        )?;
        Ok(())
    })
}

/// Clears the clipboard if it still holds secret copy `id`.
pub fn clear_sensitive(id: u64) -> Result<(), String> {
    jvm::call(|env, bridge| {
        env.call_static_method(bridge, "clearSensitive", "(J)V", &[JValue::Long(id as i64)])?;
        Ok(())
    })
}

/// The Keystore's AES-GCM keys, through the bridge.
pub struct KeystoreSealer;

fn alias(key: SystemKey) -> &'static str {
    match key {
        SystemKey::Silent => SILENT_KEY,
        SystemKey::Biometric => BIOMETRIC_KEY,
    }
}

fn seal_error(message: String) -> SealError {
    match message.as_str() {
        CANCELED => SealError::Canceled,
        INVALIDATED => SealError::Invalidated,
        _ => SealError::Failed(format!("系统密钥库出错：{message}")),
    }
}

/// Calls the bridge's `seal` or `open` on `input`: the bytes it returns,
/// with both Java arrays zeroed after.
fn crypt(
    method: &str,
    key: SystemKey,
    input: &[u8],
    prompt: &str,
) -> Result<Zeroizing<Vec<u8>>, SealError> {
    jvm::call(|env, bridge| {
        let alias = env.new_string(alias(key))?;
        let prompt = env.new_string(prompt)?;
        let input_array = env.byte_array_from_slice(input)?;
        let result = env.call_static_method(
            bridge,
            method,
            "(Ljava/lang/String;[BZLjava/lang/String;)[B",
            &[
                JValue::Object(&alias),
                JValue::Object(&input_array),
                JValue::Bool(u8::from(key == SystemKey::Biometric)),
                JValue::Object(&prompt),
            ],
        );
        jvm::zero(env, &input_array, input.len());
        let output = JByteArray::from(result?.l()?);
        let bytes = Zeroizing::new(env.convert_byte_array(&output)?);
        jvm::zero(env, &output, bytes.len());
        Ok(bytes)
    })
    .map_err(seal_error)
}

impl Sealer for KeystoreSealer {
    fn biometric_available(&self) -> bool {
        jvm::call(|env, bridge| {
            env.call_static_method(bridge, "biometricAvailable", "()Z", &[])?
                .z()
        })
        .inspect_err(|e| log::error!("cannot ask for biometrics: {e}"))
        .unwrap_or(false)
    }

    fn seal(&self, key: SystemKey, plain: &[u8], prompt: &str) -> Result<Vec<u8>, SealError> {
        crypt("seal", key, plain, prompt).map(|sealed| sealed.to_vec())
    }

    fn open(
        &self,
        key: SystemKey,
        sealed: &[u8],
        prompt: &str,
    ) -> Result<Zeroizing<Vec<u8>>, SealError> {
        crypt("open", key, sealed, prompt)
    }

    fn delete_key(&self, key: SystemKey) {
        let deleted = jvm::call(|env, bridge| {
            let alias = env.new_string(alias(key))?;
            env.call_static_method(
                bridge,
                "deleteKey",
                "(Ljava/lang/String;)V",
                &[JValue::Object(&alias)],
            )?;
            Ok(())
        });
        if let Err(e) = deleted {
            log::warn!("cannot delete the Keystore key {key:?}: {e}");
        }
    }
}
