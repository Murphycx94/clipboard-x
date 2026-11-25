//! The sync server's clipboard history endpoints.

use clipboardx_sync_proto::history::{HistoryPage, HistoryPush, HistoryPushed};
use reqwest::StatusCode;
use reqwest::blocking::Response;
use serde::de::DeserializeOwned;

use super::{Client, sync_error};
use crate::vault::VaultError;

/// What a server from before history sync answers.
pub const OLD_SERVER: &str = "同步服务器需要升级才能同步复制历史";

/// The body of a history answer; `None` when the server's header is not
/// the one the request named.
fn answer<T: DeserializeOwned>(response: Response) -> Result<Option<T>, VaultError> {
    match response.status() {
        StatusCode::CONFLICT => Ok(None),
        StatusCode::NOT_FOUND => Err(sync_error(OLD_SERVER)),
        _ => Client::expect(response).map(Some),
    }
}

impl Client {
    /// The items changed after `since`, sealed under header version `header`.
    pub fn history(&self, since: u64, header: u64) -> Result<Option<HistoryPage>, VaultError> {
        let url = self.url(&format!("history?since={since}&header={header}"));
        answer(self.send(self.http.get(url))?)
    }

    pub fn push_history(
        &self,
        request: &HistoryPush,
    ) -> Result<Option<HistoryPushed>, VaultError> {
        answer(self.send(self.http.post(self.url("history")).json(request))?)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;

    use super::*;

    /// A server on this machine answering one request with `status`.
    fn answering(status: &'static str) -> Client {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let _ = stream.read(&mut request);
            let response = format!("HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
            stream.write_all(response.as_bytes()).unwrap();
        });
        Client::new(&format!("http://{address}")).unwrap()
    }

    #[test]
    fn a_server_without_history_is_told_to_upgrade() {
        let client = answering("404 Not Found");
        assert_eq!(
            client.history(0, 1).unwrap_err(),
            VaultError::Sync(OLD_SERVER.into())
        );
    }

    #[test]
    fn another_header_on_the_server_is_no_answer() {
        let client = answering("409 Conflict");
        let push = HistoryPush {
            header_version: 1,
            records: vec![],
        };
        assert_eq!(client.push_history(&push).unwrap(), None);
    }
}
