//! The server's API over real HTTP, on a port of its own.

use std::time::Duration;

use clipboardx_sync_proto::{
    BootstrapRequest, Bytes, Changes, Device, DeviceToken, Event, JoinRequest, Manifest,
    PairingCode, PushRequest, PushResponse, RecordUp, VaultHeader, VaultState, VaultUpdate,
};
use std::sync::Arc;

use clipboardx_sync_server::{
    AppState, Config, issue_pairing_code, new_state, router, store::Store,
};
use futures_util::StreamExt;
use reqwest::StatusCode;

struct Server {
    base: String,
    admin: String,
    http: reqwest::Client,
    state: Arc<AppState>,
}

async fn start(config: Config) -> Server {
    let (state, admin) = new_state(Store::in_memory().unwrap(), config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let served = state.clone();
    tokio::spawn(async move { axum::serve(listener, router(served)).await.unwrap() });
    Server {
        base: format!("http://{addr}/v1"),
        admin: admin.unwrap(),
        http: reqwest::Client::new(),
        state,
    }
}

impl Server {
    async fn bootstrap(&self, code: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/bootstrap", self.base))
            .json(&BootstrapRequest {
                admin_code: code.into(),
                device_name: "Mac".into(),
            })
            .send()
            .await
            .unwrap()
    }

    async fn first_device(&self) -> String {
        let response = self.bootstrap(&self.admin).await;
        assert_eq!(response.status(), StatusCode::OK);
        response.json::<DeviceToken>().await.unwrap().token
    }

    async fn join(&self, code: &str) -> reqwest::Response {
        self.http
            .post(format!("{}/join", self.base))
            .json(&JoinRequest {
                code: code.into(),
                device_name: "Laptop".into(),
            })
            .send()
            .await
            .unwrap()
    }

    async fn pair(&self, token: &str) -> String {
        let response = self
            .http
            .post(format!("{}/pair", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap();
        response.json::<PairingCode>().await.unwrap().code
    }

    async fn push(
        &self,
        token: &str,
        header_version: u64,
        records: Vec<RecordUp>,
    ) -> reqwest::Response {
        self.http
            .post(format!("{}/changes", self.base))
            .bearer_auth(token)
            .json(&PushRequest {
                header_version,
                records,
            })
            .send()
            .await
            .unwrap()
    }

    async fn changes(&self, token: &str, since: u64) -> Changes {
        self.http
            .get(format!("{}/changes?since={since}", self.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }

    async fn put_vault(&self, token: &str, update: &VaultUpdate) -> StatusCode {
        self.http
            .put(format!("{}/vault", self.base))
            .bearer_auth(token)
            .json(update)
            .send()
            .await
            .unwrap()
            .status()
    }
}

fn record(id: &str, base: u64, revision: u64) -> RecordUp {
    RecordUp {
        id: id.into(),
        base_revision: base,
        revision,
        deleted: false,
        nonce: Bytes(vec![1; 24]),
        ciphertext: Bytes(vec![revision as u8; 32]),
    }
}

fn header(version: u64) -> VaultHeader {
    VaultHeader {
        version,
        format: 2,
        kdf_m: 65536,
        kdf_t: 3,
        kdf_p: 1,
        salt: Bytes(vec![1; 16]),
        wrapped_key: Bytes(vec![2; 72]),
        key_check: Bytes(vec![3; 64]),
        auth: None,
        recovery_wrap: None,
        recovery_sealed: None,
        seal: Some(Bytes(vec![4; 72])),
    }
}

#[tokio::test]
async fn the_first_device_needs_the_admin_code_once() {
    let server = start(Config::default()).await;
    assert_eq!(
        server.bootstrap("WRONG-CODE").await.status(),
        StatusCode::UNAUTHORIZED
    );
    // Case, spaces and dashes do not matter.
    let loose = server.admin.to_lowercase().replace('-', " ");
    assert_eq!(server.bootstrap(&loose).await.status(), StatusCode::OK);
    assert_eq!(
        server.bootstrap(&server.admin).await.status(),
        StatusCode::CONFLICT
    );
}

#[tokio::test]
async fn requests_need_a_valid_token() {
    let server = start(Config::default()).await;
    let token = server.first_device().await;
    for (method, path) in [
        ("GET", "vault"),
        ("GET", "devices"),
        ("GET", "changes"),
        ("POST", "pair"),
    ] {
        let url = format!("{}/{path}", server.base);
        let request = match method {
            "GET" => server.http.get(&url),
            _ => server.http.post(&url),
        };
        assert_eq!(
            request.try_clone().unwrap().send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );
        let wrong = request.bearer_auth("0".repeat(64)).send().await.unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    }
    let ok = server
        .http
        .get(format!("{}/vault", server.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    assert_eq!(
        ok.json::<VaultState>().await.unwrap(),
        VaultState::default()
    );
}

#[tokio::test]
async fn pairing_codes_are_single_use_and_expire() {
    let server = start(Config {
        code_ttl: Duration::from_millis(300),
        ..Config::default()
    })
    .await;
    let token = server.first_device().await;
    let code = server.pair(&token).await;
    let joined = server.join(&code).await;
    assert_eq!(joined.status(), StatusCode::OK);
    let second: DeviceToken = joined.json().await.unwrap();
    assert_eq!(server.join(&code).await.status(), StatusCode::UNAUTHORIZED);

    let late = server.pair(&second.token).await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(server.join(&late).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn wrong_codes_cool_down_without_voiding_the_users_code() {
    let server = start(Config {
        cooldown: Duration::from_millis(300),
        ..Config::default()
    })
    .await;
    let token = server.first_device().await;
    let code = server.pair(&token).await;
    for _ in 0..5 {
        assert_eq!(
            server.join("AAAA-AAAA").await.status(),
            StatusCode::UNAUTHORIZED
        );
    }
    // Even the right code waits out the cooldown, then still works.
    assert_eq!(
        server.join(&code).await.status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert_eq!(server.join(&code).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn the_operator_can_issue_a_pairing_code_when_every_device_is_lost() {
    let server = start(Config::default()).await;
    let _lost = server.first_device().await;
    // No admin code once a device exists.
    assert_eq!(
        server.bootstrap(&server.admin).await.status(),
        StatusCode::CONFLICT
    );
    let code = issue_pairing_code(&server.state).code;
    assert_eq!(server.join(&code).await.status(), StatusCode::OK);
    assert_eq!(server.join(&code).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_the_latest_pairing_codes_stay_valid() {
    let server = start(Config::default()).await;
    let token = server.first_device().await;
    let first = server.pair(&token).await;
    for _ in 0..5 {
        server.pair(&token).await;
    }
    assert_eq!(server.join(&first).await.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn records_move_forward_only_from_their_base() {
    let server = start(Config::default()).await;
    let token = server.first_device().await;
    let update = VaultUpdate {
        header: Some(header(1)),
        base_header: 0,
        ..VaultUpdate::default()
    };
    assert_eq!(
        server.put_vault(&token, &update).await,
        StatusCode::NO_CONTENT
    );

    let pushed: PushResponse = server
        .push(&token, 1, vec![record("a", 0, 1), record("b", 0, 1)])
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(
        pushed,
        PushResponse {
            seq: 2,
            conflicts: vec![]
        }
    );

    // A stale base and a revision that does not move forward are refused.
    let pushed: PushResponse = server
        .push(
            &token,
            1,
            vec![record("a", 0, 2), record("b", 1, 1), record("a", 1, 2)],
        )
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(pushed.conflicts, vec!["a".to_owned(), "b".to_owned()]);
    assert_eq!(pushed.seq, 3);

    let changes = server.changes(&token, 2).await;
    assert_eq!(changes.seq, 3);
    assert_eq!(changes.records.len(), 1);
    assert_eq!(
        (changes.records[0].id.as_str(), changes.records[0].revision),
        ("a", 2)
    );
    assert_eq!(server.changes(&token, 0).await.records.len(), 2);

    // Records under an older header are refused.
    let stale = server.push(&token, 0, vec![record("c", 0, 1)]).await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_header_and_manifest_move_forward_from_their_base() {
    let server = start(Config::default()).await;
    let token = server.first_device().await;
    let manifest = |version| Manifest {
        version,
        sealed: Bytes(vec![9; 40]),
    };
    let put = |header: Option<VaultHeader>, base_header, m: Option<Manifest>, base_manifest| {
        VaultUpdate {
            header,
            base_header,
            manifest: m,
            base_manifest,
        }
    };
    assert_eq!(
        server
            .put_vault(&token, &put(Some(header(1)), 0, Some(manifest(1)), 0))
            .await,
        StatusCode::NO_CONTENT
    );
    // Stale bases, and versions that do not move forward.
    assert_eq!(
        server
            .put_vault(&token, &put(Some(header(2)), 0, None, 0))
            .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        server
            .put_vault(&token, &put(None, 0, Some(manifest(2)), 0))
            .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        server
            .put_vault(&token, &put(None, 0, Some(manifest(1)), 1))
            .await,
        StatusCode::CONFLICT
    );
    // Both or neither: a good header with a stale manifest changes nothing.
    assert_eq!(
        server
            .put_vault(&token, &put(Some(header(2)), 1, Some(manifest(2)), 0))
            .await,
        StatusCode::CONFLICT
    );
    let state: VaultState = server
        .http
        .get(format!("{}/vault", server.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state.header, Some(header(1)));
    assert_eq!(state.manifest.unwrap().version, 1);
}

#[tokio::test]
async fn revoked_devices_lose_access() {
    let server = start(Config::default()).await;
    let token = server.first_device().await;
    let other: DeviceToken = server
        .join(&server.pair(&token).await)
        .await
        .json()
        .await
        .unwrap();
    let devices: Vec<Device> = server
        .http
        .get(format!("{}/devices", server.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(devices.len(), 2);
    let revoked = server
        .http
        .delete(format!("{}/devices/{}", server.base, other.device_id))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::NO_CONTENT);
    let after = server
        .http
        .get(format!("{}/vault", server.base))
        .bearer_auth(&other.token)
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn devices_hear_of_changes() {
    let server = start(Config::default()).await;
    let token = server.first_device().await;
    assert_eq!(
        server
            .put_vault(
                &token,
                &VaultUpdate {
                    header: Some(header(1)),
                    ..VaultUpdate::default()
                }
            )
            .await,
        StatusCode::NO_CONTENT
    );
    let url = format!("{}/events", server.base.replace("http://", "ws://"));
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
            .unwrap();
    request
        .headers_mut()
        .insert("Authorization", format!("Bearer {token}").parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let next = |text: String| serde_json::from_str::<Event>(&text).unwrap().seq;
    let first = socket
        .next()
        .await
        .unwrap()
        .unwrap()
        .into_text()
        .unwrap()
        .to_string();
    assert_eq!(next(first), 0);
    server.push(&token, 1, vec![record("a", 0, 1)]).await;
    let event = tokio::time::timeout(Duration::from_secs(5), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(next(event.into_text().unwrap().to_string()), 1);
}
