use std::{
    collections::{HashMap, HashSet, VecDeque},
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bincode::Options as _;
use serde::{Deserialize, Serialize, Serializer, de::DeserializeOwned, ser::SerializeStruct};

use crate::model::{RequestDraft, ResponseSnapshot, Workspace, next_id};

pub const STATE_VERSION: u32 = 1;
pub const MAX_HISTORY_ENTRIES: usize = 500;
const STATE_CACHE_MAGIC: &[u8; 8] = b"ALULAC01";
const STATE_CACHE_HEADER_BYTES: usize = 28;
const MAX_STATE_CACHE_BYTES: u64 = 256 * 1024 * 1024;
const ENVIRONMENT_SHARE_PREFIX: &str = "alula-env-v1:";
const ENVIRONMENT_SHARE_VERSION: u32 = 1;
const MAX_ENVIRONMENT_SHARE_BYTES: usize = 16 * 1024 * 1024;
static NEXT_TEMPORARY_FILE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct SourceStamp {
    len: u64,
    modified_secs: u64,
    modified_nanos: u32,
}

impl SourceStamp {
    fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?;
        let duration = modified.duration_since(UNIX_EPOCH).ok()?;
        Some(Self {
            len: metadata.len(),
            modified_secs: duration.as_secs(),
            modified_nanos: duration.subsec_nanos(),
        })
    }
}

#[derive(Serialize, Deserialize)]
struct EnvironmentSharePayload {
    version: u32,
    environment: Environment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub name: String,
    /// Requests that have not been placed in a folder. Kept at the original
    /// TOML location so state files written by older Alula versions continue
    /// to load without a migration.
    #[serde(default)]
    pub requests: Vec<RequestDraft>,
    #[serde(default)]
    pub folders: Vec<EnvironmentFolder>,
    #[serde(default)]
    pub variables: Vec<EnvironmentVariable>,
}

