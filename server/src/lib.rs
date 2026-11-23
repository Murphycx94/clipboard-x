//! ClipboardX's sync server. It keeps what clients upload (ciphertext, the
//! vault's header and its sealed manifest) and tells connected devices when
//! something changed. It holds no key and can decrypt nothing.

pub mod store;

use std::collections::HashMap;
use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::connect_info::IntoMakeServiceWithConnectInfo;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, DefaultBodyLimit, FromRequestParts, Path, Query, State};
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
/// What a request with a code may weigh (a code and a device's name): read
/// before the code is checked, so anyone can send it.
const CODE_BODY_LIMIT: usize = 4 * 1024;
/// What the proxy in front of the server says of who it speaks for.
const FORWARDED_FOR: &str = "x-forwarded-for";
/// Clients whose wrong codes are counted apart at once.
const MAX_GUESSING_CLIENTS: usize = 4096;
/// Whom the clients past [`MAX_GUESSING_CLIENTS`] are counted as, together.
const EVERYONE_ELSE: IpAddr = IpAddr::V4(Ipv4Addr::UNSPECIFIED);
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const MAX_DEVICE_NAME: usize = 64;

#[derive(Debug, Clone, Copy)]
pub struct Config {
    /// How long admin and pairing codes last.
    pub code_ttl: Duration,
    /// Wrong codes in a row from one client before its codes are refused
    /// for `cooldown`.
    pub max_failures: u32,
    /// How long a client's codes are refused after too many wrong ones.
    /// Outstanding codes stay valid, and other clients' are still taken: a
    /// stranger's guesses must neither void the user's code nor keep the
    /// user out.
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
    /// The device that asked for it, of a pairing code: the one it lets in
    /// is listed as added by that device, and the code goes when that
    /// device is revoked. None: the server's operator's.
    issued_by: Option<String>,
}

/// One client's wrong codes.
struct Guesses {
    failures: u32,
    cooldown_until: Option<Instant>,
    /// When to forget them: a cooldown after the last one.
    forgotten_at: Instant,
}

#[derive(Default)]
struct Codes {
    admin: Option<Code>,
    pairing: Vec<Code>,
    guesses: HashMap<IpAddr, Guesses>,
}

impl Codes {
    /// Whom `client`'s wrong codes count as: itself, or everyone else
    /// while too many clients guess for each to be told apart.
    fn guesser(&self, client: IpAddr) -> IpAddr {
        if self.guesses.contains_key(&client) || self.guesses.len() < MAX_GUESSING_CLIENTS {
            client
        } else {
            EVERYONE_ELSE
        }
    }
}

/// Pairing codes outstanding at once; a new one replaces the oldest.
const MAX_PAIRING_CODES: usize = 5;

pub struct AppState {
    store: Mutex<Store>,
    codes: Mutex<Codes>,
    events: broadcast::Sender<u64>,
    /// The ids of devices as they are revoked: their event streams end.
    revoked: broadcast::Sender<String>,
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
    fn new_code(&self, issued_by: Option<String>) -> Code {
        Code {
            value: random_code(),
            expires: Instant::now() + self.config.code_ttl,
            issued_by,
        }
    }

    /// Counts a wrong code from `client`; past the limit its codes are
    /// refused for a while (40-bit codes, a few guesses a minute from each
    /// address: never found in time).
    fn failed_code(&self, codes: &mut Codes, client: IpAddr) {
        let now = Instant::now();
        if codes.guesses.len() >= MAX_GUESSING_CLIENTS {
            // Room for who guesses now.
            codes
                .guesses
                .retain(|_, guesses| guesses.forgotten_at > now);
        }
        let client = codes.guesser(client);
        let guesses = codes.guesses.entry(client).or_insert(Guesses {
            failures: 0,
            cooldown_until: None,
            forgotten_at: now,
        });
        if guesses.forgotten_at <= now {
            guesses.failures = 0;
        }
        guesses.failures += 1;
        guesses.forgotten_at = now + self.config.cooldown;
        if guesses.failures >= self.config.max_failures {
            log::warn!("too many wrong codes from {client}: refusing its codes for a while");
            guesses.cooldown_until = Some(now + self.config.cooldown);
            guesses.failures = 0;
        }
    }

