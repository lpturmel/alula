use std::{
    fmt::Write as _,
    io,
    net::TcpStream,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow};
use reqwest::cookie::{CookieStore as _, Jar};
use tungstenite::{
    ClientRequestBuilder, Error as WebSocketError, Message, connect, http::Uri,
    stream::MaybeTlsStream,
};

use crate::{RequestDraft, ResponseSnapshot};

const READ_TIMEOUT: Duration = Duration::from_millis(100);
const MAX_MESSAGE_PREVIEW_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSocketDirection {
    Sent,
    Received,
}

impl WebSocketDirection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sent => "Sent",
            Self::Received => "Received",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebSocketMessageKind {
    Text,
    Binary,
    Ping,
    Pong,
    Close,
}

impl WebSocketMessageKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Binary => "Binary",
            Self::Ping => "Ping",
            Self::Pong => "Pong",
            Self::Close => "Close",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSocketMessageSnapshot {
    pub sequence: u64,
    pub direction: WebSocketDirection,
    pub kind: WebSocketMessageKind,
    pub elapsed_ms: u128,
    pub size_bytes: usize,
    pub body: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebSocketStreamEvent {
    Started(ResponseSnapshot),
    Message(WebSocketMessageSnapshot),
    Closed {
        elapsed_ms: u128,
        total_bytes: usize,
        stopped: bool,
        detail: Option<String>,
    },
}

pub struct WebSocketExecutor;

pub fn is_websocket_request(request: &RequestDraft) -> bool {
    websocket_scheme(&request.url).is_some()
        || request.headers.iter().any(|header| {
            header.enabled
                && header.key.trim().eq_ignore_ascii_case("upgrade")
                && header.value.trim().eq_ignore_ascii_case("websocket")
        })
}

fn websocket_scheme(url: &str) -> Option<&'static str> {
    let scheme = url.trim().split_once(':')?.0;
    if scheme.eq_ignore_ascii_case("ws") {
        Some("ws")
    } else if scheme.eq_ignore_ascii_case("wss") {
        Some("wss")
    } else {
        None
    }
}

fn websocket_url(request: &RequestDraft) -> Result<url::Url> {
    let mut url = request.resolved_url().map_err(|error| anyhow!(error))?;
    match url.scheme() {
        "ws" | "wss" => {}
        "http" => {
            url.set_scheme("ws")
                .map_err(|_| anyhow!("could not convert HTTP URL to WebSocket URL"))?;
        }
        "https" => {
            url.set_scheme("wss")
                .map_err(|_| anyhow!("could not convert HTTPS URL to WebSocket URL"))?;
        }
        scheme => return Err(anyhow!("unsupported WebSocket URL scheme `{scheme}`")),
    }
    Ok(url)
}

fn cookie_url(mut url: url::Url) -> Result<url::Url> {
    let cookie_scheme = match url.scheme() {
        "ws" => "http",
        "wss" => "https",
        "http" | "https" => return Ok(url),
        scheme => return Err(anyhow!("unsupported cookie URL scheme `{scheme}`")),
    };
    url.set_scheme(cookie_scheme)
        .map_err(|_| anyhow!("could not convert WebSocket URL for cookie matching"))?;
    Ok(url)
}

fn managed_handshake_header(name: &str) -> bool {
    [
        "connection",
        "host",
        "upgrade",
        "sec-websocket-key",
        "sec-websocket-version",
    ]
    .iter()
    .any(|managed| name.eq_ignore_ascii_case(managed))
}

fn set_read_timeout(stream: &mut MaybeTlsStream<TcpStream>) -> io::Result<()> {
    match stream {
        MaybeTlsStream::Plain(stream) => stream.set_read_timeout(Some(READ_TIMEOUT)),
        MaybeTlsStream::Rustls(stream) => stream.sock.set_read_timeout(Some(READ_TIMEOUT)),
        _ => Ok(()),
    }
}

