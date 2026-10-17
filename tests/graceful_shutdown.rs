//! `server::serve_with_shutdown`: an application that embeds the HTTP server
//! can stop it, keep-alive connections included, and then delete the files of
//! a file-backed database.
//!
//! The database runs in WAL mode, so its `-wal` file exists while any SQLite
//! connection is open and goes when the last one closes. That is how these
//! tests see that the server has let go of every clone of the database,
//! without reaching into its internals.

#![cfg(feature = "http-server")]

use dynoxide::Database;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How long a stop may take before the test calls it hung.
const STOPS_WITHIN: Duration = Duration::from_secs(5);
/// How long the database's `-wal` file may take to go once the server has
/// returned. The last connection task can drop its clone a moment after the
/// server returns, so this is a backstop and not a delay anyone should see.
const RELEASED_WITHIN: Duration = Duration::from_secs(2);
/// How long a request is held open after the stop signal. Long enough that a
/// server which gives running requests a short grace period and then drops
/// them ends the test in failure, short enough not to slow the suite.
const HELD_OPEN_FOR: Duration = Duration::from_secs(1);

const TARGET: &str = "DynamoDB_20120810.CreateTable";
const CONTENT_TYPE: &str = "application/x-amz-json-1.0";
const AMZ_DATE: &str = "20260101T000000Z";
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 Credential=fakekey/20260101/us-east-1/dynamodb/aws4_request, SignedHeaders=host;x-amz-date;x-amz-target, Signature=fakesig";

/// A `CreateTable` body. The table name needs at least three characters.
fn create_table_body() -> String {
    json!({
        "TableName": "Served",
        "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
        "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}]
    })
    .to_string()
}

fn wal_of(db_path: &Path) -> PathBuf {
    let mut wal = db_path.as_os_str().to_owned();
    wal.push("-wal");
    PathBuf::from(wal)
}

/// Waits for `wal` to stop existing, for up to [`RELEASED_WITHIN`].
async fn released(wal: &Path) -> bool {
    let deadline = tokio::time::Instant::now() + RELEASED_WITHIN;
    while wal.exists() {
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    true
}

/// Waits for the server to end and checks it ended cleanly.
async fn ended_ok(server: tokio::task::JoinHandle<Result<(), String>>, what: &str) {
    let ended = tokio::time::timeout(STOPS_WITHIN, server)
        .await
        .expect(what)
        .unwrap();
    assert_eq!(ended, Ok(()));
}

/// Writes through `client`, which then keeps its connection in its pool: an
/// idle keep-alive connection, which is what held the database open.
async fn write_and_keep_connection(client: &reqwest::Client, base: &str) {
    let response = client
        .post(base)
        .header("x-amz-target", TARGET)
        .header("content-type", CONTENT_TYPE)
        .header("authorization", AUTHORIZATION)
        .header("x-amz-date", AMZ_DATE)
        .body(create_table_body())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, 200, "CreateTable answered: {body}");
}

/// A server on a random loopback port, and the means to stop it.
struct RunningServer {
    base: String,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<Result<(), String>>,
}

/// Opens a file-backed database at `db_path` and serves it. The `-wal` file
/// has to exist first: `released` would pass at once for a file that was never
/// there, and then would show nothing.
async fn serve(db_path: &Path) -> RunningServer {
    let db = Database::new(db_path.to_str().unwrap()).unwrap();
    assert!(
        wal_of(db_path).exists(),
        "the database should be open while it serves"
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(async move {
        dynoxide::server::serve_with_shutdown(listener, db, async {
            let _ = stopped.await;
        })
        .await
    });
    RunningServer { base, stop, server }
}

#[tokio::test]
async fn shutdown_ends_the_server_while_a_client_holds_a_keep_alive_connection() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("served.db");
    let wal = wal_of(&db_path);
    let RunningServer { base, stop, server } = serve(&db_path).await;

    let client = reqwest::Client::new();
    write_and_keep_connection(&client, &base).await;
    assert!(wal.exists(), "the database should still be open");

    stop.send(()).unwrap();
    ended_ok(
        server,
        "the server should stop with a keep-alive client still attached",
    )
    .await;

    // The client still holds its pooled connection here; the server has
    // closed its end and let go of the database.
    assert!(
        released(&wal).await,
        "the server should have released every clone of the database"
    );
    drop(client);
    std::fs::remove_file(&db_path).unwrap();
}