    /// Refuses any code from `client` while it cools down.
    fn check_cooldown(&self, codes: &Codes, client: IpAddr) -> ApiResult<()> {
        let until = codes
            .guesses
            .get(&codes.guesser(client))
            .and_then(|g| g.cooldown_until);
        match until {
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
    let (revoked, _) = broadcast::channel(16);
    let state = Arc::new(AppState {
        store: Mutex::new(store),
        codes: Mutex::new(Codes::default()),
        events,
        revoked,
        config,
    });
    let admin = if state.store().active_devices()? == 0 {
        let code = state.new_code(None);
        let value = code.value.clone();
        state.codes().admin = Some(code);
        Some(value)
    } else {
        None
    };
    Ok((state, admin))
}

pub fn router(state: Arc<AppState>) -> Router {
    // Inside the router's own limit, the nearer one counts.
    let code_body = || DefaultBodyLimit::max(CODE_BODY_LIMIT);
    Router::new()
        .route("/v1/bootstrap", post(bootstrap).layer(code_body()))
        .route("/v1/join", post(join).layer(code_body()))
        .route("/v1/pair", post(pair))
        .route("/v1/devices", get(devices))
        .route("/v1/devices/{id}", delete(revoke))
        .route("/v1/vault", get(get_vault).put(put_vault))
        .route("/v1/changes", get(changes).post(push))
        .route("/v1/events", get(events))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(state)
}

/// The API as a service that knows each connection's peer, which telling
/// clients apart needs (see [`Client`]).
pub fn service(state: Arc<AppState>) -> IntoMakeServiceWithConnectInfo<Router, SocketAddr> {
    router(state).into_make_service_with_connect_info::<SocketAddr>()
}

/// Serves the API on `listener` until it fails.
pub async fn serve(listener: tokio::net::TcpListener, state: Arc<AppState>) -> std::io::Result<()> {
    axum::serve(listener, service(state)).await
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

/// Whether `c` is kept out of a device's name: a control character or a
/// line or paragraph separator (a line break would let a name write lines
/// of its own in the server's log), or one that shows what follows in
/// another order than it is written (a name tells devices apart in a list).
fn breaks_a_name(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{2028}' | '\u{2029}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A device's name as it is kept: one plain line of text.
fn device_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    let plain = !name.is_empty()
        && name.chars().count() <= MAX_DEVICE_NAME
        && !name.chars().any(breaks_a_name);
    if !plain {
        return Err(error(StatusCode::BAD_REQUEST, "invalid device name"));
    }
    Ok(name.to_owned())
}

/// Registers a device, added by the device `added_by` when one let it in;
/// its token, which only its hash outlives.
fn register(state: &AppState, name: &str, added_by: Option<&str>) -> ApiResult<DeviceToken> {
    let id = hex::encode(random_bytes::<16>());
    let token = hex::encode(random_bytes::<32>());
    let added = state
        .store()
        .add_device(&id, name, &hash_token(&token), now(), added_by)?;
    if !added {
        // The device whose pairing code this was has been revoked since.
        return Err(error(
            StatusCode::UNAUTHORIZED,
            "invalid or expired pairing code",
        ));
    }
    match added_by {
        Some(adder) => log::info!("device {id} ({name}) registered, added by {adder}"),
        None => log::info!("device {id} ({name}) registered"),
    }
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

/// Who a request comes from, to count wrong codes by.
struct Client(IpAddr);

impl<S: Sync> FromRequestParts<S> for Client {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Infallible> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0.ip());
        // The last line, should there be several: the proxy's own.
        let forwarded = parts
            .headers
            .get_all(FORWARDED_FOR)
            .iter()
            .next_back()
            .and_then(|value| value.to_str().ok());
        Ok(Client(client_address(peer, forwarded)))
    }
}

/// The address a request comes from: its peer's, or, when the peer is the
/// proxy on this machine, the one the proxy saw (the last it listed: the
/// ones before come from the request and prove nothing). Without a peer
/// (served without [`service`]) every request is one client.
fn client_address(peer: Option<IpAddr>, forwarded: Option<&str>) -> IpAddr {
    let peer = peer.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    let seen_by_proxy = forwarded
        // ::ffff:127.0.0.1 too, as a listener on both families sees it.
        .filter(|_| peer.to_canonical().is_loopback())
        .and_then(|list| list.rsplit(',').next())
        .and_then(|address| address.trim().parse::<IpAddr>().ok());
    network(seen_by_proxy.unwrap_or(peer))
}

/// An IPv6 address's /56: a subscriber holds that much or a part of it,
/// and would otherwise be as many clients as it likes.
fn network(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => {
                let [a, b, c, d, ..] = v6.segments();
                IpAddr::V6(Ipv6Addr::new(a, b, c, d & 0xff00, 0, 0, 0, 0))
            }
        },
        v4 @ IpAddr::V4(_) => v4,
    }
}

