//! Talking to the sync server: the HTTP client, one sync round, and the
//! background worker that syncs when the server says something changed
//! (WebSocket) or every 30 seconds.
//!
//! Only ciphertext and the vault's header go over the wire; the vault
//! ([`crate::vault`]) does the crypto and the verification.

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use clipboardx_sync_proto::{
    BootstrapRequest, Changes, Device, DeviceToken, ErrorBody, JoinRequest, PairingCode,
    PushRequest, PushResponse, VaultState, VaultUpdate,
};
use reqwest::StatusCode;
use reqwest::blocking::{Client as Http, RequestBuilder, Response};
use serde::de::DeserializeOwned;
use zeroize::Zeroizing;

use crate::vault::sync::Applied;
use crate::vault::{VaultError, VaultService, VaultStatus};

const TIMEOUT: Duration = Duration::from_secs(20);
/// How often to sync without news from the server.
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);
const MAX_RECONNECT_WAIT: Duration = Duration::from_secs(60);
/// Rounds of push, pull and manifest before giving up on a busy server.
const MAX_ROUNDS: usize = 3;

fn sync_error(why: impl std::fmt::Display) -> VaultError {
    VaultError::Sync(why.to_string())
}

/// `url` as the base of the API (`…/v1`), when it is safe to send to:
/// `https://` anywhere; plain `http://` only on this machine (tests, a
/// tunnel), since tokens would cross the network in the clear.
pub fn api_base(url: &str) -> Result<String, VaultError> {
    let url = url.trim().trim_end_matches('/');
    let parsed = reqwest::Url::parse(url).map_err(|_| sync_error("服务器地址格式不正确"))?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    let local = || {
        host == "localhost"
            || host == "[::1]"
            || host.parse::<Ipv4Addr>().is_ok_and(|ip| ip.is_loopback())
    };
    match parsed.scheme() {
        "https" => {}
        "http" if local() => {}
        "http" => {
            return Err(sync_error(
                "服务器地址需使用 https://，明文 http 只允许本机地址",
            ));
        }
        _ => return Err(sync_error("服务器地址需以 https:// 开头")),
    }
    if host.is_empty() || parsed.path() != "/" && !parsed.path().is_empty() {
        return Err(sync_error(
            "服务器地址只需填写域名和端口，例如 https://sync.example.com:8443",
        ));
    }
    Ok(format!("{url}{}", clipboardx_sync_proto::API))
}

/// The largest answer read from the server.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024 * 1024;

/// A response's body, up to [`MAX_RESPONSE_BYTES`].
fn read_limited(response: Response) -> Result<Vec<u8>, VaultError> {
    use std::io::Read as _;
    let mut body = Vec::new();
    response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(sync_error)?;
    if body.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(sync_error("服务器的应答过大"));
    }
    Ok(body)
}

/// The sync server's API.
pub struct Client {
    base: String,
    token: Option<Zeroizing<String>>,
    http: Http,
}

impl Client {
    pub fn new(server_url: &str) -> Result<Client, VaultError> {
        Ok(Client {
            base: api_base(server_url)?,
            token: None,
            http: Http::builder()
                .timeout(TIMEOUT)
                // A redirect could carry a code or the token's request to a
                // host api_base refuses.
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(sync_error)?,
        })
    }

    pub fn with_token(mut self, token: Zeroizing<String>) -> Client {
        self.token = Some(token);
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/{path}", self.base)
    }

    fn authed(&self, request: RequestBuilder) -> RequestBuilder {
        match &self.token {
            Some(token) => request.bearer_auth(token.as_str()),
            None => request,
        }
    }

    fn send(&self, request: RequestBuilder) -> Result<Response, VaultError> {
        self.authed(request).send().map_err(|e| {
            log::warn!("sync request failed: {e}");
            sync_error("无法连接同步服务器，请检查地址和网络连接")
        })
    }

    /// The body of a successful answer; `conflict` maps 409.
    fn expect<T: DeserializeOwned>(response: Response) -> Result<T, VaultError> {
        let status = response.status();
        let body = read_limited(response)?;
        if status.is_success() {
            return serde_json::from_slice(&body).map_err(sync_error);
        }
        let message = serde_json::from_slice::<ErrorBody>(&body)
            .map(|b| b.error)
            .unwrap_or_default();
        Err(match status {
            StatusCode::UNAUTHORIZED => sync_error(format!("认证失败（{message}）")),
            StatusCode::CONFLICT => sync_error(format!("服务器拒绝（{message}）")),
            _ => sync_error(format!("服务器错误 {status}（{message}）")),
        })
    }