impl Environment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: next_id("environment"),
            name: name.into(),
            requests: Vec::new(),
            folders: Vec::new(),
            variables: Vec::new(),
        }
    }

    /// Encodes this environment, including its folder tree and saved request
    /// snapshots, as a versioned string suitable for copying between Alula
    /// installations. Secret variable values are omitted by the variable's
    /// serializer; only their names and secret marker are shared.
    pub fn to_share_string(&self) -> Result<String> {
        let payload = EnvironmentSharePayload {
            version: ENVIRONMENT_SHARE_VERSION,
            environment: self.clone(),
        };
        let json = serde_json::to_vec(&payload).context("failed to serialize environment")?;
        Ok(format!(
            "{ENVIRONMENT_SHARE_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(json)
        ))
    }

    /// Decodes a shared environment and assigns fresh IDs to every imported
    /// object so the result can safely coexist with the source environment.
    pub fn from_share_string(source: &str) -> Result<Self> {
        let source = source.trim();
        let encoded = source
            .strip_prefix(ENVIRONMENT_SHARE_PREFIX)
            .ok_or_else(|| anyhow!("not an Alula environment share string"))?;
        if encoded.len() > MAX_ENVIRONMENT_SHARE_BYTES {
            anyhow::bail!("environment share string is too large");
        }
        let json = URL_SAFE_NO_PAD
            .decode(encoded)
            .context("environment share string is not valid")?;
        let payload: EnvironmentSharePayload =
            serde_json::from_slice(&json).context("environment share data is not valid")?;
        if payload.version != ENVIRONMENT_SHARE_VERSION {
            anyhow::bail!("unsupported environment share version {}", payload.version);
        }
        let mut environment = payload.environment;
        environment.regenerate_ids();
        Ok(environment)
    }

    fn regenerate_ids(&mut self) {
        self.id = next_id("environment");
        for request in &mut self.requests {
            regenerate_request_ids(request);
        }
        for folder in &mut self.folders {
            folder.regenerate_ids();
        }
        for variable in &mut self.variables {
            variable.id = next_id("variable");
            if variable.secret {
                variable.value = None;
            }
        }
    }

    pub fn request_count(&self) -> usize {
        self.requests.len()
            + self
                .folders
                .iter()
                .map(EnvironmentFolder::request_count)
                .sum::<usize>()
    }

    pub fn request(&self, request_id: &str) -> Option<&RequestDraft> {
        self.requests
            .iter()
            .find(|request| request.id == request_id)
            .or_else(|| {
                self.folders
                    .iter()
                    .find_map(|folder| folder.request(request_id))
            })
    }

    pub fn request_mut(&mut self, request_id: &str) -> Option<&mut RequestDraft> {
        if let Some(request) = self
            .requests
            .iter_mut()
            .find(|request| request.id == request_id)
        {
            return Some(request);
        }
        self.folders
            .iter_mut()
            .find_map(|folder| folder.request_mut(request_id))
    }

    pub fn request_ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(
            self.requests
                .iter()
                .map(|request| request.id.as_str())
                .chain(self.folders.iter().flat_map(EnvironmentFolder::request_ids)),
        )
    }

    pub fn find_folder(&self, folder_id: &str) -> Option<&EnvironmentFolder> {
        self.folders
            .iter()
            .find_map(|folder| folder.find_folder(folder_id))
    }

    pub fn find_folder_mut(&mut self, folder_id: &str) -> Option<&mut EnvironmentFolder> {
        self.folders
            .iter_mut()
            .find_map(|folder| folder.find_folder_mut(folder_id))
    }

    pub fn folder_for_request(&self, request_id: &str) -> Option<&EnvironmentFolder> {
        self.folders
            .iter()
            .find_map(|folder| folder.folder_for_request(request_id))
    }

    pub fn folder_paths(&self) -> Vec<(String, String)> {
        let mut paths = Vec::new();
        for folder in &self.folders {
            folder.collect_paths("", &mut paths);
        }
        paths
    }

    pub fn folder_name_exists_in(
        &self,
        parent_folder_id: Option<&str>,
        name: &str,
    ) -> Option<bool> {
        let folders = match parent_folder_id {
            Some(parent_folder_id) => &self.find_folder(parent_folder_id)?.folders,
            None => &self.folders,
        };
        Some(
            folders
                .iter()
                .any(|folder| folder.name.eq_ignore_ascii_case(name)),
        )
    }

    pub fn sibling_folder_name_exists(&self, folder_id: &str, name: &str) -> Option<bool> {
        if self.folders.iter().any(|folder| folder.id == folder_id) {
            return Some(
                self.folders
                    .iter()
                    .any(|folder| folder.id != folder_id && folder.name.eq_ignore_ascii_case(name)),
            );
        }
        self.folders
            .iter()
            .find_map(|folder| folder.sibling_name_exists(folder_id, name))
    }

    fn delete_folder(&mut self, folder_id: &str) -> Option<usize> {
        if let Some(position) = self
            .folders
            .iter()
            .position(|folder| folder.id == folder_id)
        {
            let folder = self.folders.remove(position);
            let moved = folder.requests.len();
            self.requests.extend(folder.requests);
            self.folders.splice(position..position, folder.folders);
            return Some(moved);
        }
        self.folders
            .iter_mut()
            .find_map(|folder| folder.delete_descendant(folder_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentFolder {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub requests: Vec<RequestDraft>,
    #[serde(default)]
    pub folders: Vec<EnvironmentFolder>,
}

impl EnvironmentFolder {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: next_id("folder"),
            name: name.into(),
            requests: Vec::new(),
            folders: Vec::new(),
        }
    }

    fn regenerate_ids(&mut self) {
        self.id = next_id("folder");
        for request in &mut self.requests {
            regenerate_request_ids(request);
        }
        for folder in &mut self.folders {
            folder.regenerate_ids();
        }
    }

    pub fn request_count(&self) -> usize {
        self.requests.len()
            + self
                .folders
                .iter()
                .map(EnvironmentFolder::request_count)
                .sum::<usize>()
    }

    pub fn request(&self, request_id: &str) -> Option<&RequestDraft> {
        self.requests
            .iter()
            .find(|request| request.id == request_id)
            .or_else(|| {
                self.folders
                    .iter()
                    .find_map(|folder| folder.request(request_id))
            })
    }

    pub fn request_mut(&mut self, request_id: &str) -> Option<&mut RequestDraft> {
        if let Some(request) = self
            .requests
            .iter_mut()
            .find(|request| request.id == request_id)
        {
            return Some(request);
        }
        self.folders
            .iter_mut()
            .find_map(|folder| folder.request_mut(request_id))
    }

    pub fn request_ids(&self) -> Box<dyn Iterator<Item = &str> + '_> {
        Box::new(
            self.requests
                .iter()
                .map(|request| request.id.as_str())
                .chain(self.folders.iter().flat_map(EnvironmentFolder::request_ids)),
        )
    }

    pub fn find_folder(&self, folder_id: &str) -> Option<&EnvironmentFolder> {
        if self.id == folder_id {
            return Some(self);
        }
        self.folders
            .iter()
            .find_map(|folder| folder.find_folder(folder_id))
    }

    pub fn find_folder_mut(&mut self, folder_id: &str) -> Option<&mut EnvironmentFolder> {
        if self.id == folder_id {
            return Some(self);
        }
        self.folders
            .iter_mut()
            .find_map(|folder| folder.find_folder_mut(folder_id))
    }

    pub fn folder_for_request(&self, request_id: &str) -> Option<&EnvironmentFolder> {
        if self.requests.iter().any(|request| request.id == request_id) {
            return Some(self);
        }
        self.folders
            .iter()
            .find_map(|folder| folder.folder_for_request(request_id))
    }

    pub fn remove_request(&mut self, request_id: &str) -> bool {
        let previous_len = self.requests.len();
        self.requests.retain(|request| request.id != request_id);
        let mut removed = previous_len != self.requests.len();
        for folder in &mut self.folders {
            removed |= folder.remove_request(request_id);
        }
        removed
    }

    fn sync_open_requests(&mut self, requests: &HashMap<&str, &RequestDraft>) {
        for saved in &mut self.requests {
            if let Some(open) = requests.get(saved.id.as_str())
                && saved != *open
            {
                saved.clone_from(open);
            }
        }
        for folder in &mut self.folders {
            folder.sync_open_requests(requests);
        }
    }

    fn collect_matching_request_ids<'a>(
        &'a self,
        requested: &HashSet<&str>,
        assigned: &mut HashSet<&'a str>,
    ) {
        assigned.extend(
            self.requests
                .iter()
                .map(|request| request.id.as_str())
                .filter(|request_id| requested.contains(request_id)),
        );
        for folder in &self.folders {
            folder.collect_matching_request_ids(requested, assigned);
        }
    }

    fn collect_paths(&self, prefix: &str, paths: &mut Vec<(String, String)>) {
        let path = if prefix.is_empty() {
            self.name.clone()
        } else {
            format!("{prefix} / {}", self.name)
        };
        paths.push((self.id.clone(), path.clone()));
        for folder in &self.folders {
            folder.collect_paths(&path, paths);
        }
    }

    fn sibling_name_exists(&self, folder_id: &str, name: &str) -> Option<bool> {
        if self.folders.iter().any(|folder| folder.id == folder_id) {
            return Some(
                self.folders
                    .iter()
                    .any(|folder| folder.id != folder_id && folder.name.eq_ignore_ascii_case(name)),
            );
        }
        self.folders
            .iter()
            .find_map(|folder| folder.sibling_name_exists(folder_id, name))
    }

    fn delete_descendant(&mut self, folder_id: &str) -> Option<usize> {
        if let Some(position) = self
            .folders
            .iter()
            .position(|folder| folder.id == folder_id)
        {
            let folder = self.folders.remove(position);
            let moved = folder.requests.len();
            self.requests.extend(folder.requests);
            self.folders.splice(position..position, folder.folders);
            return Some(moved);
        }
        self.folders
            .iter_mut()
            .find_map(|folder| folder.delete_descendant(folder_id))
    }
}