// --- Devices ---------------------------------------------------------------

async fn bootstrap(
    State(state): State<Arc<AppState>>,
    Client(client): Client,
    Json(request): Json<BootstrapRequest>,
) -> ApiResult<Json<DeviceToken>> {
    let name = device_name(&request.device_name)?;
    if state.store().active_devices()? > 0 {
        return Err(error(StatusCode::CONFLICT, "already set up"));
    }
    {
        let mut codes = state.codes();
        state.check_cooldown(&codes, client)?;
        let given = normalize_code(&request.admin_code);
        let valid = codes.admin.as_ref().is_some_and(|code| {
            code.expires > Instant::now() && same(&normalize_code(&code.value), &given)
        });
        if !valid {
            state.failed_code(&mut codes, client);
            return Err(error(
                StatusCode::UNAUTHORIZED,
                "invalid or expired admin code",
            ));
        }
        codes.admin = None;
        codes.guesses.remove(&client);
    }
    Ok(Json(register(&state, &name, None)?))
}

async fn pair(State(state): State<Arc<AppState>>, authed: Authed) -> ApiResult<Json<PairingCode>> {
    let answer = pairing_code(&state, Some(authed.device_id.clone()));
    log::info!("device {} made a pairing code", authed.device_id);
    Ok(Json(answer))
}

/// A new pairing code for the server's operator (a signal to the process):
/// with every device lost, it is how a new one joins and recovers the vault
/// with its recovery phrase. It gives no more than the server's files do:
/// ciphertext.
pub fn issue_pairing_code(state: &AppState) -> PairingCode {
    pairing_code(state, None)
}

/// A new pairing code, asked for by device `issued_by` or by the operator.
fn pairing_code(state: &AppState, issued_by: Option<String>) -> PairingCode {
    let code = state.new_code(issued_by);
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
    Client(client): Client,
    Json(request): Json<JoinRequest>,
) -> ApiResult<Json<DeviceToken>> {
    let name = device_name(&request.device_name)?;
    let added_by = {
        let mut codes = state.codes();
        state.check_cooldown(&codes, client)?;
        let given = normalize_code(&request.code);
        let now = Instant::now();
        let found = codes
            .pairing
            .iter()
            .position(|code| code.expires > now && same(&normalize_code(&code.value), &given));
        match found {
            Some(at) => {
                codes.guesses.remove(&client);
                codes.pairing.remove(at).issued_by
            }
            None => {
                state.failed_code(&mut codes, client);
                return Err(error(
                    StatusCode::UNAUTHORIZED,
                    "invalid or expired pairing code",
                ));
            }
        }
    };
    Ok(Json(register(&state, &name, added_by.as_deref())?))
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
    // What it could still come back with goes too: the pairing codes it
    // asked for, and the streams it listens on.
    let voided = {
        let mut codes = state.codes();
        let before = codes.pairing.len();
        codes
            .pairing
            .retain(|code| code.issued_by.as_deref() != Some(id.as_str()));
        before - codes.pairing.len()
    };
    // No listener is fine.
    let _ = state.revoked.send(id.clone());
    log::info!(
        "device {} revoked {id} ({voided} of its pairing codes voided)",
        authed.device_id
    );
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
        Err(Refused::TooFar) => Err(error(StatusCode::CONFLICT, "version too far ahead")),
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
    authed: Authed,
    upgrade: WebSocketUpgrade,
) -> Response {
    upgrade.on_upgrade(move |socket| stream_events(state, socket, authed.device_id))
}

