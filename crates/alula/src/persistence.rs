use std::{
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
    #[serde(default)]
    pub requests: Vec<RequestDraft>,
    #[serde(default)]
    pub variables: Vec<EnvironmentVariable>,
}

impl Environment {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            id: next_id("environment"),
            name: name.into(),
            requests: Vec::new(),
            variables: Vec::new(),
        }
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
    pub fn create(&mut self, name: impl Into<String>) -> String {
        let environment = Environment::new(name);
        let id = environment.id.clone();
        self.environments.push(environment);
        id
    }

    pub fn environment_for_request(&self, request_id: &str) -> Option<&Environment> {
        self.environments.iter().find(|environment| {
            environment
                .requests
                .iter()
                .any(|request| request.id == request_id)
        })
    }

    pub fn environment_for_request_mut(&mut self, request_id: &str) -> Option<&mut Environment> {
        self.environments.iter_mut().find(|environment| {
            environment
                .requests
                .iter()
                .any(|request| request.id == request_id)
        })
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
        if !self
            .environments
            .iter()
            .any(|environment| environment.id == environment_id)
        {
            anyhow::bail!("environment not found");
        }
        self.remove_request(&request.id);
        let environment = self
            .environments
            .iter_mut()
            .find(|environment| environment.id == environment_id)
            .expect("environment existence checked");
        environment.requests.push(request);
        Ok(())
    }

    pub fn remove_request(&mut self, request_id: &str) -> bool {
        let mut removed = false;
        for environment in &mut self.environments {
            let previous_len = environment.requests.len();
            environment
                .requests
                .retain(|request| request.id != request_id);
            removed |= previous_len != environment.requests.len();
        }
        removed
    }

    pub fn sync_open_requests(&mut self, requests: &[RequestDraft]) {
        for environment in &mut self.environments {
            for saved in &mut environment.requests {
                if let Some(open) = requests.iter().find(|request| request.id == saved.id) {
                    *saved = open.clone();
                }
            }
        }
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
                    .map_err(|_| anyhow!("environment loader panicked"))??,
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