    pub fn bootstrap(
        &self,
        admin_code: &str,
        device_name: &str,
    ) -> Result<DeviceToken, VaultError> {
        let request = self
            .http
            .post(self.url("bootstrap"))
            .json(&BootstrapRequest {
                admin_code: admin_code.to_owned(),
                device_name: device_name.to_owned(),
            });
        Self::expect(self.send(request)?)
    }

    pub fn join(&self, code: &str, device_name: &str) -> Result<DeviceToken, VaultError> {
        let request = self.http.post(self.url("join")).json(&JoinRequest {
            code: code.to_owned(),
            device_name: device_name.to_owned(),
        });
        Self::expect(self.send(request)?)
    }

    pub fn pair(&self) -> Result<PairingCode, VaultError> {
        Self::expect(self.send(self.http.post(self.url("pair")))?)
    }

    pub fn devices(&self) -> Result<Vec<Device>, VaultError> {
        Self::expect(self.send(self.http.get(self.url("devices")))?)
    }

    pub fn revoke(&self, device_id: &str) -> Result<(), VaultError> {
        let response = self.send(self.http.delete(self.url(&format!("devices/{device_id}"))))?;
        match response.status() {
            s if s.is_success() => Ok(()),
            _ => Self::expect::<()>(response),
        }
    }

    pub fn vault(&self) -> Result<VaultState, VaultError> {
        Self::expect(self.send(self.http.get(self.url("vault")))?)
    }

    /// Whether the server took it (`false`: someone got there first).
    pub fn put_vault(&self, update: &VaultUpdate) -> Result<bool, VaultError> {
        let response = self.send(self.http.put(self.url("vault")).json(update))?;
        match response.status() {
            StatusCode::CONFLICT => Ok(false),
            s if s.is_success() => Ok(true),
            _ => Self::expect::<()>(response).map(|()| false),
        }
    }

    /// `None`: the header changed on the server.
    pub fn push(&self, request: &PushRequest) -> Result<Option<PushResponse>, VaultError> {
        let response = self.send(self.http.post(self.url("changes")).json(request))?;
        if response.status() == StatusCode::CONFLICT {
            return Ok(None);
        }
        Self::expect(response).map(Some)
    }

    pub fn changes(&self, since: u64) -> Result<Changes, VaultError> {
        Self::expect(self.send(self.http.get(self.url(&format!("changes?since={since}"))))?)
    }

    /// The WebSocket URL of `/events`.
    fn events_url(&self) -> String {
        let url = self.url("events");
        match url.strip_prefix("https://") {
            Some(rest) => format!("wss://{rest}"),
            None => url.replacen("http://", "ws://", 1),
        }
    }
}

/// What one sync did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Report {
    pub pushed: usize,
    pub applied: Applied,
}