fn text_preview(text: &str) -> (String, bool) {
    if text.len() <= MAX_MESSAGE_PREVIEW_BYTES {
        return (text.to_owned(), false);
    }
    let mut end = MAX_MESSAGE_PREVIEW_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut preview = text[..end].to_owned();
    preview.push_str("\n\n… message preview truncated");
    (preview, true)
}

fn binary_preview(bytes: &[u8]) -> (String, bool) {
    if let Ok(text) = std::str::from_utf8(bytes) {
        return text_preview(text);
    }
    let truncated = bytes.len() > MAX_MESSAGE_PREVIEW_BYTES;
    let bytes = &bytes[..bytes.len().min(MAX_MESSAGE_PREVIEW_BYTES)];
    let mut preview = String::with_capacity(bytes.len().saturating_mul(3));
    for (line, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(preview, "{:08x}  ", line * 16);
        for byte in chunk {
            let _ = write!(preview, "{byte:02x} ");
        }
        if line + 1 < bytes.len().div_ceil(16) {
            preview.push('\n');
        }
    }
    if truncated {
        preview.push_str("\n\n… binary preview truncated");
    }
    (preview, truncated)
}

fn message_snapshot(
    sequence: u64,
    direction: WebSocketDirection,
    message: Message,
    elapsed_ms: u128,
) -> WebSocketMessageSnapshot {
    let size_bytes = message.len();
    let (kind, body, truncated) = match message {
        Message::Text(text) => {
            let (body, truncated) = text_preview(text.as_str());
            (WebSocketMessageKind::Text, body, truncated)
        }
        Message::Binary(bytes) => {
            let (body, truncated) = binary_preview(&bytes);
            (WebSocketMessageKind::Binary, body, truncated)
        }
        Message::Ping(bytes) => {
            let (body, truncated) = binary_preview(&bytes);
            (WebSocketMessageKind::Ping, body, truncated)
        }
        Message::Pong(bytes) => {
            let (body, truncated) = binary_preview(&bytes);
            (WebSocketMessageKind::Pong, body, truncated)
        }
        Message::Close(frame) => {
            let body = frame
                .map(|frame| {
                    format!("{:?} {}", frame.code, frame.reason)
                        .trim()
                        .to_owned()
                })
                .unwrap_or_else(|| "Connection closed without a close frame".into());
            (WebSocketMessageKind::Close, body, false)
        }
        Message::Frame(_) => unreachable!("raw WebSocket frames are not returned by read"),
    };
    WebSocketMessageSnapshot {
        sequence,
        direction,
        kind,
        elapsed_ms,
        size_bytes,
        body,
        truncated,
    }
}

impl WebSocketExecutor {
    pub fn execute_streaming(
        request: &RequestDraft,
        stop: Arc<AtomicBool>,
        on_event: impl FnMut(WebSocketStreamEvent),
    ) -> Result<ResponseSnapshot> {
        Self::execute_streaming_with_cookies(request, stop, None, on_event)
    }

    pub fn execute_streaming_with_cookie_jar(
        request: &RequestDraft,
        stop: Arc<AtomicBool>,
        cookie_jar: Arc<Jar>,
        on_event: impl FnMut(WebSocketStreamEvent),
    ) -> Result<ResponseSnapshot> {
        Self::execute_streaming_with_cookies(request, stop, Some(cookie_jar), on_event)
    }