fn regenerate_request_ids(request: &mut RequestDraft) {
    request.id = next_id("request");
    for field in request
        .parameters
        .iter_mut()
        .chain(request.headers.iter_mut())
    {
        field.id = next_id("field");
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EnvironmentVariable {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub value: Option<String>,
    #[serde(default)]
    pub secret: bool,
}

impl EnvironmentVariable {
    pub fn public(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            id: next_id("variable"),
            name: name.into(),
            value: Some(value.into()),
            secret: false,
        }
    }

    pub fn secret(name: impl Into<String>, value: Option<String>) -> Self {
        Self {
            id: next_id("variable"),
            name: name.into(),
            value,
            secret: true,
        }
    }
}

// Secret values may live in memory while a request is being edited or sent,
// but serialization deliberately drops them. Only the variable metadata is
// written to environments.toml; the value belongs to the OS credential store.
impl Serialize for EnvironmentVariable {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state =
            serializer.serialize_struct("EnvironmentVariable", if self.secret { 3 } else { 4 })?;
        state.serialize_field("id", &self.id)?;
        state.serialize_field("name", &self.name)?;
        if !self.secret {
            state.serialize_field("value", &self.value)?;
        }
        state.serialize_field("secret", &self.secret)?;
        state.end()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentStore {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub environments: Vec<Environment>,
}

impl Default for EnvironmentStore {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            environments: Vec::new(),
        }
    }
}

impl EnvironmentStore {
    /// Repairs IDs written by older versions whose process-local counter could
    /// reuse the same folder ID after an application restart. The traversal
    /// order and suffixes are deterministic so an unsaved migration remains
    /// stable across launches.
    pub fn normalize(mut self) -> Self {
        let mut seen = HashSet::new();
        for environment in &mut self.environments {
            normalize_folder_ids(&mut environment.folders, &mut seen);
        }
        self
    }

    pub fn create(&mut self, name: impl Into<String>) -> String {
        let environment = Environment::new(name);
        let id = environment.id.clone();
        self.environments.push(environment);
        id
    }

    pub fn environment_for_request(&self, request_id: &str) -> Option<&Environment> {
        self.environments
            .iter()
            .find(|environment| environment.request(request_id).is_some())
    }

    pub fn environment_for_request_mut(&mut self, request_id: &str) -> Option<&mut Environment> {
        self.environments
            .iter_mut()
            .find(|environment| environment.request(request_id).is_some())
    }

    pub fn folder_for_request(&self, request_id: &str) -> Option<&EnvironmentFolder> {
        self.environment_for_request(request_id)
            .and_then(|environment| environment.folder_for_request(request_id))
    }

    pub fn hydrate_secrets(&mut self) {
        for environment in &mut self.environments {
            for variable in &mut environment.variables {
                if variable.secret {
                    variable.value = crate::variables::load_secret(&environment.id, &variable.id)
                        .ok()
                        .flatten();
                }
            }
        }
    }

    /// A request belongs to at most one environment. Reassigning it moves the
    /// saved snapshot without affecting open tabs or execution history.
    pub fn assign(&mut self, environment_id: &str, request: RequestDraft) -> Result<()> {
        self.assign_to_folder(environment_id, None, request)
    }

