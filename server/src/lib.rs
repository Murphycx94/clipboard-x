//! ClipboardX's sync server. It keeps what clients upload (ciphertext, the
//! vault's header and its sealed manifest) and tells connected devices when
//! something changed. It holds no key and can decrypt nothing.

pub mod store;

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use clipboardx_sync_proto::{
    BootstrapRequest, Changes, Device, DeviceToken, ErrorBody, Event, JoinRequest, PairingCode,
    PushRequest, PushResponse, VaultState, VaultUpdate,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use store::{Refused, Store};

/// Uploads up to this size (a whole vault re-encrypted after a password
/// change goes up at once).
const BODY_LIMIT: usize = 32 * 1024 * 1024;
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const MAX_DEVICE_NAME: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// How long admin and pairing codes last.
    pub code_ttl: Duration,
    /// Wrong codes in a row before codes are refused for `cooldown`.
    pub max_failures: u32,
    /// How long codes are refused after too many wrong ones. Outstanding
    /// codes stay valid: a stranger's guesses must not void the user's.
    pub cooldown: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            code_ttl: Duration::from_secs(600),
            max_failures: 5,
            cooldown: Duration::from_secs(60),
        }
    }
}

struct Code {
    value: String,
    expires: Instant,
}

#[derive(Default)]
struct Codes {
    admin: Option<Code>,
    pairing: Vec<Code>,
    failures: u32,
    cooldown_until: Option<Instant>,
}

/// Pairing codes outstanding at once; a new one replaces the oldest.
const MAX_PAIRING_CODES: usize = 5;

pub struct AppState {
    store: Mutex<Store>,
    codes: Mutex<Codes>,
    events: broadcast::Sender<u64>,
    config: Config,
}

impl AppState {
    fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn codes(&self) -> MutexGuard<'_, Codes> {
        self.codes.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn notify(&self, seq: u64) {
        // No listener is fine.
        let _ = self.events.send(seq);
    }

    /// A one-time code, accepted once before `config.code_ttl`.
    fn new_code(&self) -> Code {
        Code {
            value: random_code(),
            expires: Instant::now() + self.config.code_ttl,
        }
    }

    /// Counts a wrong code; past the limit every outstanding code goes.
    /// Counts a wrong code; past the limit codes are refused for a while
    /// (40-bit codes, a few guesses a minute: never found in time).
    fn failed_code(&self, codes: &mut Codes) {
        codes.failures += 1;
        if codes.failures >= self.config.max_failures {
            log::warn!("too many wrong codes: refusing codes for a while");
            codes.cooldown_until = Some(Instant::now() + self.config.cooldown);
            codes.failures = 0;
        }
    }

    /// Refuses any code while cooling down.
    fn check_cooldown(&self, codes: &Codes) -> ApiResult<()> {
        match codes.cooldown_until {
            Some(until) if until > Instant::now() => Err(error(
                StatusCode::TOO_MANY_REQUESTS,
                "too many wrong codes, try again in a minute",
            )),
            _ => Ok(()),
        }
    }
}

/// The server's state on `store`, and the admin code to print when it has
/// no device yet.
pub fn new_state(
    store: Store,
    config: Config,
) -> rusqlite::Result<(Arc<AppState>, Option<String>)> {
    let (events, _) = broadcast::channel(64);
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        codes: Mutex::new(Codes::default()),
        events,
        config,
    });
    let admin = if state.store().active_devices()? == 0 {
        let code = state.new_code();
        let value = code.value.clone();
        state.codes().admin = Some(code);
        Some(value)
    } else {
        None
    };
    Ok((state, admin))
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/v1/bootstrap", post(bootstrap))
        .route("/v1/join", post(join))
        .route("/v1/pair", post(pair))
        .route("/v1/devices", get(devices))
        .route("/v1/devices/{id}", delete(revoke))
        .route("/v1/vault", get(get_vault).put(put_vault))
        .route("/v1/changes", get(changes).post(push))
        .route("/v1/events", get(events))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// Serves the API on `listener` until it fails.
pub async fn serve(listener: tokio::net::TcpListener, state: Arc<AppState>) -> std::io::Result<()> {
    axum::serve(listener, router(state)).await
}

// --- Helpers ---------------------------------------------------------------

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(ErrorBody { error: self.1 })).into_response()
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        log::error!("storage: {e}");
        ApiError(StatusCode::INTERNAL_SERVER_ERROR, "storage error".into())
    }
}