    fn execute_streaming_with_cookies(
        request: &RequestDraft,
        stop: Arc<AtomicBool>,
        cookie_jar: Option<Arc<Jar>>,
        mut on_event: impl FnMut(WebSocketStreamEvent),
    ) -> Result<ResponseSnapshot> {
        let url = websocket_url(request)?;
        let cookie_url = cookie_url(url.clone())?;
        let uri: Uri = url
            .as_str()
            .parse()
            .context("invalid WebSocket endpoint URL")?;
        let mut handshake = ClientRequestBuilder::new(uri);
        for header in request
            .headers
            .iter()
            .filter(|header| header.enabled && !header.key.trim().is_empty())
            .filter(|header| !managed_handshake_header(header.key.trim()))
        {
            handshake = handshake.with_header(header.key.trim(), header.value.as_str());
        }
        let has_explicit_cookie = request
            .headers
            .iter()
            .any(|header| header.enabled && header.key.trim().eq_ignore_ascii_case("cookie"));
        if !has_explicit_cookie
            && let Some(cookie) = cookie_jar
                .as_ref()
                .and_then(|cookie_jar| cookie_jar.cookies(&cookie_url))
        {
            handshake = handshake.with_header(
                "cookie",
                cookie
                    .to_str()
                    .context("session cookie is not valid header text")?,
            );
        }

        let started = Instant::now();
        let (mut socket, handshake_response) =
            connect(handshake).context("WebSocket handshake failed")?;
        if let Some(cookie_jar) = cookie_jar.as_ref() {
            for cookie in handshake_response.headers().get_all("set-cookie") {
                if let Ok(cookie) = cookie.to_str() {
                    cookie_jar.add_cookie_str(cookie, &cookie_url);
                }
            }
        }
        set_read_timeout(socket.get_mut()).context("could not configure WebSocket stream")?;

        let headers_elapsed_ms = started.elapsed().as_millis();
        let status = handshake_response.status();
        let headers = handshake_response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    value.to_str().unwrap_or("<binary>").to_owned(),
                )
            })
            .collect::<Vec<_>>();
        let mut response = ResponseSnapshot {
            status: status.as_u16(),
            status_text: status
                .canonical_reason()
                .unwrap_or("Switching Protocols")
                .into(),
            elapsed_ms: headers_elapsed_ms,
            size_bytes: 0,
            headers,
            body: String::new(),
            content_type: None,
        };
        on_event(WebSocketStreamEvent::Started(response.clone()));

        let mut sequence = 0_u64;
        let mut total_bytes = 0_usize;
        if !request.body.is_empty() {
            let message = Message::text(request.body.clone());
            total_bytes = total_bytes.saturating_add(message.len());
            socket
                .send(message.clone())
                .context("failed to send initial WebSocket message")?;
            on_event(WebSocketStreamEvent::Message(message_snapshot(
                sequence,
                WebSocketDirection::Sent,
                message,
                started.elapsed().as_millis(),
            )));
            sequence = sequence.wrapping_add(1);
        }

        let mut stopped = false;
        let mut close_detail = None;
        loop {
            if stop.load(Ordering::Acquire) {
                stopped = true;
                close_detail = Some("Stopped by user".into());
                let _ = socket.close(None);
                break;
            }
            match socket.read() {
                Ok(message) => {
                    total_bytes = total_bytes.saturating_add(message.len());
                    let is_close = message.is_close();
                    let snapshot = message_snapshot(
                        sequence,
                        WebSocketDirection::Received,
                        message,
                        started.elapsed().as_millis(),
                    );
                    if is_close {
                        close_detail = Some(snapshot.body.clone());
                    }
                    on_event(WebSocketStreamEvent::Message(snapshot));
                    sequence = sequence.wrapping_add(1);
                    if is_close {
                        let _ = socket.flush();
                        break;
                    }
                }
                Err(WebSocketError::Io(error))
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) => {}
                Err(WebSocketError::ConnectionClosed | WebSocketError::AlreadyClosed) => break,
                Err(error) => return Err(error).context("WebSocket stream failed"),
            }
        }

        response.elapsed_ms = started.elapsed().as_millis();
        response.size_bytes = total_bytes;
        on_event(WebSocketStreamEvent::Closed {
            elapsed_ms: response.elapsed_ms,
            total_bytes,
            stopped,
            detail: close_detail,
        });
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{atomic::AtomicBool, mpsc},
        thread,
    };

    use tungstenite::{
        Message, accept, accept_hdr,
        handshake::server::{Request, Response},
    };

    use super::*;
    use crate::{HttpSession, KeyValueField};

    #[test]
    fn detects_websocket_protocols_and_explicit_upgrades() {
        let direct = RequestDraft {
            url: "wss://example.com/socket".into(),
            ..RequestDraft::default()
        };
        assert!(is_websocket_request(&direct));

        let upgraded = RequestDraft {
            url: "https://example.com/socket".into(),
            headers: vec![KeyValueField::new("Upgrade", "websocket")],
            ..RequestDraft::default()
        };
        assert!(is_websocket_request(&upgraded));
        assert!(!is_websocket_request(&RequestDraft::default()));
    }

    #[test]
    fn streams_individual_messages_and_stops_cleanly() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = accept(stream).unwrap();
            assert_eq!(
                socket.read().unwrap(),
                Message::text(r#"{"subscribe":true}"#)
            );
            socket.send(Message::text(r#"{"value":1}"#)).unwrap();
            socket.send(Message::binary(vec![0_u8, 1, 2, 255])).unwrap();
            while socket.read().is_ok() {}
        });
        let request = RequestDraft {
            url: format!("http://{address}/events"),
            headers: vec![KeyValueField::new("Upgrade", "websocket")],
            body: r#"{"subscribe":true}"#.into(),
            ..RequestDraft::default()
        };
        let stop = Arc::new(AtomicBool::new(false));
        let event_stop = stop.clone();
        let mut events = Vec::new();
        let response = WebSocketExecutor::execute_streaming(&request, stop, |event| {
            let received_messages = events
                .iter()
                .filter(|event| {
                    matches!(
                        event,
                        WebSocketStreamEvent::Message(WebSocketMessageSnapshot {
                            direction: WebSocketDirection::Received,
                            ..
                        })
                    )
                })
                .count();
            events.push(event);
            if received_messages >= 1 {
                event_stop.store(true, Ordering::Release);
            }
        })
        .unwrap();
        server.join().unwrap();

        assert_eq!(response.status, 101);
        assert!(matches!(
            events.first(),
            Some(WebSocketStreamEvent::Started(_))
        ));
        assert!(events.iter().any(|event| matches!(
            event,
            WebSocketStreamEvent::Message(WebSocketMessageSnapshot {
                direction: WebSocketDirection::Sent,
                kind: WebSocketMessageKind::Text,
                ..
            })
        )));
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, WebSocketStreamEvent::Message(_)))
                .count(),
            3
        );
        assert!(matches!(
            events.last(),
            Some(WebSocketStreamEvent::Closed { stopped: true, .. })
        ));
    }

    #[test]
    #[allow(clippy::result_large_err)]
    fn websocket_handshake_uses_matching_http_session_cookie() {
        let login_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let login_address = login_listener.local_addr().unwrap();
        let login_server = thread::spawn(move || {
            let (mut stream, _) = login_listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nSet-Cookie: session=socket-auth; Path=/socket; HttpOnly\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                )
                .unwrap();
        });

        let session = HttpSession::new();
        session
            .execute(&RequestDraft {
                url: format!("http://{login_address}/login"),
                ..RequestDraft::default()
            })
            .unwrap();
        login_server.join().unwrap();

        let websocket_listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let websocket_address = websocket_listener.local_addr().unwrap();
        let (cookie_tx, cookie_rx) = mpsc::channel();
        let websocket_server = thread::spawn(move || {
            let (stream, _) = websocket_listener.accept().unwrap();
            let callback = move |request: &Request, response: Response| {
                cookie_tx
                    .send(
                        request
                            .headers()
                            .get("cookie")
                            .and_then(|value| value.to_str().ok())
                            .unwrap_or_default()
                            .to_owned(),
                    )
                    .unwrap();
                Ok(response)
            };
            let mut socket = accept_hdr(stream, callback).unwrap();
            socket.close(None).unwrap();
        });

        WebSocketExecutor::execute_streaming_with_cookie_jar(
            &RequestDraft {
                url: format!("ws://{websocket_address}/socket"),
                ..RequestDraft::default()
            },
            Arc::new(AtomicBool::new(false)),
            session.cookie_jar(),
            |_| {},
        )
        .unwrap();
        websocket_server.join().unwrap();

        assert_eq!(cookie_rx.recv().unwrap(), "session=socket-auth");
    }
}
