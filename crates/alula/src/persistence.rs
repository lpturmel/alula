use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, anyhow};
use serde::{Deserialize, Serialize, Serializer, de::DeserializeOwned, ser::SerializeStruct};

use crate::model::{RequestDraft, ResponseSnapshot, Workspace, next_id};

pub const STATE_VERSION: u32 = 1;
pub const MAX_HISTORY_ENTRIES: usize = 500;

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

    pub fn sync_open_requests(&mut self, requests: &[RequestDraft]) {
        for saved in &mut self.requests {
            if let Some(open) = requests.iter().find(|request| request.id == saved.id) {
                *saved = open.clone();
            }
        }
        for folder in &mut self.folders {
            folder.sync_open_requests(requests);
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
        if !self
            .environments
            .iter()
            .any(|environment| environment.id == environment_id)
        {
            anyhow::bail!("environment not found");
        }
        if let Some(folder_id) = folder_id
            && !self.environments.iter().any(|environment| {
                environment.id == environment_id && environment.find_folder(folder_id).is_some()
            })
        {
            anyhow::bail!("folder not found");
        }
        self.remove_request(&request.id);
        let environment = self
            .environments
            .iter_mut()
            .find(|environment| environment.id == environment_id)
            .expect("environment existence checked");
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
        for environment in &mut self.environments {
            for saved in &mut environment.requests {
                if let Some(open) = requests.iter().find(|request| request.id == saved.id) {
                    *saved = open.clone();
                }
            }
            for folder in &mut environment.folders {
                folder.sync_open_requests(requests);
            }
        }
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
    pub entries: Arc<Vec<HistoryEntry>>,
}

impl Default for HistoryStore {
    fn default() -> Self {
        Self {
            version: STATE_VERSION,
            entries: Arc::new(Vec::new()),
        }
    }
}

impl HistoryStore {
    pub fn push(&mut self, entry: HistoryEntry) {
        let entries = Arc::make_mut(&mut self.entries);
        entries.insert(0, entry);
        entries.truncate(MAX_HISTORY_ENTRIES);
    }

    pub fn remove(&mut self, history_id: &str) -> bool {
        let entries = Arc::make_mut(&mut self.entries);
        let previous_len = entries.len();
        entries.retain(|entry| entry.id != history_id);
        entries.len() != previous_len
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

pub fn load_or_default<T: DeserializeOwned + Default>(path: &Path) -> Result<T> {
    if !path.exists() {
        return Ok(T::default());
    }
    let source =
        fs::read_to_string(path).with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&source).with_context(|| format!("failed to parse {}", path.display()))
}

pub fn save_toml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let source = toml::to_string_pretty(value).context("failed to serialize persisted state")?;
    let temporary = path.with_extension("toml.tmp");
    fs::write(&temporary, source)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, path)
        .with_context(|| format!("failed to replace {}", path.display()))?;
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
