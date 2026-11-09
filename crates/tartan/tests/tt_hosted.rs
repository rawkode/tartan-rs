//! `tt`'s hosted development client and `tt logout` without a forge (design D16, *Forge URLs
//! and the signing client*; spec tt-sign, tt-cli), against the development network.
//!
//! Runs the real `tt` binary as `tests/tt.rs` does (set `TT_BIN` to use one already built).
//! Needs a Rivet engine binary: set `RIVET_ENGINE_BINARY_PATH` (see `scripts/e2e.sh`).

mod common;

use serde_json::Value;

use common::Forge;
use common::tt::{Answer, Machine, text, tt_binary};

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("address")
        .port()
}

/// A hosted development client (`--client-metadata-url`): `tt` prints the document to publish
/// when it cannot load it, signs in with it once it is served, and `tt logout` ends that
/// signing session although no forge is configured.
#[tokio::test(flavor = "multi_thread")]
async fn a_hosted_client_and_logout_without_a_forge() {
    let tt = tt_binary();
    let forge = Forge::start("tt-hosted").await;
    let machine = Machine::new(&tt, &forge);
    let port = free_port();
    let url = format!("http://127.0.0.1:{port}/tt-client.json");
    let login = [
        "sign",
        "--login",
        "--client-metadata-url",
        url.as_str(),
        "--handle",
        "alice.test",
        "--no-open",
    ];

    // Nothing served yet: tt names the document to host.
    let output = machine.run(&login, None);
    assert!(!output.status.success(), "{}", text(&output));
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(stderr.contains("Host exactly this JSON"), "{stderr}");
    let start = stderr.find('{').expect("a JSON document");
    let document: Value = serde_json::Deserializer::from_str(&stderr[start..])
        .into_iter::<Value>()
        .next()
        .expect("a document")
        .expect("JSON");
    assert_eq!(document["client_id"], url.as_str());

    // Served: the sign-in goes through with that client.
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .expect("bind");
    let served = document.clone();
    let app = axum::Router::new().route(
        "/tt-client.json",
        axum::routing::get(move || {
            let served = served.clone();
            async move { axum::Json(served) }
        }),
    );
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    let output = machine
        .login(&login, vec![Answer::Follow { extra: "" }])
        .await;
    assert!(output.status.success(), "{}", text(&output));
    let status = machine.status();
    assert!(status.contains("Signing: "), "{status}");
    assert!(status.contains(&url), "{status}");

    // No forge is configured, and logout still ends the signing session.
    let output = machine.run(&["logout"], None);
    assert!(output.status.success(), "{}", text(&output));
    let printed = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(printed.contains("Revoked the signing session"), "{printed}");
    assert!(
        printed.contains("Local credentials are deleted"),
        "{printed}"
    );
    let credentials = machine.home.path().join("state/tt/credentials");
    let left = std::fs::read_dir(&credentials).map_or(0, |dir| dir.count());
    assert_eq!(left, 0, "no credential is left");
    assert!(
        machine.status().contains("Signing: no signing session"),
        "{}",
        machine.status()
    );
    let output = machine.run(&["logout"], None);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("No credentials were stored"),
        "{}",
        text(&output)
    );
    server.abort();
}
