use std::{
    collections::BTreeMap,
    io::{self, Read as _, Write as _},
    net::{IpAddr, SocketAddr, TcpListener, TcpStream},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde_json::{Value, json};
use url::Url;

use crate::{McpServer, McpToolHandler, SUPPORTED_PROTOCOL_VERSIONS};

const MAX_HEADER_BYTES: usize = 64 * 1024;
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

pub struct McpHttpServer {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl McpHttpServer {
    pub fn start(
        port: u16,
        config_path: PathBuf,
        tool_handler: Option<McpToolHandler>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", port))?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let mut server = McpServer::new(config_path);
        if let Some(tool_handler) = tool_handler {
            server = server.with_tool_handler(tool_handler);
        }
        let server = Arc::new(Mutex::new(server));
        let thread = thread::Builder::new()
            .name("alula-mcp-http".into())
            .spawn(move || serve(listener, address.port(), server, thread_stop))?;
        Ok(Self {
            address,
            stop,
            thread: Some(thread),
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.address
    }

    pub fn endpoint(&self) -> String {
        format!("http://{}/mcp", self.address)
    }
}

impl Drop for McpHttpServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(listener: TcpListener, port: u16, server: Arc<Mutex<McpServer>>, stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((stream, _)) => handle_connection(stream, port, &server),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                eprintln!("Alula MCP HTTP accept error: {error}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

fn handle_connection(mut stream: TcpStream, port: u16, server: &Arc<Mutex<McpServer>>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let response = match read_request(&mut stream) {
        Ok(request) => route_request(request, port, server),
        Err(error) => HttpResponse::json(
            400,
            "Bad Request",
            &json!({ "error": format!("invalid HTTP request: {error}") }),
        ),
    };
    let _ = response.write_to(&mut stream);
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: Vec<u8>,
}

fn read_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut bytes = Vec::with_capacity(4096);
    let mut buffer = [0_u8; 4096];
    let header_end = loop {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before headers",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
        if bytes.len() > MAX_HEADER_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "headers are too large",
            ));
        }
        if let Some(index) = find_bytes(&bytes, b"\r\n\r\n") {
            break index + 4;
        }
    };

    let header_text = std::str::from_utf8(&bytes[..header_end - 4])
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "headers are not UTF-8"))?;
    let mut lines = header_text.split("\r\n");
    let mut request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?
        .split_whitespace();
    let method = request_line.next().unwrap_or_default().to_ascii_uppercase();
    let path = request_line.next().unwrap_or_default().to_owned();
    let version = request_line.next().unwrap_or_default();
    if method.is_empty() || path.is_empty() || !version.starts_with("HTTP/1.") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid request line",
        ));
    }

    let mut headers = BTreeMap::new();
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid header"));
        };
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = headers
        .get("content-length")
        .map(|value| value.parse::<usize>())
        .transpose()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid content length"))?
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request body is too large",
        ));
    }
    while bytes.len() - header_end < content_length {
        let read = stream.read(&mut buffer)?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before request body",
            ));
        }
        bytes.extend_from_slice(&buffer[..read]);
    }
    Ok(HttpRequest {
        method,
        path,
        headers,
        body: bytes[header_end..header_end + content_length].to_vec(),
    })
}

fn route_request(request: HttpRequest, port: u16, server: &Arc<Mutex<McpServer>>) -> HttpResponse {
    if !origin_allowed(request.headers.get("origin"), port) {
        return HttpResponse::json(
            403,
            "Forbidden",
            &json!({ "error": "Origin is not allowed" }),
        );
    }
    if request.path == "/health" && request.method == "GET" {
        return HttpResponse::json(
            200,
            "OK",
            &json!({
                "status": "ready",
                "transport": "streamable-http",
                "endpoint": "/mcp",
                "port": port
            }),
        );
    }
    if request.path != "/mcp" {
        return HttpResponse::empty(404, "Not Found");
    }
    if request.method == "GET" || request.method == "DELETE" {
        return HttpResponse::empty(405, "Method Not Allowed").header("Allow", "POST");
    }
    if request.method != "POST" {
        return HttpResponse::empty(405, "Method Not Allowed").header("Allow", "POST");
    }
    let content_type = request
        .headers
        .get("content-type")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if !content_type.starts_with("application/json") {
        return HttpResponse::empty(415, "Unsupported Media Type");
    }
    let accept = request
        .headers
        .get("accept")
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_default();
    if !accept.contains("application/json") || !accept.contains("text/event-stream") {
        return HttpResponse::empty(406, "Not Acceptable");
    }
    let message: Value = match serde_json::from_slice(&request.body) {
        Ok(Value::Object(message)) => Value::Object(message),
        Ok(_) => {
            return HttpResponse::json(
                400,
                "Bad Request",
                &json!({ "error": "MCP body must be one JSON-RPC object" }),
            );
        }
        Err(error) => {
            return HttpResponse::json(
                400,
                "Bad Request",
                &json!({ "error": format!("invalid JSON: {error}") }),
            );
        }
    };
    let method = message.get("method").and_then(Value::as_str);
    if method != Some("initialize")
        && let Some(version) = request.headers.get("mcp-protocol-version")
        && !SUPPORTED_PROTOCOL_VERSIONS.contains(&version.as_str())
    {
        return HttpResponse::json(
            400,
            "Bad Request",
            &json!({
                "error": "unsupported MCP protocol version",
                "supported": SUPPORTED_PROTOCOL_VERSIONS
            }),
        );
    }
    if method.is_none() || message.get("id").is_none() {
        if let Ok(mut server) = server.lock() {
            let _ = server.handle(&message.to_string());
        }
        return HttpResponse::empty(202, "Accepted");
    }
    let response = match server.lock() {
        Ok(mut server) => server.handle(&message.to_string()),
        Err(_) => None,
    };
    match response {
        Some(response) => HttpResponse::new(200, "OK", "application/json", response.into_bytes()),
        None => HttpResponse::empty(500, "Internal Server Error"),
    }
}

fn origin_allowed(origin: Option<&String>, port: u16) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    let Ok(url) = Url::parse(origin) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let is_loopback = host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    is_loopback && url.port().is_none_or(|origin_port| origin_port == port)
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

struct HttpResponse {
    status: u16,
    reason: &'static str,
    content_type: Option<&'static str>,
    headers: Vec<(&'static str, &'static str)>,
    body: Vec<u8>,
}

impl HttpResponse {
    fn new(status: u16, reason: &'static str, content_type: &'static str, body: Vec<u8>) -> Self {
        Self {
            status,
            reason,
            content_type: Some(content_type),
            headers: Vec::new(),
            body,
        }
    }

    fn json(status: u16, reason: &'static str, body: &Value) -> Self {
        Self::new(
            status,
            reason,
            "application/json",
            serde_json::to_vec(body).unwrap_or_default(),
        )
    }

    fn empty(status: u16, reason: &'static str) -> Self {
        Self {
            status,
            reason,
            content_type: None,
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    fn header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }

    fn write_to(self, stream: &mut TcpStream) -> io::Result<()> {
        write!(stream, "HTTP/1.1 {} {}\r\n", self.status, self.reason)?;
        write!(stream, "Content-Length: {}\r\n", self.body.len())?;
        write!(stream, "Connection: close\r\nCache-Control: no-store\r\n")?;
        if let Some(content_type) = self.content_type {
            write!(stream, "Content-Type: {content_type}; charset=utf-8\r\n")?;
        }
        for (name, value) in self.headers {
            write!(stream, "{name}: {value}\r\n")?;
        }
        write!(stream, "\r\n")?;
        stream.write_all(&self.body)?;
        stream.flush()
    }
}