fn error(status: StatusCode, message: &str) -> ApiError {
    ApiError(status, message.to_owned())
}

type ApiResult<T> = Result<T, ApiError>;

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0_u8; N];
    getrandom::fill(&mut bytes).unwrap_or_else(|e| panic!("no system randomness: {e}"));
    bytes
}

/// `XXXX-XXXX` of an alphabet without look-alikes: 40 bits.
fn random_code() -> String {
    let chars: String = random_bytes::<8>()
        .iter()
        .map(|b| CODE_ALPHABET[usize::from(b % 32)] as char)
        .collect();
    format!("{}-{}", &chars[..4], &chars[4..])
}

fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// Compares without stopping at the first difference.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len()
        && a.bytes()
            .zip(b.bytes())
            .fold(0_u8, |acc, (x, y)| acc | (x ^ y))
            == 0
}

fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

fn device_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_DEVICE_NAME {
        return Err(error(StatusCode::BAD_REQUEST, "invalid device name"));
    }
    Ok(name.to_owned())
}

/// Registers a device; its token, which only its hash outlives.
fn register(state: &AppState, name: &str) -> ApiResult<DeviceToken> {
    let id = hex::encode(random_bytes::<16>());
    let token = hex::encode(random_bytes::<32>());
    state
        .store()
        .add_device(&id, name, &hash_token(&token), now())?;
    log::info!("device {id} ({name}) registered");
    Ok(DeviceToken {
        device_id: id,
        token,
    })
}

/// A request from a registered device.
struct Authed {
    device_id: String,
}

impl FromRequestParts<Arc<AppState>> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &Arc<AppState>) -> ApiResult<Self> {
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "missing token"))?;
        let device_id = state
            .store()
            .device_by_token(&hash_token(token), now())?
            .ok_or_else(|| error(StatusCode::UNAUTHORIZED, "invalid token"))?;
        Ok(Authed { device_id })
    }
}

// --- Devices ---------------------------------------------------------------

async fn bootstrap(
    State(state): State<Arc<AppState>>,
    Json(request): Json<BootstrapRequest>,
) -> ApiResult<Json<DeviceToken>> {
    let name = device_name(&request.device_name)?;
    if state.store().active_devices()? > 0 {
        return Err(error(StatusCode::CONFLICT, "already set up"));
    }
    {
        let mut codes = state.codes();
        state.check_cooldown(&codes)?;
        let given = normalize_code(&request.admin_code);
        let valid = codes.admin.as_ref().is_some_and(|code| {
            code.expires > Instant::now() && same(&normalize_code(&code.value), &given)
        });
        if !valid {
            state.failed_code(&mut codes);
            return Err(error(
                StatusCode::UNAUTHORIZED,
                "invalid or expired admin code",
            ));
        }
        codes.admin = None;
        codes.failures = 0;
    }
    Ok(Json(register(&state, &name)?))
}

async fn pair(State(state): State<Arc<AppState>>, authed: Authed) -> ApiResult<Json<PairingCode>> {
    let answer = issue_pairing_code(&state);
    log::info!("device {} made a pairing code", authed.device_id);
    Ok(Json(answer))
}

/// A new pairing code, as a device asks for one. The server's operator can
/// ask too (a signal to the process): with every device lost, it is how a
/// new one joins and recovers the vault with its recovery phrase. It gives
/// no more than the server's files do: ciphertext.
pub fn issue_pairing_code(state: &AppState) -> PairingCode {
    let code = state.new_code();
    let answer = PairingCode {
        code: code.value.clone(),
        expires_at: now() + i64::try_from(state.config.code_ttl.as_secs()).unwrap_or(0),
    };
    let mut codes = state.codes();
    codes.pairing.retain(|c| c.expires > Instant::now());
    if codes.pairing.len() >= MAX_PAIRING_CODES {
        codes.pairing.remove(0);
    }
    codes.pairing.push(code);
    answer
}