/// One sync of the unlocked `vault` with the server: the header first, then
/// local changes up, the server's down (verified), and a new manifest.
pub fn sync_once(vault: &VaultService, client: &Client) -> Result<Report, VaultError> {
    // A new master password from another device waits to be entered: until
    // then this device's records are under a key the others no longer use.
    if vault.has_staged_header() {
        return Err(VaultError::HeaderChanged);
    }
    let mut report = Report::default();
    let mut remote = client.vault()?;
    let local_header = vault.header_version()?;
    let remote_manifest = |remote: &VaultState| remote.manifest.as_ref().map_or(0, |m| m.version);
    match remote.header.as_ref() {
        // Another device changed the master password: its header waits apart
        // until the new password opens it. One that keeps the data key (a
        // new recovery phrase) is taken as it comes.
        Some(header) if header.version > local_header => {
            if vault.receive_header(header)? {
                return Err(VaultError::HeaderChanged);
            }
        }
        // This device did, or it is the first: the header, with a manifest
        // sealed under its key, goes up before any record. The manifest is
        // numbered after the server's (sealed under the old key, so not
        // checked here) without taking that number as its own.
        header if header.is_none_or(|h| h.version < local_header) => {
            let manifest = vault.next_manifest(remote_manifest(&remote))?;
            let update = VaultUpdate {
                header: Some(vault.header()?),
                base_header: header.map_or(0, |h| h.version),
                manifest: Some(manifest.clone()),
                base_manifest: remote_manifest(&remote),
            };
            if !client.put_vault(&update)? {
                return Err(sync_error("服务器上的密码本已被其他设备更新，请稍后重试"));
            }
            vault.manifest_stored(manifest.version)?;
            remote = client.vault()?;
        }
        // Two devices changed the header at once: records wait.
        Some(header) if !vault.is_own_header(header)? => {
            vault.resolve_header_conflict(header)?;
        }
        // The same header on both sides.
        _ => {}
    }
    for _ in 0..MAX_ROUNDS {
        let pending = vault.pending()?;
        if !pending.is_empty() {
            let request = PushRequest {
                header_version: vault.header_version()?,
                records: pending.clone(),
            };
            let Some(pushed) = client.push(&request)? else {
                // The header changed meanwhile: next sync takes it.
                return Err(sync_error("服务器上的密码本头部已变化，将重新同步"));
            };
            vault.mark_pushed(&pending, &pushed.conflicts)?;
            report.pushed += pending.len() - pushed.conflicts.len();
        }
        let since = vault.sync_state()?.map_or(0, |s| s.last_seq);
        let applied = match vault.apply(&client.changes(since)?, remote.manifest.as_ref()) {
            // A cursor the server pushed too far makes records look withheld:
            // one full pull settles whether anything is wrong.
            Err(VaultError::Tampered) if since > 0 => {
                vault.reset_sync_cursor()?;
                vault.apply(&client.changes(0)?, remote.manifest.as_ref())?
            }
            result => result?,
        };
        report.applied.changed += applied.changed;
        report.applied.conflicts += applied.conflicts;
        // Nothing moved and the server has a manifest: done.
        if pending.is_empty() && applied == Applied::default() && remote.manifest.is_some() {
            return Ok(report);
        }
        let manifest = vault.next_manifest(remote_manifest(&remote))?;
        let update = VaultUpdate {
            manifest: Some(manifest.clone()),
            base_manifest: remote_manifest(&remote),
            ..VaultUpdate::default()
        };
        if client.put_vault(&update)? {
            vault.manifest_stored(manifest.version)?;
            if vault.pending()?.is_empty() {
                return Ok(report);
            }
        }
        // Another device wrote meanwhile, or conflict copies wait: again.
        remote = client.vault()?;
    }
    Ok(report)
}

/// The client of the vault's sync state, or `None` while sync is off.
pub fn client_for(vault: &VaultService) -> Result<Option<Client>, VaultError> {
    let Some(state) = vault.sync_state()? else {
        return Ok(None);
    };
    let token = vault
        .device_token()
        .ok_or_else(|| sync_error("此设备的同步凭证丢失，请关闭同步后重新加入"))?;
    Ok(Some(Client::new(&state.server_url)?.with_token(token)))
}

// --- The background worker ------------------------------------------------

/// What the settings page shows of sync.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub syncing: bool,
    /// Unix ms of the last successful sync.
    pub last_ok_ms: u64,
    pub last_error: Option<String>,
    /// Conflict copies made by the last sync.
    pub conflicts: usize,
}

/// Syncs in the background while the vault is unlocked and sync is on.
pub struct Worker {
    trigger: Mutex<Option<Sender<()>>>,
    status: Arc<Mutex<Status>>,
    started: AtomicBool,
}

impl Default for Worker {
    fn default() -> Self {
        Worker {
            trigger: Mutex::new(None),
            status: Arc::new(Mutex::new(Status::default())),
            started: AtomicBool::new(false),
        }
    }
}