    /// Assign a request to an environment's root or to one of its folders.
    /// Reassignment is a move, so a request can appear in only one location.
    pub fn assign_to_folder(
        &mut self,
        environment_id: &str,
        folder_id: Option<&str>,
        request: RequestDraft,
    ) -> Result<()> {
        let Some(environment_index) = self
            .environments
            .iter()
            .position(|environment| environment.id == environment_id)
        else {
            anyhow::bail!("environment not found");
        };
        if let Some(folder_id) = folder_id
            && self.environments[environment_index]
                .find_folder(folder_id)
                .is_none()
        {
            anyhow::bail!("folder not found");
        }
        self.remove_request(&request.id);
        let environment = &mut self.environments[environment_index];
        if let Some(folder_id) = folder_id {
            let folder = environment
                .find_folder_mut(folder_id)
                .expect("folder existence checked");
            folder.requests.push(request);
        } else {
            environment.requests.push(request);
        }
        Ok(())
    }

    pub fn create_folder(
        &mut self,
        environment_id: &str,
        parent_folder_id: Option<&str>,
        name: impl Into<String>,
    ) -> Result<String> {
        let environment = self
            .environments
            .iter_mut()
            .find(|environment| environment.id == environment_id)
            .ok_or_else(|| anyhow!("environment not found"))?;
        let folder = EnvironmentFolder::new(name);
        let id = folder.id.clone();
        if let Some(parent_folder_id) = parent_folder_id {
            let parent = environment
                .find_folder_mut(parent_folder_id)
                .ok_or_else(|| anyhow!("parent folder not found"))?;
            parent.folders.push(folder);
        } else {
            environment.folders.push(folder);
        }
        Ok(id)
    }

    pub fn rename_folder(
        &mut self,
        environment_id: &str,
        folder_id: &str,
        name: impl Into<String>,
    ) -> Result<()> {
        let folder = self
            .environments
            .iter_mut()
            .find(|environment| environment.id == environment_id)
            .and_then(|environment| environment.find_folder_mut(folder_id))
            .ok_or_else(|| anyhow!("folder not found"))?;
        folder.name = name.into();
        Ok(())
    }

    /// Deleting a folder promotes its requests and child folders to its parent,
    /// making folder management non-destructive to saved request snapshots.
    pub fn delete_folder(&mut self, environment_id: &str, folder_id: &str) -> Result<usize> {
        let environment = self
            .environments
            .iter_mut()
            .find(|environment| environment.id == environment_id)
            .ok_or_else(|| anyhow!("environment not found"))?;
        environment
            .delete_folder(folder_id)
            .ok_or_else(|| anyhow!("folder not found"))
    }

    pub fn remove_request(&mut self, request_id: &str) -> bool {
        let mut removed = false;
        for environment in &mut self.environments {
            let previous_len = environment.requests.len();
            environment
                .requests
                .retain(|request| request.id != request_id);
            removed |= previous_len != environment.requests.len();
            for folder in &mut environment.folders {
                removed |= folder.remove_request(request_id);
            }
        }
        removed
    }

    pub fn remove(&mut self, environment_id: &str) -> Option<Environment> {
        let position = self
            .environments
            .iter()
            .position(|environment| environment.id == environment_id)?;
        Some(self.environments.remove(position))
    }

    pub fn sync_open_requests(&mut self, requests: &[RequestDraft]) {
        if requests.is_empty() {
            return;
        }
        let requests = requests
            .iter()
            .map(|request| (request.id.as_str(), request))
            .collect::<HashMap<_, _>>();
        for environment in &mut self.environments {
            for saved in &mut environment.requests {
                if let Some(open) = requests.get(saved.id.as_str())
                    && saved != *open
                {
                    saved.clone_from(open);
                }
            }
            for folder in &mut environment.folders {
                folder.sync_open_requests(&requests);
            }
        }
    }

    /// Returns the request IDs assigned anywhere in the environment tree.
    /// This is intended for batch membership checks during a render; building
    /// one set avoids repeatedly walking every saved request for each open tab.
    pub fn assigned_request_ids<'a, 'b>(
        &'a self,
        request_ids: impl IntoIterator<Item = &'b str>,
    ) -> HashSet<&'a str> {
        let requested = request_ids.into_iter().collect::<HashSet<_>>();
        let mut assigned = HashSet::with_capacity(requested.len());
        for environment in &self.environments {
            assigned.extend(
                environment
                    .requests
                    .iter()
                    .map(|request| request.id.as_str())
                    .filter(|request_id| requested.contains(request_id)),
            );
            for folder in &environment.folders {
                folder.collect_matching_request_ids(&requested, &mut assigned);
            }
            if assigned.len() == requested.len() {
                break;
            }
        }
        assigned
    }
}

