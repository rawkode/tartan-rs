//! The fetch-safe client ignores proxy settings in the environment (design D9, *Fetch
//! safety*), in production and in development mode.
//!
//! Environment variables are process-wide, so the test runs the requests in a child process
//! (this test binary again) with `HTTP_PROXY`, `HTTPS_PROXY` and `ALL_PROXY` pointing at a
//! listener that counts connections; the listener must see none.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use tartan_atproto::Mode;
use tartan_atproto::fetch::{FetchClient, FetchConfig};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const CHILD: &str = "TARTAN_ATPROTO_PROXY_CHILD";
const TARGET: &str = "TARTAN_ATPROTO_PROXY_TARGET";

#[tokio::test]
async fn connects_directly_with_proxy_variables_set() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.expect("bind proxy");
    let proxy_url = format!("http://{}", proxy.local_addr().expect("addr"));
    let proxied = Arc::new(AtomicUsize::new(0));
    let counter = proxied.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = proxy.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = stream.shutdown().await;
        }
    });

    let target = TcpListener::bind("127.0.0.1:0").await.expect("bind target");
    let target_url = format!("http://{}/", target.local_addr().expect("addr"));
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = target.accept().await {
            let mut buffer = [0u8; 2048];
            let _ = stream.read(&mut buffer).await;
            let _ = stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ndirect",
                )
                .await;
        }
    });

    let exe = std::env::current_exe().expect("test binary");
    let output = tokio::process::Command::new(exe)
        .args([
            "--exact",
            "child_requests",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(CHILD, "1")
        .env(TARGET, &target_url)
        .env("HTTP_PROXY", &proxy_url)
        .env("HTTPS_PROXY", &proxy_url)
        .env("ALL_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("https_proxy", &proxy_url)
        .env("all_proxy", &proxy_url)
        .output()
        .await
        .expect("child");
    assert!(
        output.status.success(),
        "child failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    assert_eq!(
        proxied.load(Ordering::SeqCst),
        0,
        "a request went through the proxy"
    );
}

/// Runs only as the child of the test above.
#[tokio::test]
async fn child_requests() {
    if std::env::var_os(CHILD).is_none() {
        return;
    }
    // Production: a public address is dialled directly (the connection may fail in a sandbox;
    // what matters is that the proxy is never contacted).
    let mut config = FetchConfig::new(Mode::Production);
    config.timeout = Duration::from_millis(1500);
    let production = FetchClient::new(config).expect("client");
    let _ = production.get("https://100.63.255.254/", 1024).await;

    #[cfg(feature = "dev-identity")]
    {
        let target = std::env::var(TARGET).expect("target");
        let development = FetchClient::new(FetchConfig::new(Mode::Development)).expect("client");
        let fetched = development
            .get(&target, 1024)
            .await
            .expect("direct connection");
        assert_eq!(fetched.body, b"direct");
    }
}
