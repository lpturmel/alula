use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use url::Url;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub(crate) fn next_id(prefix: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{timestamp:x}-{sequence:x}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl HttpMethod {
    pub const ALL: [Self; 7] = [
        Self::Get,
        Self::Post,
        Self::Put,
        Self::Patch,
        Self::Delete,
        Self::Head,
        Self::Options,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL.iter().position(|item| *item == self).unwrap_or(0);
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyValueField {
    pub id: String,
    pub enabled: bool,
    pub key: String,
    pub value: String,
}

impl KeyValueField {
    pub fn empty() -> Self {
        Self {
            id: next_id("field"),
            enabled: true,
            key: String::new(),
            value: String::new(),
        }
    }

    pub fn new(key: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            id: next_id("field"),
            enabled: true,
            key: key.into(),
            value: value.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestDraft {
    pub id: String,
    pub name: String,
    pub method: HttpMethod,
    pub url: String,
    pub parameters: Vec<KeyValueField>,
    pub headers: Vec<KeyValueField>,
    pub body: String,
}

impl Default for RequestDraft {
    fn default() -> Self {
        Self {
            id: next_id("request"),
            name: "Untitled request".into(),
            method: HttpMethod::Get,
            url: "https://httpbin.org/anything".into(),
            parameters: vec![KeyValueField::empty()],
            headers: vec![KeyValueField::new("Accept", "application/json")],
            body: String::new(),
        }
    }
}

impl RequestDraft {
    pub fn resolved_url(&self) -> Result<Url, String> {
        let mut url = Url::parse(self.url.trim()).map_err(|error| error.to_string())?;
        {
            let mut pairs = url.query_pairs_mut();
            for parameter in self
                .parameters
                .iter()
                .filter(|item| item.enabled && !item.key.trim().is_empty())
            {
                pairs.append_pair(parameter.key.trim(), parameter.value.as_str());
            }
        }
        Ok(url)
    }

    pub fn display_name(&self) -> String {
        if self.name != "Untitled request" && !self.name.trim().is_empty() {
            return self.name.clone();
        }
        Url::parse(self.url.trim())
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_else(|| "Untitled request".into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseSnapshot {
    pub status: u16,
    pub status_text: String,
    pub elapsed_ms: u128,
    pub size_bytes: usize,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub content_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    pub requests: Vec<RequestDraft>,
    pub active_request_id: String,
}

impl Default for Workspace {
    fn default() -> Self {
        let request = RequestDraft::default();
        Self {
            active_request_id: request.id.clone(),
            requests: vec![request],
        }
    }
}

impl Workspace {
    pub fn active(&self) -> Option<&RequestDraft> {
        self.requests
            .iter()
            .find(|request| request.id == self.active_request_id)
    }

    pub fn active_mut(&mut self) -> Option<&mut RequestDraft> {
        self.requests
            .iter_mut()
            .find(|request| request.id == self.active_request_id)
    }

    pub fn add_request(&mut self, request: RequestDraft) -> String {
        let id = request.id.clone();
        self.active_request_id = id.clone();
        self.requests.push(request);
        id
    }

    pub fn normalize(mut self) -> Self {
        if self.requests.is_empty() {
            return Self::default();
        }
        if !self
            .requests
            .iter()
            .any(|request| request.id == self.active_request_id)
        {
            self.active_request_id = self.requests[0].id.clone();
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_enabled_parameters_and_preserves_existing_query() {
        let mut request = RequestDraft {
            url: "https://example.com/search?existing=1".into(),
            ..RequestDraft::default()
        };
        request.parameters = vec![
            KeyValueField::new("q", "rust ui"),
            KeyValueField {
                enabled: false,
                ..KeyValueField::new("ignored", "yes")
            },
        ];

        let url = request.resolved_url().unwrap();
        let pairs: Vec<_> = url.query_pairs().collect();
        assert!(
            pairs
                .iter()
                .any(|(key, value)| key == "existing" && value == "1")
        );
        assert!(
            pairs
                .iter()
                .any(|(key, value)| key == "q" && value == "rust ui")
        );
        assert!(!pairs.iter().any(|(key, _)| key == "ignored"));
    }
}