fn normalize_folder_ids(folders: &mut [EnvironmentFolder], seen: &mut HashSet<String>) {
    for folder in folders {
        let base = if folder.id.trim().is_empty() {
            "folder".to_owned()
        } else {
            folder.id.clone()
        };
        if !seen.insert(base.clone()) {
            let mut suffix = 2;
            loop {
                let candidate = format!("{base}-{suffix}");
                if seen.insert(candidate.clone()) {
                    folder.id = candidate;
                    break;
                }
                suffix += 1;
            }
        } else if folder.id != base {
            folder.id = base;
        }
        normalize_folder_ids(&mut folder.folders, seen);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: String,
    pub sent_at_unix_ms: u64,
    pub request: RequestDraft,
    pub status: Option<u16>,
    pub status_text: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub size_bytes: Option<usize>,
    pub error: Option<String>,
}

impl HistoryEntry {
    pub fn success(request: RequestDraft, response: &ResponseSnapshot) -> Self {
        Self {
            id: next_id("history"),
            sent_at_unix_ms: now_unix_ms(),
            request,
            status: Some(response.status),
            status_text: Some(response.status_text.clone()),
            elapsed_ms: Some(response.elapsed_ms.min(u64::MAX as u128) as u64),
            size_bytes: Some(response.size_bytes),
            error: None,
        }
    }

    pub fn failure(request: RequestDraft, error: impl Into<String>) -> Self {
        Self {
            id: next_id("history"),
            sent_at_unix_ms: now_unix_ms(),
            request,
            status: None,
            status_text: None,
            elapsed_ms: None,
            size_bytes: None,
            error: Some(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryStore {
    #[serde(default = "state_version")]
    pub version: u32,
    #[serde(default)]
    pub entries: Arc<VecDeque<HistoryEntry>>,
}

impl Default for HistoryStore {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            entries: Arc::new(VecDeque::new()),
        }
    }
}

impl HistoryStore {
    pub fn push(&mut self, entry: HistoryEntry) {
        let entries = Arc::make_mut(&mut self.entries);
        entries.push_front(entry);
        entries.truncate(MAX_HISTORY_ENTRIES);
    }

    pub fn remove(&mut self, history_id: &str) -> bool {
        let entries = Arc::make_mut(&mut self.entries);
        let previous_len = entries.len();
        entries.retain(|entry| entry.id != history_id);
        entries.len() != previous_len
    }

    /// Adds an older, persisted history behind entries recorded since its
    /// asynchronous load started. IDs are de-duplicated so an MCP command and
    /// the desktop app cannot make the same entry appear twice.
    pub fn merge_older(&mut self, older: Self) {
        self.version = self.version.max(older.version);
        if self.entries.is_empty() {
            self.entries = older.entries;
            Arc::make_mut(&mut self.entries).truncate(MAX_HISTORY_ENTRIES);
            return;
        }

        let entries = Arc::make_mut(&mut self.entries);
        let remaining = MAX_HISTORY_ENTRIES.saturating_sub(entries.len());
        let mut ids = entries
            .iter()
            .map(|entry| entry.id.clone())
            .collect::<HashSet<_>>();
        entries.extend(
            older
                .entries
                .iter()
                .filter(|entry| ids.insert(entry.id.clone()))
                .take(remaining)
                .cloned(),
        );
    }
}

#[derive(Debug, Clone)]
pub struct StatePaths {
    pub workspace: PathBuf,
    pub history: PathBuf,
    pub environments: PathBuf,
}

impl StatePaths {
    pub fn beside(config_path: &Path) -> Self {
        let directory = config_path.parent().unwrap_or_else(|| Path::new("."));
        Self {
            workspace: directory.join("workspace.toml"),
            history: directory.join("history.toml"),
            environments: directory.join("environments.toml"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PersistedState {
    pub workspace: Workspace,
    pub history: HistoryStore,
    pub environments: EnvironmentStore,
}

impl PersistedState {
    /// Loads only state required to construct the first window. History is
    /// deliberately left empty and can be loaded after the first frame with
    /// [`Self::load_history`].
    pub fn load_startup(paths: &StatePaths) -> Result<Self> {
        thread::scope(|scope| {
            let workspace = scope.spawn(|| load_or_default::<Workspace>(&paths.workspace));
            let environments =
                scope.spawn(|| load_or_default::<EnvironmentStore>(&paths.environments));
            Ok(Self {
                workspace: workspace
                    .join()
                    .map_err(|_| anyhow!("workspace loader panicked"))??
                    .normalize(),
                history: HistoryStore::default(),
                environments: environments
                    .join()
                    .map_err(|_| anyhow!("environment loader panicked"))??
                    .normalize(),
            })
        })
    }

    pub fn load_history(paths: &StatePaths) -> Result<HistoryStore> {
        load_or_default(&paths.history)
    }

    pub fn load(paths: &StatePaths) -> Result<Self> {
        thread::scope(|scope| {
            let workspace = scope.spawn(|| load_or_default::<Workspace>(&paths.workspace));
            let history = scope.spawn(|| load_or_default::<HistoryStore>(&paths.history));
            let environments =
                scope.spawn(|| load_or_default::<EnvironmentStore>(&paths.environments));
            Ok(Self {
                workspace: workspace
                    .join()
                    .map_err(|_| anyhow!("workspace loader panicked"))??
                    .normalize(),
                history: history
                    .join()
                    .map_err(|_| anyhow!("history loader panicked"))??,
                environments: environments
                    .join()
                    .map_err(|_| anyhow!("environment loader panicked"))??
                    .normalize(),
            })
        })
    }

    pub fn save(&self, paths: &StatePaths) -> Result<()> {
        thread::scope(|scope| {
            let workspace = scope.spawn(|| save_toml(&paths.workspace, &self.workspace));
            let history = scope.spawn(|| save_toml(&paths.history, &self.history));
            let environments = scope.spawn(|| save_toml(&paths.environments, &self.environments));
            workspace
                .join()
                .map_err(|_| anyhow!("workspace saver panicked"))??;
            history
                .join()
                .map_err(|_| anyhow!("history saver panicked"))??;
            environments
                .join()
                .map_err(|_| anyhow!("environment saver panicked"))??;
            Ok(())
        })
    }
}

pub fn load_or_default<T: DeserializeOwned + Serialize + Default>(path: &Path) -> Result<T> {
    let Some(source_stamp) = SourceStamp::read(path) else {
        if path.exists() {
            return Err(anyhow!("failed to read metadata for {}", path.display()));
        }
        return Ok(T::default());
    };
    if let Some(value) = load_state_cache(path, source_stamp) {
        return Ok(value);
    }
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    let value =
        toml::from_str(&source).with_context(|| format!("failed to parse {}", path.display()))?;
    // A cache is disposable acceleration data. Failure to create it must not
    // turn a successfully parsed, user-editable TOML file into a load error.
    let _ = save_state_cache(path, source_stamp, &value);
    Ok(value)
}

pub fn save_toml<T: Serialize + Sync>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    // TOML remains the editable source of truth, while a compact binary cache powers
    // subsequent loads. Serialize both representations concurrently so the
    // cache does not nearly double persistence latency for large histories.
    let (source, cached_value) = thread::scope(|scope| {
        let cached_value = scope.spawn(|| serialize_state_cache_value(value));
        let source = toml::to_string_pretty(value);
        let cached_value = cached_value.join().ok().and_then(Result::ok);
        (source, cached_value)
    });
    let source = source.context("failed to serialize persisted state")?;
    if fs::read(path).is_ok_and(|existing| existing.as_slice() == source.as_bytes()) {
        if !cache_path(path).exists()
            && let (Some(source_stamp), Some(cached_value)) =
                (SourceStamp::read(path), cached_value)
        {
            let _ = save_serialized_state_cache(path, source_stamp, &cached_value);
        }
        return Ok(());
    }
    atomic_write(path, source.as_bytes())?;
    if let (Some(source_stamp), Some(cached_value)) = (SourceStamp::read(path), cached_value) {
        let _ = save_serialized_state_cache(path, source_stamp, &cached_value);
    }
    Ok(())
}

fn load_state_cache<T: DeserializeOwned>(path: &Path, source_stamp: SourceStamp) -> Option<T> {
    let cache_path = cache_path(path);
    if fs::metadata(&cache_path).ok()?.len() > MAX_STATE_CACHE_BYTES {
        return None;
    }
    let source = fs::read(cache_path).ok()?;
    if source.get(..STATE_CACHE_MAGIC.len())? != STATE_CACHE_MAGIC {
        return None;
    }
    let cached_stamp = SourceStamp {
        len: u64::from_le_bytes(source.get(8..16)?.try_into().ok()?),
        modified_secs: u64::from_le_bytes(source.get(16..24)?.try_into().ok()?),
        modified_nanos: u32::from_le_bytes(source.get(24..28)?.try_into().ok()?),
    };
    if cached_stamp != source_stamp {
        return None;
    }
    deserialize_state_cache_value(source.get(STATE_CACHE_HEADER_BYTES..)?).ok()
}

fn save_state_cache<T: Serialize>(path: &Path, source_stamp: SourceStamp, value: &T) -> Result<()> {
    let value =
        serialize_state_cache_value(value).context("failed to serialize persisted-state cache")?;
    save_serialized_state_cache(path, source_stamp, &value)
}

fn save_serialized_state_cache(path: &Path, source_stamp: SourceStamp, value: &[u8]) -> Result<()> {
    let mut source = Vec::with_capacity(value.len().saturating_add(STATE_CACHE_HEADER_BYTES));
    source.extend_from_slice(STATE_CACHE_MAGIC);
    source.extend_from_slice(&source_stamp.len.to_le_bytes());
    source.extend_from_slice(&source_stamp.modified_secs.to_le_bytes());
    source.extend_from_slice(&source_stamp.modified_nanos.to_le_bytes());
    source.extend_from_slice(value);
    atomic_write(&cache_path(path), &source)
}

fn serialize_state_cache_value<T: Serialize>(value: &T) -> bincode::Result<Vec<u8>> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_STATE_CACHE_BYTES)
        .serialize(value)
}

fn deserialize_state_cache_value<T: DeserializeOwned>(source: &[u8]) -> bincode::Result<T> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_STATE_CACHE_BYTES)
        .deserialize(source)
}

fn cache_path(path: &Path) -> PathBuf {
    let mut cache = OsString::from(path.as_os_str());
    cache.push(".cache");
    PathBuf::from(cache)
}

fn atomic_write(path: &Path, source: &[u8]) -> Result<()> {
    let sequence = NEXT_TEMPORARY_FILE.fetch_add(1, Ordering::Relaxed);
    let mut temporary = OsString::from(path.as_os_str());
    temporary.push(format!(".tmp-{}-{sequence}", std::process::id()));
    let temporary = PathBuf::from(temporary);
    fs::write(&temporary, source)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).with_context(|| format!("failed to replace {}", path.display()));
    }
    Ok(())
}

