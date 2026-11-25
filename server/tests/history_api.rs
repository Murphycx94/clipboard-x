//! The clipboard history's endpoints over real HTTP, on a port of their own.

use std::time::Duration;

use clipboardx_sync_proto::history::{
    HistoryPage, HistoryPush, HistoryPushed, HistoryUp, MAX_CIPHERTEXT, MAX_PUSH_ITEMS,
};
use clipboardx_sync_proto::{BootstrapRequest, Bytes, DeviceToken, VaultHeader, VaultUpdate};
use clipboardx_sync_server::{Config, new_state, serve, store::Store};
use futures_util::StreamExt;
use reqwest::StatusCode;

struct Server {
    base: String,
    http: reqwest::Client,
    token: String,
}

/// A server with one device and a vault at header version 1.
async fn start() -> Server {
    start_with(Config::default()).await
}

async fn start_with(config: Config) -> Server {
    let (state, admin) = new_state(Store::in_memory().unwrap(), config).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { serve(listener, state).await.unwrap() });
    let base = format!("http://{addr}/v1");
    let http = reqwest::Client::new();
    let token = http
        .post(format!("{base}/bootstrap"))
        .json(&BootstrapRequest {
            admin_code: admin.unwrap(),
            device_name: "Mac".into(),
        })
        .send()
        .await
        .unwrap()
        .json::<DeviceToken>()
        .await
        .unwrap()
        .token;
    let server = Server { base, http, token };
    assert_eq!(server.put_header(1, 0).await, StatusCode::NO_CONTENT);
    server
}

impl Server {
    async fn put_header(&self, version: u64, base: u64) -> StatusCode {
        self.http
            .put(format!("{}/vault", self.base))
            .bearer_auth(&self.token)
            .json(&VaultUpdate {
                header: Some(header(version)),
                base_header: base,
                ..VaultUpdate::default()
            })
            .send()
            .await
            .unwrap()
            .status()
    }

    async fn push(&self, header_version: u64, records: Vec<HistoryUp>) -> reqwest::Response {
        self.http
            .post(format!("{}/history", self.base))
            .bearer_auth(&self.token)
            .json(&HistoryPush {
                header_version,
                records,
            })
            .send()
            .await
            .unwrap()
    }

    async fn page(&self, since: u64, header: u64) -> reqwest::Response {
        self.http
            .get(format!(
                "{}/history?since={since}&header={header}",
                self.base
            ))
            .bearer_auth(&self.token)
            .send()
            .await
            .unwrap()
    }

    async fn items(&self, since: u64) -> HistoryPage {
        let response = self.page(since, 1).await;
        assert_eq!(response.status(), StatusCode::OK);
        response.json().await.unwrap()
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

fn item(id: char, size: usize) -> HistoryUp {
    HistoryUp {
        id: id.to_string().repeat(64),
        nonce: Bytes(vec![1; 24]),
        ciphertext: Bytes(vec![7; size]),
    }
}

fn first_chars(page: &HistoryPage) -> String {
    page.records
        .iter()
        .map(|r| r.id.chars().next().unwrap())
        .collect()
}

#[tokio::test]
async fn history_needs_a_token() {
    let server = start().await;
    let status = server
        .http
        .get(format!("{}/history?since=0&header=1", server.base))
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn items_go_up_and_come_down_by_number() {
    let server = start().await;
    let response = server.push(1, vec![item('a', 32), item('b', 32)]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<HistoryPushed>().await.unwrap().seq, 2);
    server.push(1, vec![item('a', 40)]).await;

    let all = server.items(0).await;
    assert_eq!(first_chars(&all), "ba");
    assert_eq!(all.records[1].ciphertext.0.len(), 40);
    assert_eq!((all.seq, all.more), (3, false));
    assert_eq!(first_chars(&server.items(2).await), "a");
}

#[tokio::test]
async fn another_header_version_is_refused_both_ways() {
    let server = start().await;
    assert_eq!(
        server.push(2, vec![item('a', 32)]).await.status(),
        StatusCode::CONFLICT
    );
    assert_eq!(server.page(0, 2).await.status(), StatusCode::CONFLICT);
    assert_eq!(server.page(0, 0).await.status(), StatusCode::CONFLICT);
    assert!(server.items(0).await.records.is_empty());
}

#[tokio::test]
async fn malformed_items_are_refused_whole() {
    let server = start().await;
    let mut short_id = item('a', 32);
    short_id.id.pop();
    let mut upper_id = item('a', 32);
    upper_id.id = "A".repeat(64);
    let mut short_nonce = item('a', 32);
    short_nonce.nonce.0.pop();
    for bad in [
        short_id,
        upper_id,
        short_nonce,
        // Shorter than the authentication tag.
        item('a', 15),
        item('a', MAX_CIPHERTEXT + 1),
    ] {
        let response = server.push(1, vec![item('b', 32), bad]).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
    // Not even the good one went in.
    assert!(server.items(0).await.records.is_empty());
    assert_eq!(
        server
            .push(1, vec![item('a', MAX_CIPHERTEXT)])
            .await
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn an_upload_of_too_many_items_is_refused() {
    let server = start().await;
    let ids = (0..=MAX_PUSH_ITEMS).map(|n| HistoryUp {
        id: format!("{n:064x}"),
        ..item('a', 16)
    });
    let response = server.push(1, ids.collect()).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(server.items(0).await.records.is_empty());
}

#[tokio::test]
async fn the_history_stops_taking_items_when_full() {
    let server = start_with(Config {
        history_bytes: 100,
        ..Config::default()
    })
    .await;
    let response = server.push(1, vec![item('a', 60)]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let full = server.push(1, vec![item('b', 60)]).await;
    assert_eq!(full.status(), StatusCode::INSUFFICIENT_STORAGE);
    // Replacing an item with one no larger still goes in.
    let replaced = server.push(1, vec![item('a', 60)]).await;
    assert_eq!(replaced.status(), StatusCode::OK);
}

#[tokio::test]
async fn large_histories_come_down_in_pages() {
    let server = start().await;
    let big = MAX_CIPHERTEXT * 3 / 4;
    server
        .push(1, vec![item('a', big), item('b', big), item('c', big)])
        .await;
    let first = server.items(0).await;
    assert_eq!((first_chars(&first).as_str(), first.more), ("ab", true));
    let last = first.records.last().unwrap().seq;
    let second = server.items(last).await;
    assert_eq!((first_chars(&second).as_str(), second.more), ("c", false));
}

#[tokio::test]
async fn a_new_header_leaves_no_item_of_the_old_key() {
    let server = start().await;
    server.push(1, vec![item('a', 32)]).await;
    assert_eq!(server.put_header(2, 1).await, StatusCode::NO_CONTENT);
    let response = server.page(0, 2).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .json::<HistoryPage>()
            .await
            .unwrap()
            .records
            .is_empty()
    );
}

#[tokio::test]
async fn devices_hear_of_history_changes() {
    let server = start().await;
    let url = format!("{}/events", server.base.replace("http://", "ws://"));
    let mut request =
        tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
            .unwrap();
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", server.token).parse().unwrap(),
    );
    let mut socket = tokio_tungstenite::connect_async(request).await.unwrap().0;
    // What it says on connecting.
    socket.next().await.unwrap().unwrap();

    server.push(1, vec![item('a', 32)]).await;
    let event = tokio::time::timeout(Duration::from_secs(5), socket.next()).await;
    assert!(event.unwrap().unwrap().unwrap().is_text());
}
