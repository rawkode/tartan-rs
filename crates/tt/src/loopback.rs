//! The loopback listener both browser hand-offs end on (design D16, *Forge login hand-off*, and
//! D7, *tt's OAuth client*): `tt` binds `127.0.0.1` on a free port, or `--port` so the port can
//! be forwarded over SSH, and waits for one `GET` on the path it expects.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use reqwest::Url;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

/// How long `tt` waits for the browser.
pub const WAIT: Duration = Duration::from_secs(10 * 60);

/// A bound loopback listener.
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    /// Binds `127.0.0.1:<port>` (`0` or `None` for any free port).
    pub async fn bind(port: Option<u16>) -> Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port.unwrap_or(0)))
            .await
            .with_context(|| match port {
                Some(port) => format!("listening on 127.0.0.1:{port} (is the port in use?)"),
                None => "listening on a loopback port".to_owned(),
            })?;
        let port = listener.local_addr().context("the loopback port")?.port();
        Ok(Loopback { listener, port })
    }

    /// The port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Waits for `GET <path>?…` and answers it with `page`; returns the query parameters.
    /// Requests for other paths (a browser's favicon, for example) get 404.
    pub async fn wait(&self, path: &str, page: &str) -> Result<HashMap<String, String>> {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let accepted = tokio::time::timeout_at(deadline, self.listener.accept()).await;
            let (stream, _) = match accepted {
                Ok(Ok(accepted)) => accepted,
                Ok(Err(error)) => return Err(error).context("accepting the browser's redirect"),
                Err(_) => bail!("no answer from the browser within ten minutes"),
            };
            if let Some(query) = serve(stream, path, page).await {
                return Ok(query);
            }
        }
    }
}

async fn serve(mut stream: TcpStream, path: &str, page: &str) -> Option<HashMap<String, String>> {
    let mut buffer = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let head = loop {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break String::from_utf8_lossy(&buffer[..end]).into_owned();
        }
        if buffer.len() > 16 * 1024 {
            return None;
        }
    };
    let target = head
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("GET "))
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_default()
        .to_owned();
    let url = Url::parse(&format!("http://127.0.0.1{target}")).ok();
    let matched = url.as_ref().is_some_and(|url| url.path() == path);
    let (status, body) = if matched {
        ("200 OK", page.to_owned())
    } else {
        ("404 Not Found", "Not found".to_owned())
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nCache-Control: no-store\r\n\
         Connection: close\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    if !matched {
        return None;
    }
    url.map(|url| url.query_pairs().into_owned().collect())
}

/// The page the browser shows once `tt` has its answer.
pub fn page(message: &str) -> String {
    let escaped = message
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>tt</title></head>\
         <body style=\"font-family: sans-serif; margin: 3rem\"><p>{escaped}</p>\
         <p>You can close this window and return to your terminal.</p></body></html>"
    )
}

/// Shows `url` to the person: opens it in a browser unless `no_open`, and always prints it.
pub fn present(url: &str, what: &str, no_open: bool) {
    if no_open {
        eprintln!("Open this URL in a browser to {what}:\n\n  {url}\n");
        return;
    }
    eprintln!("Opening your browser to {what}. If it does not open, visit:\n\n  {url}\n");
    if webbrowser::open(url).is_err() {
        eprintln!("(the browser could not be opened)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_the_expected_path_only() {
        let loopback = Loopback::bind(None).await.expect("bind");
        let port = loopback.port();
        let client = reqwest::Client::builder()
            .no_proxy()
            .build()
            .expect("client");
        let waiting = tokio::spawn(async move { loopback.wait("/forge", &page("done")).await });
        let missing = client
            .get(format!("http://127.0.0.1:{port}/favicon.ico"))
            .send()
            .await
            .expect("send");
        assert_eq!(missing.status(), 404);
        let hit = client
            .get(format!(
                "http://127.0.0.1:{port}/forge?code=abc&state=s%20t"
            ))
            .send()
            .await
            .expect("send");
        assert_eq!(hit.status(), 200);
        let query = waiting.await.expect("join").expect("query");
        assert_eq!(query.get("code").map(String::as_str), Some("abc"));
        assert_eq!(query.get("state").map(String::as_str), Some("s t"));
    }
}