fn state_version() -> u32 {
    STATE_VERSION
}

fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_history_and_environments_are_separate_files() {
        let directory = std::env::temp_dir().join(format!(
            "alula-state-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let paths = StatePaths::beside(&directory.join("config.toml"));
        let mut state = PersistedState::load(&paths).unwrap();
        let request = state.workspace.active().unwrap().clone();
        let environment_id = state.environments.create("Production");
        state
            .environments
            .assign(&environment_id, request.clone())
            .unwrap();
        state
            .history
            .push(HistoryEntry::failure(request, "connection refused"));
        state.save(&paths).unwrap();

        let loaded = PersistedState::load(&paths).unwrap();
        assert_eq!(loaded.workspace.requests.len(), 1);
        assert_eq!(loaded.environments.environments[0].name, "Production");
        assert_eq!(
            loaded.history.entries[0].error.as_deref(),
            Some("connection refused")
        );
        assert!(paths.workspace.exists());
        assert!(paths.history.exists());
        assert!(paths.environments.exists());
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn startup_load_defers_history_without_losing_newer_entries() {
        let directory = std::env::temp_dir().join(format!(
            "alula-startup-state-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let paths = StatePaths::beside(&directory.join("config.toml"));
        let mut state = PersistedState::load(&paths).unwrap();
        let request = state.workspace.active().unwrap().clone();
        state
            .history
            .push(HistoryEntry::failure(request.clone(), "persisted"));
        state.save(&paths).unwrap();

        let mut startup = PersistedState::load_startup(&paths).unwrap();
        assert!(startup.history.entries.is_empty());
        startup
            .history
            .push(HistoryEntry::failure(request, "newer"));
        startup
            .history
            .merge_older(PersistedState::load_history(&paths).unwrap());

        assert_eq!(startup.history.entries.len(), 2);
        assert_eq!(startup.history.entries[0].error.as_deref(), Some("newer"));
        assert_eq!(
            startup.history.entries[1].error.as_deref(),
            Some("persisted")
        );
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn state_cache_accelerates_loads_but_never_overrides_edited_toml() {
        let directory = std::env::temp_dir().join(format!(
            "alula-cache-{}-{}",
            std::process::id(),
            now_unix_ms()
        ));
        let path = directory.join("workspace.toml");
        let workspace = Workspace::default();
        save_toml(&path, &workspace).unwrap();
        assert!(cache_path(&path).exists());
        assert_eq!(
            load_or_default::<Workspace>(&path)
                .unwrap()
                .active_request_id,
            workspace.active_request_id
        );

        let mut edited = workspace.clone();
        let second = RequestDraft::default();
        edited.active_request_id = second.id.clone();
        edited.requests.push(second);
        let mut source = toml::to_string_pretty(&edited).unwrap();
        source.push_str("\n# edited outside Alula\n");
        fs::write(&path, source).unwrap();

        let loaded = load_or_default::<Workspace>(&path).unwrap();
        assert_eq!(loaded.active_request_id, edited.active_request_id);
        assert_eq!(loaded.requests.len(), 2);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn large_environment_sync_updates_nested_requests_and_batch_ids() {
        let mut environment = Environment::new("Performance");
        let mut folder = EnvironmentFolder::new("Nested");
        let saved = RequestDraft {
            name: "Saved".into(),
            ..RequestDraft::default()
        };
        let request_id = saved.id.clone();
        folder.requests.push(saved);
        environment.folders.push(folder);
        let mut store = EnvironmentStore {
            environments: vec![environment],
            ..EnvironmentStore::default()
        };
        let mut open = store.environments[0].folders[0].requests[0].clone();
        open.name = "Open".into();

        store.sync_open_requests(&[open]);

        assert_eq!(store.environments[0].folders[0].requests[0].name, "Open");
        assert!(
            store
                .assigned_request_ids([request_id.as_str()])
                .contains(request_id.as_str())
        );
    }

    #[test]
    fn assigning_moves_request_between_environments() {
        let mut store = EnvironmentStore::default();
        let first = store.create("Local");
        let second = store.create("Production");
        let request = RequestDraft::default();
        store.assign(&first, request.clone()).unwrap();
        store.assign(&second, request.clone()).unwrap();
        assert!(store.environments[0].requests.is_empty());
        assert_eq!(store.environments[1].requests, vec![request]);
    }

    #[test]
    fn folders_move_requests_and_delete_back_to_root() {
        let mut store = EnvironmentStore::default();
        let environment_id = store.create("Production");
        let folder_id = store
            .create_folder(&environment_id, None, "Authentication")
            .unwrap();
        let request = RequestDraft::default();

        store
            .assign_to_folder(&environment_id, Some(&folder_id), request.clone())
            .unwrap();
        assert_eq!(
            store.folder_for_request(&request.id).unwrap().name,
            "Authentication"
        );
        assert_eq!(store.environments[0].request_count(), 1);

        assert_eq!(store.delete_folder(&environment_id, &folder_id).unwrap(), 1);
        assert!(store.folder_for_request(&request.id).is_none());
        assert_eq!(store.environments[0].requests, vec![request]);
    }

    #[test]
    fn nested_folders_assign_and_promote_contents_recursively() {
        let mut store = EnvironmentStore::default();
        let environment_id = store.create("Production");
        let parent_id = store.create_folder(&environment_id, None, "API").unwrap();
        let child_id = store
            .create_folder(&environment_id, Some(&parent_id), "Users")
            .unwrap();
        let grandchild_id = store
            .create_folder(&environment_id, Some(&child_id), "Admin")
            .unwrap();
        let child_request = RequestDraft::default();
        let grandchild_request = RequestDraft::default();
        store
            .assign_to_folder(&environment_id, Some(&child_id), child_request.clone())
            .unwrap();
        store
            .assign_to_folder(
                &environment_id,
                Some(&grandchild_id),
                grandchild_request.clone(),
            )
            .unwrap();

        let environment = &store.environments[0];
        assert_eq!(environment.request_count(), 2);
        assert_eq!(
            environment.folder_paths(),
            vec![
                (parent_id.clone(), "API".into()),
                (child_id.clone(), "API / Users".into()),
                (grandchild_id.clone(), "API / Users / Admin".into()),
            ]
        );

        assert_eq!(store.delete_folder(&environment_id, &child_id).unwrap(), 1);
        let parent = store.environments[0].find_folder(&parent_id).unwrap();
        assert_eq!(parent.requests, vec![child_request]);
        assert_eq!(parent.folders[0].id, grandchild_id);
        assert_eq!(parent.folders[0].requests, vec![grandchild_request]);
    }

    #[test]
    fn duplicate_legacy_folder_ids_are_normalized_deterministically() {
        let mut environment = Environment::new("Legacy");
        let mut first = EnvironmentFolder::new("Auth");
        first.id = "folder-1".into();
        let mut second = EnvironmentFolder::new("Backend");
        second.id = "folder-1".into();
        environment.folders = vec![first, second];
        let mut store = EnvironmentStore {
            environments: vec![environment],
            ..EnvironmentStore::default()
        }
        .normalize();

        assert_eq!(store.environments[0].folders[0].id, "folder-1");
        assert_eq!(store.environments[0].folders[1].id, "folder-1-2");
        let environment_id = store.environments[0].id.clone();
        store
            .create_folder(&environment_id, Some("folder-1-2"), "Nested")
            .unwrap();
        assert!(store.environments[0].folders[0].folders.is_empty());
        assert_eq!(store.environments[0].folders[1].folders[0].name, "Nested");
        assert_eq!(
            store.clone().normalize().environments[0].folders[1].id,
            "folder-1-2"
        );
    }

    #[test]
    fn old_environment_toml_loads_with_no_folders() {
        let source = r#"
version = 1

[[environments]]
id = "environment-old"
name = "Legacy"
requests = []
variables = []
"#;
        let store: EnvironmentStore = toml::from_str(source).unwrap();
        assert!(store.environments[0].folders.is_empty());
    }

    #[test]
    fn environment_share_string_round_trips_requests_folders_and_variables() {
        let mut environment = Environment::new("Production");
        environment.variables.push(EnvironmentVariable::public(
            "api_url",
            "https://example.com",
        ));
        environment.variables.push(EnvironmentVariable::secret(
            "token",
            Some("do-not-share".into()),
        ));
        let mut folder = EnvironmentFolder::new("Authentication");
        let request = RequestDraft {
            name: "Sign in".into(),
            url: "{{api_url}}/login".into(),
            ..RequestDraft::default()
        };
        folder.requests.push(request);
        environment.folders.push(folder);

        let original_environment_id = environment.id.clone();
        let original_folder_id = environment.folders[0].id.clone();
        let original_request_id = environment.folders[0].requests[0].id.clone();
        let encoded = environment.to_share_string().unwrap();
        let imported = Environment::from_share_string(&encoded).unwrap();

        assert!(encoded.starts_with(ENVIRONMENT_SHARE_PREFIX));
        assert!(!encoded.contains("do-not-share"));
        assert_eq!(imported.name, "Production");
        assert_eq!(imported.folder_paths()[0].1, "Authentication");
        assert_eq!(imported.folders[0].requests[0].name, "Sign in");
        assert_eq!(imported.folders[0].requests[0].url, "{{api_url}}/login");
        assert_eq!(
            imported.variables[0].value.as_deref(),
            Some("https://example.com")
        );
        assert!(imported.variables[1].secret);
        assert_eq!(imported.variables[1].value, None);
        assert_ne!(imported.id, original_environment_id);
        assert_ne!(imported.folders[0].id, original_folder_id);
        assert_ne!(imported.folders[0].requests[0].id, original_request_id);
    }

    #[test]
    fn environment_share_string_rejects_invalid_input() {
        assert!(Environment::from_share_string("not-a-share-string").is_err());
        assert!(Environment::from_share_string("alula-env-v1:not-base64!").is_err());
    }

    #[test]
    fn secret_variable_values_are_never_serialized() {
        let variable = EnvironmentVariable::secret("token", Some("do-not-persist".into()));
        let source = toml::to_string(&variable).unwrap();
        assert!(source.contains("name = \"token\""));
        assert!(source.contains("secret = true"));
        assert!(!source.contains("do-not-persist"));
        let restored: EnvironmentVariable = toml::from_str(&source).unwrap();
        assert_eq!(restored.value, None);
    }
}