impl Worker {
    pub fn status(&self) -> Status {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// Syncs soon (after a local change, an unlock, a press of the button).
    pub fn sync_now(&self) {
        if let Ok(trigger) = self.trigger.lock()
            && let Some(trigger) = trigger.as_ref()
        {
            let _ = trigger.send(());
        }
    }

    /// Starts the threads, once. `vault` gives the vault; `changed` runs
    /// after a sync that changed something.
    pub fn start(
        &self,
        vault: impl Fn() -> &'static VaultService + Send + Sync + 'static,
        changed: impl Fn() + Send + 'static,
    ) {
        if self.started.swap(true, Ordering::AcqRel) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        if let Ok(mut trigger) = self.trigger.lock() {
            *trigger = Some(tx.clone());
        }
        let vault = Arc::new(vault);
        let status = self.status.clone();
        let sync_vault = vault.clone();
        let spawned = thread::Builder::new()
            .name("vault-sync".into())
            .spawn(move || sync_loop(&*sync_vault, &rx, &status, &changed));
        if let Err(e) = spawned {
            log::error!("cannot start the sync thread: {e}");
            return;
        }
        let spawned = thread::Builder::new()
            .name("vault-events".into())
            .spawn(move || events_loop(&*vault, &tx));
        if let Err(e) = spawned {
            log::error!("cannot start the sync events thread: {e}");
        }
    }
}

fn sync_loop(
    vault: &dyn Fn() -> &'static VaultService,
    trigger: &Receiver<()>,
    status: &Mutex<Status>,
    changed: &dyn Fn(),
) {
    loop {
        match trigger.recv_timeout(POLL_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        // Several triggers at once make one sync.
        while trigger.try_recv().is_ok() {}
        let vault = vault();
        if vault.status() != VaultStatus::Unlocked {
            continue;
        }
        let client = match client_for(vault) {
            Ok(Some(client)) => client,
            Ok(None) => continue,
            Err(e) => {
                set_status(status, |s| s.last_error = Some(e.to_string()));
                continue;
            }
        };
        set_status(status, |s| s.syncing = true);
        let result = sync_once(vault, &client);
        set_status(status, |s| {
            s.syncing = false;
            match &result {
                Ok(report) => {
                    s.last_ok_ms = crate::vault::now_ms();
                    s.last_error = None;
                    s.conflicts = report.applied.conflicts;
                }
                Err(e) => s.last_error = Some(e.to_string()),
            }
        });
        match result {
            Ok(report) if report.applied.changed > 0 => changed(),
            Err(VaultError::HeaderChanged) => changed(),
            Err(e) => log::warn!("sync failed: {e}"),
            Ok(_) => {}
        }
    }
}

fn set_status(status: &Mutex<Status>, f: impl FnOnce(&mut Status)) {
    if let Ok(mut status) = status.lock() {
        f(&mut status);
    }
}

/// Keeps a WebSocket to `/events` while sync is on and wakes the sync loop
/// on every event; reconnects with backoff.
fn events_loop(vault: &dyn Fn() -> &'static VaultService, trigger: &Sender<()>) {
    let mut wait = Duration::from_secs(1);
    loop {
        let client = client_for(vault()).ok().flatten();
        let Some(client) = client else {
            thread::sleep(Duration::from_secs(5));
            continue;
        };
        match listen(&client, trigger) {
            // The server closed it: reconnect, but not in a tight loop.
            Ok(()) => {
                wait = Duration::from_secs(1);
                thread::sleep(wait);
            }
            Err(e) => {
                log::debug!("sync events: {e}");
                thread::sleep(wait);
                wait = (wait * 2).min(MAX_RECONNECT_WAIT);
            }
        }
        if trigger.send(()).is_err() {
            return;
        }
    }
}

/// One WebSocket session: each message wakes the sync loop.
fn listen(client: &Client, trigger: &Sender<()>) -> Result<(), String> {
    use tungstenite::client::IntoClientRequest as _;
    let mut request = client
        .events_url()
        .into_client_request()
        .map_err(|e| e.to_string())?;
    if let Some(token) = &client.token {
        let value = format!("Bearer {}", token.as_str())
            .parse()
            .map_err(|_| "bad token".to_owned())?;
        request.headers_mut().insert("Authorization", value);
    }
    let (mut socket, _) = tungstenite::connect(request).map_err(|e| e.to_string())?;
    if let tungstenite::stream::MaybeTlsStream::Plain(stream) = socket.get_ref() {
        // Wakes the loop to check sync is still on.
        let _ = stream.set_read_timeout(Some(POLL_INTERVAL * 2));
    }
    loop {
        match socket.read() {
            Ok(tungstenite::Message::Text(_)) => {
                if trigger.send(()).is_err() {
                    return Ok(());
                }
            }
            Ok(tungstenite::Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(e) => return Err(e.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_http_only_on_this_machine() {
        assert_eq!(
            api_base("https://sync.example.com:8443/").unwrap(),
            "https://sync.example.com:8443/v1"
        );
        assert!(api_base("http://127.0.0.1:8787").is_ok());
        assert!(api_base("http://localhost:8787").is_ok());
        assert!(api_base("http://[::1]:8787").is_ok());
        for bad in [
            "http://100.101.102.103:8787",
            "http://pve.tail1234.ts.net:8787",
            "http://192.168.1.10:8787",
            "http://example.com",
            "ftp://sync.example.com",
            "not a url",
            "https://sync.example.com/some/path",
        ] {
            assert!(api_base(bad).is_err(), "{bad} should be refused");
        }
    }

    #[test]
    fn events_go_over_ws_or_wss() {
        let plain = Client::new("http://127.0.0.1:8787").unwrap();
        assert_eq!(plain.events_url(), "ws://127.0.0.1:8787/v1/events");
        let tls = Client::new("https://vault.example.com").unwrap();
        assert_eq!(tls.events_url(), "wss://vault.example.com/v1/events");
    }
}