/// Sends device `device_id` the current sequence number, then every new
/// one, until the client goes or the device is revoked.
async fn stream_events(state: Arc<AppState>, mut socket: WebSocket, device_id: String) {
    let mut events = state.events.subscribe();
    let mut revoked = state.revoked.subscribe();
    // Unknown (the storage fails) is not active: the client asks again.
    let active = |state: &AppState| state.store().is_active(&device_id).unwrap_or(false);
    // Revoked since its token was checked, before this listened for it.
    if !active(&state) {
        return;
    }
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
            // In this order: a revoked device hears nothing more.
            biased;
            gone = revoked.recv() => {
                let this_device = match gone {
                    Ok(id) => id == device_id,
                    // Missed some: it may have been among them.
                    Err(broadcast::error::RecvError::Lagged(_)) => !active(&state),
                    Err(broadcast::error::RecvError::Closed) => true,
                };
                if this_device {
                    let _ = socket.send(Message::Close(None)).await;
                    return;
                }
            },
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

    fn address(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn the_proxy_on_this_machine_says_who_it_speaks_for() {
        let proxy = Some(address("127.0.0.1"));
        assert_eq!(
            client_address(proxy, Some("203.0.113.9")),
            address("203.0.113.9")
        );
        // What the request itself claimed comes first, and counts for
        // nothing.
        assert_eq!(
            client_address(proxy, Some("198.51.100.7, 203.0.113.9")),
            address("203.0.113.9")
        );
        assert_eq!(client_address(proxy, None), address("127.0.0.1"));
        assert_eq!(
            client_address(proxy, Some("not an address")),
            address("127.0.0.1")
        );
        for local in ["::1", "::ffff:127.0.0.1"] {
            assert_eq!(
                client_address(Some(address(local)), Some("203.0.113.9")),
                address("203.0.113.9")
            );
        }
    }

    #[test]
    fn a_peer_elsewhere_is_not_believed_about_others() {
        assert_eq!(
            client_address(Some(address("192.0.2.1")), Some("203.0.113.9")),
            address("192.0.2.1")
        );
        assert_eq!(
            client_address(None, Some("203.0.113.9")),
            address("0.0.0.0")
        );
    }

    fn state(config: Config) -> Arc<AppState> {
        new_state(Store::in_memory().unwrap(), config).unwrap().0
    }

    fn numbered(n: u32) -> IpAddr {
        IpAddr::V4(Ipv4Addr::from(0x0b00_0000 + n))
    }

    #[test]
    fn old_wrong_codes_are_forgotten() {
        let state = state(Config {
            max_failures: 2,
            cooldown: Duration::from_millis(40),
            ..Config::default()
        });
        let client = numbered(1);
        let mut codes = state.codes();
        state.failed_code(&mut codes, client);
        std::thread::sleep(Duration::from_millis(60));
        state.failed_code(&mut codes, client);
        assert!(state.check_cooldown(&codes, client).is_ok());
        state.failed_code(&mut codes, client);
        assert!(state.check_cooldown(&codes, client).is_err());
    }

    #[test]
    fn guessing_clients_past_the_limit_count_as_one() {
        let state = state(Config::default());
        let mut codes = state.codes();
        let crowd = u32::try_from(MAX_GUESSING_CLIENTS).unwrap() + 50;
        for n in 0..crowd {
            state.failed_code(&mut codes, numbered(n));
        }
        assert_eq!(codes.guesses.len(), MAX_GUESSING_CLIENTS + 1);
        // The latecomers' wrong codes added up, and cool every newcomer.
        assert!(state.check_cooldown(&codes, numbered(crowd)).is_err());
        // Who was counted apart still is.
        assert!(state.check_cooldown(&codes, numbered(0)).is_ok());
    }

    /// A pairing code asked for as its device was being revoked: the
    /// request had passed the token's check, and the revocation's sweep of
    /// that device's codes came before the code was made.
    #[test]
    fn a_code_that_outlives_its_device_lets_nobody_in() {
        let state = state(Config::default());
        let Ok(device) = register(&state, "Mac", None) else {
            panic!("the first device registers");
        };
        assert!(state.store().revoke(&device.device_id).unwrap());
        let code = pairing_code(&state, Some(device.device_id.clone()));
        assert_eq!(state.codes().pairing.len(), 1, "{code:?}");

        assert!(register(&state, "Thief", Some(&device.device_id)).is_err());
        assert_eq!(state.store().devices().unwrap(), vec![]);
        // The operator's code names no device: it still lets one in.
        assert!(register(&state, "Rescue", None).is_ok());
    }

    #[test]
    fn an_ipv6_client_is_its_network() {
        let proxy = Some(address("127.0.0.1"));
        assert_eq!(
            client_address(proxy, Some("2001:db8:1:2:3:4:5:6")),
            client_address(proxy, Some("2001:db8:1:ff::ffff"))
        );
        assert_ne!(
            client_address(proxy, Some("2001:db8:1:2::1")),
            client_address(proxy, Some("2001:db8:1:102::1"))
        );
        assert_eq!(
            client_address(proxy, Some("::ffff:203.0.113.9")),
            address("203.0.113.9")
        );
    }
}
