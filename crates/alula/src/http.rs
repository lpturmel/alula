use std::{
    io::Read,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use reqwest::Method;
use reqwest::blocking::Client;
use reqwest::cookie::Jar;
use reqwest::header::{HeaderName, HeaderValue};

use crate::{
    install_tls_crypto_provider,
    model::{HttpMethod, RequestDraft, ResponseSnapshot},
};

pub struct HttpExecutor;

/// Application-scoped HTTP state. The client and cookie jar are deliberately
/// reused so login responses can authenticate later requests without copying
/// cookies into drafts, history, or persisted workspace files.
#[derive(Clone)]
pub struct HttpSession {
    client: Arc<Mutex<Option<Client>>>,
    cookie_jar: Arc<Jar>,
}

// Keep the first paint cheap enough to syntax-highlight synchronously. Later
// reads are still coalesced before they reach the UI.
const FIRST_READ_BUFFER_BYTES: usize = 2 * 1024;
const READ_BUFFER_BYTES: usize = 64 * 1024;
const STREAM_UPDATE_BYTES: usize = 64 * 1024;
const STREAM_UPDATE_INTERVAL: Duration = Duration::from_millis(33);
const MAX_PREALLOCATED_RESPONSE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpStreamEvent {
    Started(ResponseSnapshot),
    BodyChunk {
        text: String,
        total_bytes: usize,
    },
    Completed {
        elapsed_ms: u128,
        total_bytes: usize,
    },
}

#[derive(Default)]
struct Utf8LossyDecoder {
    pending: Vec<u8>,
}

impl Utf8LossyDecoder {
    fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let mut output = String::with_capacity(self.pending.len());
        let mut consumed = 0;

        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(valid) => {
                    output.push_str(valid);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let valid_end = consumed + error.valid_up_to();
                    if valid_end > consumed {
                        let valid = std::str::from_utf8(&self.pending[consumed..valid_end])
                            .expect("valid_up_to returned a valid UTF-8 prefix");
                        output.push_str(valid);
                    }
                    consumed = valid_end;
                    let Some(invalid_len) = error.error_len() else {
                        break;
                    };
                    output.push('\u{FFFD}');
                    consumed += invalid_len;
                }
            }
        }

        if consumed == self.pending.len() {
            self.pending.clear();
        } else if consumed > 0 {
            self.pending.drain(..consumed);
        }
        output
    }

    fn finish(&mut self) -> String {
        let output = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        output
    }
}

impl HttpExecutor {
    pub fn execute(request: &RequestDraft) -> Result<ResponseSnapshot> {
        HttpSession::new().execute(request)
    }

    pub fn execute_streaming(
        request: &RequestDraft,
        on_event: impl FnMut(HttpStreamEvent),
    ) -> Result<ResponseSnapshot> {
        HttpSession::new().execute_streaming(request, on_event)
    }
}

impl HttpSession {
    pub fn new() -> Self {
        install_tls_crypto_provider();
        Self {
            client: Arc::new(Mutex::new(None)),
            cookie_jar: Arc::new(Jar::default()),
        }
    }

