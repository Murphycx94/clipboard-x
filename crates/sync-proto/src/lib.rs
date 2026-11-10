//! The sync server's API: request and response bodies, shared by the client
//! and the server. Everything vault-related in here is ciphertext or
//! non-secret parameters; the server never sees a key or plaintext.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// The API's path prefix.
pub const API: &str = "/v1";

/// Bytes, as standard Base64 in JSON.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Bytes(pub Vec<u8>);

impl std::fmt::Debug for Bytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Bytes({} bytes)", self.0.len())
    }
}

impl Serialize for Bytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        s.serialize_str(&base64::engine::general_purpose::STANDARD.encode(&self.0))
    }
}

impl<'de> Deserialize<'de> for Bytes {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let text = String::deserialize(d)?;
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .map(Bytes)
            .map_err(serde::de::Error::custom)
    }
}

/// `POST /bootstrap`: the first device, with the admin code the server printed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapRequest {
    pub admin_code: String,
    pub device_name: String,
}

/// `POST /join`: another device, with a pairing code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinRequest {
    pub code: String,
    pub device_name: String,
}

/// A device's credentials: what bootstrap and join answer.
#[derive(Clone, Serialize, Deserialize)]
pub struct DeviceToken {
    pub device_id: String,
    pub token: String,
}

impl std::fmt::Debug for DeviceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceToken")
            .field("device_id", &self.device_id)
            .finish_non_exhaustive()
    }
}

/// `POST /pair`: a one-time code for a new device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingCode {
    pub code: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// `GET /devices`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds; 0 when never seen.
    pub last_seen: i64,
}

/// The vault's header: what a device needs, with the master password and
/// the secret key, to unlock. No secret in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultHeader {
    /// Moves when the master password (and with it the data key) changes.
    pub version: u64,
    /// Key format (2: master password and secret key).
    pub format: i64,
    pub kdf_m: u32,
    pub kdf_t: u32,
    pub kdf_p: u32,
    pub salt: Bytes,
    pub wrapped_key: Bytes,
    pub key_check: Bytes,
    /// From the second version on: a digest of this header sealed with the
    /// previous version's data key, so devices holding that key can tell
    /// the device that changed the master password made it.
    #[serde(default)]
    pub auth: Option<Bytes>,
    /// The data key (and secret key) wrapped by the recovery phrase, when
    /// the vault has one.
    #[serde(default)]
    pub recovery_wrap: Option<Bytes>,
    /// The recovery phrase sealed under the data key, so a device changing
    /// the master password can wrap the new data key for the same phrase.
    #[serde(default)]
    pub recovery_sealed: Option<Bytes>,
    /// The digest of this header (version included) sealed with its own
    /// data key: whoever opens the header can tell it is whole and was
    /// made at that version, not an older one renumbered.
    #[serde(default)]
    pub seal: Option<Bytes>,
}

/// The sealed manifest: every record's id, revision and deletion, sealed
/// with the data key by a client. The server only keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u64,
    pub sealed: Bytes,
}

/// `GET /vault`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultState {
    pub header: Option<VaultHeader>,
    pub manifest: Option<Manifest>,
}

/// `PUT /vault`: either part, each replacing the one at `base_*` (0: none).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VaultUpdate {
    pub header: Option<VaultHeader>,
    pub base_header: u64,
    pub manifest: Option<Manifest>,
    pub base_manifest: u64,
}

/// A record going up: replaces the server's copy if it is at `base_revision`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordUp {
    pub id: String,
    pub base_revision: u64,
    pub revision: u64,
    pub deleted: bool,
    pub nonce: Bytes,
    pub ciphertext: Bytes,
}

/// `POST /changes`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushRequest {
    /// The header version the records are encrypted under.
    pub header_version: u64,
    pub records: Vec<RecordUp>,
}

/// `POST /changes`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushResponse {
    pub seq: u64,
    /// Records refused: the server's copy is not at their base revision.
    pub conflicts: Vec<String>,
}

/// A record coming down.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordDown {
    pub id: String,
    pub revision: u64,
    pub deleted: bool,
    pub nonce: Bytes,
    pub ciphertext: Bytes,
    pub seq: u64,
}

/// `GET /changes?since=n`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Changes {
    pub records: Vec<RecordDown>,
    /// The latest sequence number.
    pub seq: u64,
}

/// What `/events` pushes: a new sequence number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub seq: u64,
}

/// An error answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_are_base64_in_json() {
        let json = serde_json::to_string(&Bytes(vec![1, 2, 3])).unwrap();
        assert_eq!(json, "\"AQID\"");
        let back: Bytes = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Bytes(vec![1, 2, 3]));
        assert!(serde_json::from_str::<Bytes>("\"not base64!\"").is_err());
    }

    #[test]
    fn tokens_stay_out_of_debug() {
        let token = DeviceToken {
            device_id: "d".into(),
            token: "SECRET".into(),
        };
        assert!(!format!("{token:?}").contains("SECRET"));
    }
}
