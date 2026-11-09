//! A minimal HTTP/1.1 server for unit tests: it answers each connection with one scripted
//! reply and records what it was asked.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A request the server received.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// A scripted reply.
#[derive(Debug, Clone)]
pub struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    chunked: bool,
    delay: Option<Duration>,
}

impl Reply {
    pub fn ok(body: Vec<u8>) -> Self {
        Reply {
            status: 200,
            headers: Vec::new(),
            body,
            chunked: false,
            delay: None,
        }
    }

    pub fn json(value: serde_json::Value) -> Self {
        Reply::ok(serde_json::to_vec(&value).expect("json"))
            .header("Content-Type", "application/json")
    }

    pub fn redirect(location: &str) -> Self {
        Reply::ok(Vec::new())
            .status(307)
            .header("Location", location)
    }

    pub fn status(mut self, status: u16) -> Self {
        self.status = status;
        self
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub fn chunked(mut self) -> Self {
        self.chunked = true;
        self
    }

    pub fn delay(mut self, delay: Duration) -> Self {
        self.delay = Some(delay);
        self
    }
}

type Handler = Arc<dyn Fn(&Request) -> Reply + Send + Sync>;

/// The server; it stops when dropped.
pub struct TestServer {
    pub addr: SocketAddr,
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl TestServer {
    /// Answers connections with `replies` in order, then with 404.
    pub async fn start(replies: Vec<Reply>) -> Self {
        let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
        Self::with_handler(move |_| {
            queue
                .lock()
                .expect("queue")
                .pop_front()
                .unwrap_or_else(|| Reply::ok(Vec::new()).status(404))
        })
        .await
    }

    /// Answers every request with `handler`.
    pub async fn with_handler(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Handler = Arc::new(handler);
        let recorded = requests.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    return;
                };
                let handler = handler.clone();
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let Some(request) = read_request(&mut stream).await else {
                        return;
                    };
                    recorded.lock().expect("requests").push(request.clone());
                    let reply = handler(&request);
                    if let Some(delay) = reply.delay {
                        tokio::time::sleep(delay).await;
                    }
                    let _ = stream.write_all(&encode(&reply)).await;
                    let _ = stream.shutdown().await;
                });
            }
        });
        TestServer {
            addr,
            requests,
            task,
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("requests").clone()
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Option<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let mut lines = head.split("\r\n");
    let mut first = lines.next()?.split(' ');
    let method = first.next()?.to_owned();
    let path = first.next()?.to_owned();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = buffer[header_end..].to_vec();
    while body.len() < length {
        let read = stream.read(&mut chunk).await.ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

fn encode(reply: &Reply) -> Vec<u8> {
    let mut out = format!("HTTP/1.1 {} X\r\nConnection: close\r\n", reply.status).into_bytes();
    for (name, value) in &reply.headers {
        out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
    }
    if reply.chunked {
        out.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n");
        for piece in reply.body.chunks(256) {
            out.extend_from_slice(format!("{:x}\r\n", piece.len()).as_bytes());
            out.extend_from_slice(piece);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(b"0\r\n\r\n");
    } else {
        out.extend_from_slice(format!("Content-Length: {}\r\n\r\n", reply.body.len()).as_bytes());
        out.extend_from_slice(&reply.body);
    }
    out
}