async fn join(
    State(state): State<Arc<AppState>>,
    Json(request): Json<JoinRequest>,
) -> ApiResult<Json<DeviceToken>> {
    let name = device_name(&request.device_name)?;
    {
        let mut codes = state.codes();
        state.check_cooldown(&codes)?;
        let given = normalize_code(&request.code);
        let now = Instant::now();
        let found = codes
            .pairing
            .iter()
            .position(|code| code.expires > now && same(&normalize_code(&code.value), &given));
        match found {
            Some(at) => {
                codes.pairing.remove(at);
                codes.failures = 0;
            }
            None => {
                state.failed_code(&mut codes);
                return Err(error(
                    StatusCode::UNAUTHORIZED,
                    "invalid or expired pairing code",
                ));
            }
        }
    }
    Ok(Json(register(&state, &name)?))
}

async fn devices(State(state): State<Arc<AppState>>, _: Authed) -> ApiResult<Json<Vec<Device>>> {
    Ok(Json(state.store().devices()?))
}

async fn revoke(
    State(state): State<Arc<AppState>>,
    authed: Authed,
    Path(id): Path<String>,
) -> ApiResult<StatusCode> {
    if !state.store().revoke(&id)? {
        return Err(error(StatusCode::NOT_FOUND, "no such device"));
    }
    log::info!("device {} revoked {id}", authed.device_id);
    Ok(StatusCode::NO_CONTENT)
}

// --- The vault -------------------------------------------------------------

async fn get_vault(State(state): State<Arc<AppState>>, _: Authed) -> ApiResult<Json<VaultState>> {
    Ok(Json(state.store().vault()?))
}

async fn put_vault(
    State(state): State<Arc<AppState>>,
    _: Authed,
    Json(update): Json<VaultUpdate>,
) -> ApiResult<StatusCode> {
    let result = {
        let mut store = state.store();
        let result = store.update_vault(
            update.header.as_ref().map(|h| (h, update.base_header)),
            update.manifest.as_ref().map(|m| (m, update.base_manifest)),
        )?;
        result.map(|()| store.seq())
    };
    match result {
        Ok(seq) => {
            state.notify(seq?);
            Ok(StatusCode::NO_CONTENT)
        }
        Err(Refused::Stale) => Err(error(StatusCode::CONFLICT, "stale base version")),
        Err(Refused::NotNewer) => Err(error(StatusCode::CONFLICT, "version does not move forward")),
    }
}

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    since: u64,
}

async fn changes(
    State(state): State<Arc<AppState>>,
    _: Authed,
    Query(Since { since }): Query<Since>,
) -> ApiResult<Json<Changes>> {
    let store = state.store();
    Ok(Json(Changes {
        records: store.changes(since)?,
        seq: store.seq()?,
    }))
}

async fn push(
    State(state): State<Arc<AppState>>,
    _: Authed,
    Json(request): Json<PushRequest>,
) -> ApiResult<Json<PushResponse>> {
    let (seq, conflicts) = {
        let mut store = state.store();
        // Records sealed under an older key would be unreadable to devices
        // with the new one.
        if request.header_version != store.header_version()? {
            return Err(error(StatusCode::CONFLICT, "header changed"));
        }
        store.push(&request.records)?
    };
    if request.records.len() > conflicts.len() {
        state.notify(seq);
    }
    Ok(Json(PushResponse { seq, conflicts }))
}

// --- Events ----------------------------------------------------------------

async fn events(
    State(state): State<Arc<AppState>>,
    _: Authed,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| stream_events(state, socket))
}

/// Sends the current sequence number, then every new one, until the client
/// goes.
async fn stream_events(state: Arc<AppState>, mut socket: WebSocket) {
    let mut events = state.events.subscribe();
    let current = state.store().seq().unwrap_or(0);
    let send = |seq: u64| {
        Message::Text(
            serde_json::to_string(&Event { seq })
                .unwrap_or_default()
                .into(),
        )
    };
    if socket.send(send(current)).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(seq) => {
                    if socket.send(send(seq)).await.is_err() {
                        return;
                    }
                }
                // Missed some: the latest is what matters.
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let seq = state.store().seq().unwrap_or(0);
                    if socket.send(send(seq)).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => return,
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_look_right_and_compare_loosely() {
        let code = random_code();
        assert_eq!(code.len(), 9);
        assert_eq!(&code[4..5], "-");
        assert!(same(
            &normalize_code(&code),
            &normalize_code(&code.to_lowercase().replace('-', " "))
        ));
        assert!(!same("ABCD", "ABCE"));
        assert!(!same("ABCD", "ABC"));
    }
}
