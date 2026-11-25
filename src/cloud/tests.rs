use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;

use super::*;

/// Long enough for a busy machine to answer in, short enough to wait
/// through several times.
const SHORT: Duration = Duration::from_millis(300);

/// A client of a server on this machine that takes one events
/// connection and gives it to `serve`.
fn events_server(serve: impl FnOnce(tungstenite::WebSocket<TcpStream>) + Send + 'static) -> Client {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        serve(tungstenite::accept(stream).unwrap());
    });
    Client::new(&format!("http://{address}")).unwrap()
}

#[test]
fn a_quiet_connection_is_kept_while_the_server_answers() {
    // Reading is what answers a ping.
    let client = events_server(|mut socket| while socket.read().is_ok() {});
    let (trigger, _triggered) = mpsc::channel();
    let quiet_times = AtomicUsize::new(0);
    let still_on = || quiet_times.fetch_add(1, Ordering::SeqCst) < 4;
    assert_eq!(listen(&client, &trigger, SHORT, &still_on), Ok(()));
    // Kept through four quiet times, ended when sync went off.
    assert_eq!(quiet_times.load(Ordering::SeqCst), 5);
}

#[test]
fn the_real_server_answers_so_its_connection_is_kept() {
    use clipboardx_sync_server::{Config, new_state, serve, store::Store};
    let (state, admin) = new_state(Store::in_memory().unwrap(), Config::default()).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    thread::spawn(move || runtime.block_on(async { serve(listener, state).await.unwrap() }));
    let device = Client::new(&url)
        .unwrap()
        .bootstrap(&admin.unwrap(), "Mac")
        .unwrap();
    let client = Client::new(&url)
        .unwrap()
        .with_token(Zeroizing::new(device.token));

    let (trigger, triggered) = mpsc::channel();
    let quiet_times = AtomicUsize::new(0);
    let still_on = || quiet_times.fetch_add(1, Ordering::SeqCst) < 4;
    assert_eq!(listen(&client, &trigger, SHORT, &still_on), Ok(()));
    assert_eq!(quiet_times.load(Ordering::SeqCst), 5);
    // What the server says on connecting.
    assert_eq!(triggered.try_iter().count(), 1);
}

#[test]
fn a_connection_that_stopped_answering_is_dropped() {
    let (hang_up, hung_up) = mpsc::channel::<()>();
    // Connected, then gone without a word: nothing reads the pings.
    let client = events_server(move |_socket| {
        let _ = hung_up.recv();
    });
    let (trigger, _triggered) = mpsc::channel();
    let started = Instant::now();
    let ended = listen(&client, &trigger, SHORT, &|| true);
    assert_eq!(ended, Err("the server stopped answering".into()));
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(hang_up);
}

#[test]
fn a_server_that_takes_the_connection_and_says_nothing_is_given_up_on() {
    // Not even the WebSocket handshake.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (hang_up, hung_up) = mpsc::channel::<()>();
    thread::spawn(move || {
        let _held = listener.accept();
        let _ = hung_up.recv();
    });
    let client = Client::new(&format!("http://{address}")).unwrap();
    let (trigger, _triggered) = mpsc::channel();
    let started = Instant::now();
    assert!(listen(&client, &trigger, SHORT, &|| true).is_err());
    assert!(started.elapsed() < Duration::from_secs(5));
    drop(hang_up);
}

#[test]
fn an_events_connection_over_tls_fails_at_the_handshake_not_before() {
    // No TLS server: the handshake is as far as it can get, and it
    // must get there (the TLS setup before it once could not).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || drop(listener.accept()));
    let client = Client::new(&format!("https://{address}")).unwrap();
    let (trigger, _triggered) = mpsc::channel();
    assert!(listen(&client, &trigger, SHORT, &|| true).is_err());
}

#[test]
fn clients_share_their_connections() {
    use std::io::{Read as _, Write as _};
    use std::sync::atomic::AtomicUsize;
    // An HTTP server that keeps connections open and counts them.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let counted = connections.clone();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            counted.fetch_add(1, Ordering::SeqCst);
            thread::spawn(move || {
                let mut request = [0_u8; 4096];
                while matches!(stream.read(&mut request), Ok(n) if n > 0) {
                    let answer = "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                                  content-length: 2\r\n\r\n[]";
                    if stream.write_all(answer.as_bytes()).is_err() {
                        return;
                    }
                }
            });
        }
    });
    let url = format!("http://{address}");
    // As each sync makes a client of its own.
    for _ in 0..3 {
        let client = Client::new(&url).unwrap();
        assert!(client.devices().unwrap().is_empty());
    }
    assert_eq!(connections.load(Ordering::SeqCst), 1);
}

#[test]
fn events_wake_the_sync() {
    let client = events_server(|mut socket| {
        socket.send(tungstenite::Message::text("{}")).unwrap();
        let _ = socket.close(None);
        while socket.read().is_ok() {}
    });
    let (trigger, triggered) = mpsc::channel();
    assert_eq!(listen(&client, &trigger, SHORT, &|| true), Ok(()));
    assert_eq!(triggered.try_iter().count(), 1);
}

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
