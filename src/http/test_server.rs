//! A scripted HTTP/1.1 server for tests: every request gets whatever the
//! handler says, and is recorded.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A request as the handler sees it.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub path: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    /// How many requests for this method and path there have been,
    /// this one included.
    pub count: usize,
}

/// What to do with a request.
pub enum Reply {
    /// The status, with an empty body.
    Status(u16),
    /// 200 with this body.
    Body(Vec<u8>),
    /// Never answer.
    Hang,
    /// Hang up without answering.
    Close,
}

type Handler = dyn Fn(&Request) -> Reply + Send + Sync;

pub struct TestServer {
    base: String,
    requests: Arc<Mutex<Vec<Request>>>,
    task: tokio::task::JoinHandle<()>,
}

impl TestServer {
    pub async fn start(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let log = requests.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                tokio::spawn(serve(stream, handler.clone(), log.clone()));
            }
        });
        Self {
            base,
            requests,
            task,
        }
    }

    /// The URL of `path` on this server.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    /// Every request so far, as `METHOD /path`.
    pub fn requests(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| format!("{} {}", r.method, r.path))
            .collect()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Answer one request, and close the connection.
async fn serve(mut stream: TcpStream, handler: Arc<Handler>, log: Arc<Mutex<Vec<Request>>>) {
    let Some(mut request) = read_request(&mut stream).await else {
        return;
    };
    {
        let mut log = log.lock().unwrap();
        request.count = 1 + log
            .iter()
            .filter(|r| r.method == request.method && r.path == request.path)
            .count();
        log.push(request.clone());
    }
    let (status, body) = match handler(&request) {
        Reply::Status(status) => (status, Vec::new()),
        Reply::Body(body) => (200, body),
        Reply::Hang => {
            std::future::pending::<()>().await;
            unreachable!()
        }
        Reply::Close => return,
    };
    let body = if request.method == "HEAD" {
        Vec::new()
    } else {
        body
    };
    let head = format!(
        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&body).await;
    let _ = stream.shutdown().await;
}

async fn read_request(stream: &mut TcpStream) -> Option<Request> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    };
    let head = String::from_utf8(buf[..head_end].to_vec()).ok()?;
    let mut lines = head.split("\r\n");
    let mut start = lines.next()?.split(' ');
    let method = start.next()?.to_string();
    let path = start.next()?.to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(n, v)| (n.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let length: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = buf[head_end + 4..].to_vec();
    while body.len() < length {
        let n = stream.read(&mut chunk).await.ok()?;
        if n == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..n]);
    }
    Some(Request {
        method,
        path,
        headers,
        count: 0,
    })
}