    fn client(&self) -> Result<Client> {
        let mut client = self
            .client
            .lock()
            .map_err(|_| anyhow!("HTTP session client lock was poisoned"))?;
        if let Some(client) = client.as_ref() {
            return Ok(client.clone());
        }
        let initialized = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(60))
            .user_agent("Alula/0.1")
            .cookie_provider(self.cookie_jar.clone())
            .build()
            .context("failed to build HTTP client")?;
        *client = Some(initialized.clone());
        Ok(initialized)
    }

    pub fn cookie_jar(&self) -> Arc<Jar> {
        self.cookie_jar.clone()
    }

    pub fn execute(&self, request: &RequestDraft) -> Result<ResponseSnapshot> {
        self.execute_streaming(request, |_| {})
    }

    pub fn execute_streaming(
        &self,
        request: &RequestDraft,
        mut on_event: impl FnMut(HttpStreamEvent),
    ) -> Result<ResponseSnapshot> {
        let url = request.resolved_url().map_err(|error| anyhow!(error))?;
        let method = match request.method {
            HttpMethod::Get => Method::GET,
            HttpMethod::Post => Method::POST,
            HttpMethod::Put => Method::PUT,
            HttpMethod::Patch => Method::PATCH,
            HttpMethod::Delete => Method::DELETE,
            HttpMethod::Head => Method::HEAD,
            HttpMethod::Options => Method::OPTIONS,
        };

        let mut builder = self.client()?.request(method, url);
        for header in request
            .headers
            .iter()
            .filter(|item| item.enabled && !item.key.trim().is_empty())
        {
            let name = HeaderName::from_bytes(header.key.trim().as_bytes())
                .with_context(|| format!("invalid header name: {}", header.key))?;
            let value = HeaderValue::from_str(&header.value)
                .with_context(|| format!("invalid value for header {}", header.key))?;
            builder = builder.header(name, value);
        }

        if !request.body.is_empty()
            && matches!(
                request.method,
                HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch | HttpMethod::Delete
            )
        {
            builder = builder.body(request.body.clone());
        }

        let started = Instant::now();
        let mut response = builder.send().context("request failed")?;
        let headers_elapsed_ms = started.elapsed().as_millis();
        let status = response.status();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(key, value)| {
                (
                    key.as_str().to_owned(),
                    value.to_str().unwrap_or("<binary>").to_owned(),
                )
            })
            .collect();

        on_event(HttpStreamEvent::Started(ResponseSnapshot {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("Unknown").to_owned(),
            elapsed_ms: headers_elapsed_ms,
            size_bytes: 0,
            headers: headers.clone(),
            body: String::new(),
            content_type: content_type.clone(),
        }));

        let mut decoder = Utf8LossyDecoder::default();
        let mut read_buffer = [0_u8; READ_BUFFER_BYTES];
        let expected_body_bytes = response
            .content_length()
            .and_then(|length| usize::try_from(length).ok())
            .unwrap_or_default()
            .min(MAX_PREALLOCATED_RESPONSE_BYTES);
        let mut body = String::with_capacity(expected_body_bytes);
        let mut pending_update =
            String::with_capacity(expected_body_bytes.min(STREAM_UPDATE_BYTES));
        let mut size_bytes = 0;
        let mut published_first_chunk = false;
        let mut last_update = Instant::now();

        loop {
            let read_capacity = if published_first_chunk {
                READ_BUFFER_BYTES
            } else {
                FIRST_READ_BUFFER_BYTES
            };
            let read = response
                .read(&mut read_buffer[..read_capacity])
                .context("failed to read response body")?;
            if read == 0 {
                break;
            }
            size_bytes += read;
            let decoded = decoder.push(&read_buffer[..read]);
            body.push_str(&decoded);
            pending_update.push_str(&decoded);

            if !pending_update.is_empty()
                && (!published_first_chunk
                    || pending_update.len() >= STREAM_UPDATE_BYTES
                    || last_update.elapsed() >= STREAM_UPDATE_INTERVAL)
            {
                on_event(HttpStreamEvent::BodyChunk {
                    text: std::mem::take(&mut pending_update),
                    total_bytes: size_bytes,
                });
                published_first_chunk = true;
                last_update = Instant::now();
            }
        }

        let tail = decoder.finish();
        body.push_str(&tail);
        pending_update.push_str(&tail);
        if !pending_update.is_empty() {
            on_event(HttpStreamEvent::BodyChunk {
                text: pending_update,
                total_bytes: size_bytes,
            });
        }

        let elapsed_ms = started.elapsed().as_millis();
        on_event(HttpStreamEvent::Completed {
            elapsed_ms,
            total_bytes: size_bytes,
        });

        Ok(ResponseSnapshot {
            status: status.as_u16(),
            status_text: status.canonical_reason().unwrap_or("Unknown").to_owned(),
            elapsed_ms,
            size_bytes,
            headers,
            body,
            content_type,
        })
    }
}