#[tokio::test]
async fn shutdown_with_no_client_returns_ok_and_releases_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("idle.db");
    let wal = wal_of(&db_path);
    let RunningServer { stop, server, .. } = serve(&db_path).await;

    stop.send(()).unwrap();
    ended_ok(server, "an idle server should stop").await;
    assert!(released(&wal).await);
}

/// A request that is running when the stop signal arrives finishes first.
///
/// `Expect: 100-continue` makes the request provably in flight: the server
/// answers `100 Continue` only once it has read the request head and its
/// handler is waiting for the body, so the stop is sent while the request is
/// mid-way and not before the server has seen it. The body is sent afterwards.
#[tokio::test]
async fn a_request_running_at_shutdown_finishes_before_the_server_ends() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("running.db");
    let wal = wal_of(&db_path);
    let RunningServer { base, stop, server } = serve(&db_path).await;
    let addr = base.trim_start_matches("http://");

    let body = create_table_body();
    let head = format!(
        "POST / HTTP/1.1\r\nHost: {addr}\r\nx-amz-target: {TARGET}\r\n\
         content-type: {CONTENT_TYPE}\r\nx-amz-date: {AMZ_DATE}\r\n\
         authorization: {AUTHORIZATION}\r\n\
         content-length: {}\r\nexpect: 100-continue\r\n\r\n",
        body.len()
    );
    let mut client = tokio::net::TcpStream::connect(addr).await.unwrap();
    client.write_all(head.as_bytes()).await.unwrap();

    // Wait for the server to say it has the head and wants the body.
    let mut seen = Vec::new();
    tokio::time::timeout(STOPS_WITHIN, async {
        let mut chunk = [0u8; 256];
        while !seen.windows(12).any(|w| w == b"100 Continue") {
            let n = client.read(&mut chunk).await.unwrap();
            assert!(n > 0, "the server closed before asking for the body");
            seen.extend_from_slice(&chunk[..n]);
        }
    })
    .await
    .expect("the server should ask for the body of a request it has begun");

    stop.send(()).unwrap();
    // The request is still open, so the server must not end while it is. This
    // wait can only let a wrongly ending server show itself: on a slow machine
    // a pass proves less, but a failure is always real.
    tokio::time::sleep(HELD_OPEN_FOR).await;
    assert!(
        !server.is_finished(),
        "the server ended with a request still running"
    );
    assert!(
        wal.exists(),
        "the database should be held until it finishes"
    );

    client.write_all(body.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(STOPS_WITHIN, client.read_to_end(&mut response))
        .await
        .expect("the running request should be answered, then the connection closed")
        .unwrap();
    let response = String::from_utf8_lossy(&response);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "the running request should finish normally, got: {response}"
    );

    ended_ok(
        server,
        "the server should end once the running request has finished",
    )
    .await;
    assert!(released(&wal).await);
}

#[tokio::test]
async fn a_shutdown_that_has_already_resolved_returns_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("resolved.db");
    let wal = wal_of(&db_path);
    let db = Database::new(db_path.to_str().unwrap()).unwrap();
    assert!(wal.exists(), "the database should be open before it serves");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    let ended = tokio::time::timeout(
        STOPS_WITHIN,
        dynoxide::server::serve_with_shutdown(listener, db, async {}),
    )
    .await
    .expect("a server told to stop at the start should return");
    assert_eq!(ended, Ok(()));
    assert!(released(&wal).await);
}

/// The doc comment says a shutdown future that panics stops the server like
/// any other, and the call still returns `Ok(())`. An embedder has to check
/// its own signal future, because nothing here reports the panic.
#[tokio::test]
async fn a_shutdown_future_that_panics_stops_the_server_and_still_returns_ok() {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("panicked.db");
    let wal = wal_of(&db_path);
    let db = Database::new(db_path.to_str().unwrap()).unwrap();
    assert!(wal.exists(), "the database should be open before it serves");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();

    let ended = tokio::time::timeout(
        STOPS_WITHIN,
        dynoxide::server::serve_with_shutdown(listener, db, async {
            panic!("the signal future failed")
        }),
    )
    .await
    .expect("a server whose shutdown future panics should still return");
    assert_eq!(ended, Ok(()));
    assert!(released(&wal).await);
}