impl Default for HttpSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    use super::*;
    use crate::model::KeyValueField;

    #[test]
    fn sends_headers_parameters_and_body_and_captures_response_metadata() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (captured_tx, captured_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buffer = [0_u8; 8192];
            let read = stream.read(&mut buffer).unwrap();
            captured_tx.send(buffer[..read].to_vec()).unwrap();

            let body = br#"{"hello":"world"}"#;
            let response = format!(
                "HTTP/1.1 201 Created\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        });

        let request = RequestDraft {
            method: HttpMethod::Post,
            url: format!("http://{address}/items"),
            parameters: vec![KeyValueField::new("page", "2")],
            headers: vec![
                KeyValueField::new("Content-Type", "application/json"),
                KeyValueField::new("X-Alula-Test", "yes"),
            ],
            body: r#"{"name":"wing"}"#.into(),
            ..RequestDraft::default()
        };

        let mut events = Vec::new();
        let response =
            HttpExecutor::execute_streaming(&request, |event| events.push(event)).unwrap();
        server.join().unwrap();
        let captured = String::from_utf8(captured_rx.recv().unwrap()).unwrap();

        assert!(captured.starts_with("POST /items?page=2 HTTP/1.1"));
        assert!(captured.to_ascii_lowercase().contains("x-alula-test: yes"));
        assert!(captured.contains(r#"{"name":"wing"}"#));
        assert_eq!(response.status, 201);
        assert_eq!(response.body, r#"{"hello":"world"}"#);
        assert_eq!(response.size_bytes, 17);
        assert!(matches!(events.first(), Some(HttpStreamEvent::Started(_))));
        assert!(events.iter().any(|event| matches!(
            event,
            HttpStreamEvent::BodyChunk { text, .. } if text == r#"{"hello":"world"}"#
        )));
        assert!(matches!(
            events.last(),
            Some(HttpStreamEvent::Completed {
                total_bytes: 17,
                ..
            })
        ));
    }

    #[test]
    fn decodes_utf8_split_across_network_reads() {
        let mut decoder = Utf8LossyDecoder::default();

        assert_eq!(decoder.push(&[0xF0, 0x9F]), "");
        assert_eq!(decoder.push(&[0xA6]), "");
        assert_eq!(decoder.push(&[0x85, b'!']), "🦅!");
        assert_eq!(decoder.finish(), "");
    }

    #[test]
    fn session_reuses_login_cookie_for_a_later_matching_request() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (private_request_tx, private_request_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            for request_index in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).unwrap();
                if request_index == 1 {
                    private_request_tx
                        .send(String::from_utf8_lossy(&buffer[..read]).into_owned())
                        .unwrap();
                }
                let extra_headers = if request_index == 0 {
                    "Set-Cookie: session=logged-in; Path=/private; HttpOnly\r\n"
                } else {
                    ""
                };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\n{extra_headers}Content-Length: 2\r\nConnection: close\r\n\r\nok"
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });

        let session = HttpSession::new();
        session
            .execute(&RequestDraft {
                url: format!("http://{address}/login"),
                ..RequestDraft::default()
            })
            .unwrap();
        session
            .execute(&RequestDraft {
                url: format!("http://{address}/private/profile"),
                ..RequestDraft::default()
            })
            .unwrap();

        server.join().unwrap();
        let request = private_request_rx.recv().unwrap().to_ascii_lowercase();
        assert!(request.contains("cookie: session=logged-in\r\n"));
    }

    #[test]
    fn publishes_the_first_body_fragment_before_the_response_finishes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (first_written_tx, first_written_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 10\r\nConnection: close\r\n\r\nhello",
                )
                .unwrap();
            stream.flush().unwrap();
            first_written_tx.send(()).unwrap();
            finish_rx.recv().unwrap();
            stream.write_all(b"world").unwrap();
        });

        let request = RequestDraft {
            url: format!("http://{address}/stream"),
            ..RequestDraft::default()
        };
        let (event_tx, event_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            HttpExecutor::execute_streaming(&request, |event| event_tx.send(event).unwrap())
                .unwrap()
        });

        first_written_rx
            .recv_timeout(Duration::from_secs(1))
            .unwrap();
        let first_chunk = loop {
            let event = event_rx.recv_timeout(Duration::from_secs(1)).unwrap();
            if let HttpStreamEvent::BodyChunk { text, .. } = event {
                break text;
            }
        };
        assert_eq!(first_chunk, "hello");
        assert!(!worker.is_finished());

        finish_tx.send(()).unwrap();
        let response = worker.join().unwrap();
        server.join().unwrap();
        assert_eq!(response.body, "helloworld");
    }
}
