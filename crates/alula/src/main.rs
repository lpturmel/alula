#![recursion_limit = "256"]

use alula::{
    AddHeader, AddParameter, AgentReply, AppConfig, CloseTab, CopyResponseBody, CreateNew,
    EnvironmentAgentCommand, EnvironmentStore, EnvironmentVariable, FocusUrl, HistoryAgentCommand,
    HistoryEntry, HistoryStore, HttpMethod, HttpSession, HttpStreamEvent, KeyValueField,
    McpHttpServer, McpToolHandler, NextTab, OpenCommandPalette, OpenSettings, PersistedState,
    PreviousTab, RequestDraft, ResponseBodyCache, ResponseSnapshot, SendRequest, SettingsView,
    ShowBody, ShowEnvironments, ShowFormattedResponse, ShowHeaders, ShowHistory, ShowParameters,
    ShowRawResponse, ShowRequests, StatePaths, ThemeAgentCommand, WebSocketDirection,
    WebSocketExecutor, WebSocketMessageSnapshot, WebSocketStreamEvent, Workspace,
    apply_environment_agent_command, apply_history_agent_command, apply_theme,
    apply_theme_agent_command, chunked_fenced_code_blocks, config_path, configured_key_bindings,
    delete_secret, inspect_template, install_tls_crypto_provider, is_websocket_request,
    load_secret, reply_to_tool, resolve_request, store_secret, syntax_language,
    trim_response_formatting_start, valid_variable_name,
};
use anyhow::Result;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Colorize as _, Icon, IconName, IndexPath, Root, Selectable as _,
    Sizable as _, WindowExt as _,
    animation::cubic_bezier,
    button::{Button, ButtonCustomVariant, ButtonVariant, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::DialogButtonProps,
    highlighter::{LanguageConfig, LanguageRegistry},
    input::{CompletionProvider, Input, InputEvent, InputState, RopeExt as _},
    label::Label,
    menu::{ContextMenuExt as _, PopupMenuItem},
    notification::Notification,
    scroll::ScrollableElement as _,
    select::{Select, SelectEvent, SelectItem, SelectState},
    tab::{Tab, TabBar},
    tag::Tag,
    text::{TextView, TextViewStyle},
};
use lsp_types::{
    CompletionContext, CompletionItem, CompletionItemKind, CompletionResponse, CompletionTextEdit,
    Range as LspRange, TextEdit,
};
use ropey::Rope;
use serde_json::{Value, json};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::VecDeque,
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, SystemTime},
};

/// gpui-components' editable code editor recomputes tree-sitter styles during
/// paint. Large editable request bodies use its multiline mode; response
/// highlighting uses the asynchronous, virtualized TextView path below.
const MAX_INTERACTIVE_SYNTAX_BYTES: usize = 32 * 1024;

/// Live highlighting stays bounded while the complete raw response continues
/// to stream. The completed response replaces this preview asynchronously.
const STREAM_HIGHLIGHT_PREVIEW_BYTES: usize = 256 * 1024;
/// Publishing every network fragment clones the accumulated markdown into a
/// new `SharedString`, turning a streamed response into quadratic copying.
/// Keep the first paint immediate, then publish at coarse visual increments.
const STREAM_HIGHLIGHT_PUBLISH_INTERVAL_BYTES: usize = 32 * 1024;
const STREAM_EVENT_BUFFER: usize = 8;
/// Drain a small burst in one application update so fast streams cannot make
/// input, scrolling, or window movement wait behind one UI task per frame.
const STREAM_UI_BATCH_SIZE: usize = 32;
const MAX_WEBSOCKET_MESSAGES: usize = 500;
const MAX_WEBSOCKET_TRANSCRIPT_BYTES: usize = 16 * 1024 * 1024;
const PERSISTENCE_POLL_INTERVAL: Duration = Duration::from_millis(150);
const PERSISTENCE_QUIET_PERIOD: Duration = Duration::from_millis(400);

fn formatting_stream_chunk(text: &str, formatted_bytes: usize) -> &str {
    if formatted_bytes == 0 {
        trim_response_formatting_start(text)
    } else {
        text
    }
}

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/arrow-right.svg" => Some(include_bytes!("../assets/icons/arrow-right.svg")),
            "icons/bot.svg" => Some(include_bytes!("../assets/icons/bot.svg")),
            "icons/check.svg" => Some(include_bytes!("../assets/icons/check.svg")),
            "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
            "icons/close.svg" => Some(include_bytes!("../assets/icons/close.svg")),
            "icons/copy.svg" => Some(include_bytes!("../assets/icons/copy.svg")),
            "icons/globe.svg" => Some(include_bytes!("../assets/icons/globe.svg")),
            "icons/loader-circle.svg" => Some(include_bytes!("../assets/icons/loader-circle.svg")),
            "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
            "icons/palette.svg" => Some(include_bytes!("../assets/icons/palette.svg")),
            "icons/redo-2.svg" => Some(include_bytes!("../assets/icons/redo-2.svg")),
            "icons/settings.svg" => Some(include_bytes!("../assets/icons/settings.svg")),
            "icons/settings-2.svg" => Some(include_bytes!("../assets/icons/settings-2.svg")),
            "icons/square-terminal.svg" => {
                Some(include_bytes!("../assets/icons/square-terminal.svg"))
            }
            _ => None,
        };
        Ok(bytes.map(Cow::Borrowed))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        if path != "icons" {
            return Ok(Vec::new());
        }
        Ok([
            "arrow-right.svg",
            "bot.svg",
            "check.svg",
            "chevron-down.svg",
            "close.svg",
            "copy.svg",
            "globe.svg",
            "loader-circle.svg",
            "plus.svg",
            "palette.svg",
            "redo-2.svg",
            "settings.svg",
            "settings-2.svg",
            "square-terminal.svg",
        ]
        .into_iter()
        .map(SharedString::from)
        .collect())
    }
}

#[derive(Debug, Clone)]
struct MethodOption(HttpMethod);

impl SelectItem for MethodOption {
    type Value = HttpMethod;

    fn title(&self) -> SharedString {
        self.0.as_str().into()
    }

    fn render(&self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(method_color(self.0, cx))
            .child(self.0.as_str())
    }

    fn value(&self) -> &Self::Value {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorSection {
    Parameters,
    Headers,
    Body,
}

impl EditorSection {
    const ALL: [Self; 3] = [Self::Parameters, Self::Headers, Self::Body];

    fn label(self) -> &'static str {
        match self {
            Self::Parameters => "Parameters",
            Self::Headers => "Headers",
            Self::Body => "Body",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResponseViewMode {
    Formatted,
    Raw,
    Headers,
    Messages,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebSocketConnectionState {
    Connecting,
    Open,
    Stopping,
    Closed,
    Failed,
}

impl WebSocketConnectionState {
    fn label(self) -> &'static str {
        match self {
            Self::Connecting => "Connecting…",
            Self::Open => "Connected",
            Self::Stopping => "Stopping…",
            Self::Closed => "Closed",
            Self::Failed => "Disconnected",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceSection {
    Requests,
    Environments,
    History,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EnvironmentDetailTab {
    Requests,
    Variables,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CreationDestination {
    Request,
    Environment,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaletteCommand {
    SendRequest,
    NewRequest,
    SwitchEnvironment,
    DeleteEnvironment,
    DeleteHistoryEntry,
    OpenSettings,
}

impl PaletteCommand {
    const ALL: [Self; 6] = [
        Self::SendRequest,
        Self::NewRequest,
        Self::SwitchEnvironment,
        Self::DeleteEnvironment,
        Self::DeleteHistoryEntry,
        Self::OpenSettings,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::SendRequest => "Send current request",
            Self::NewRequest => "Create new request",
            Self::SwitchEnvironment => "Switch environment",
            Self::DeleteEnvironment => "Delete environment…",
            Self::DeleteHistoryEntry => "Delete history entry…",
            Self::OpenSettings => "Open settings",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::SendRequest => "↗",
            Self::NewRequest => "＋",
            Self::SwitchEnvironment => "◎",
            Self::DeleteEnvironment | Self::DeleteHistoryEntry => "⌫",
            Self::OpenSettings => "✦",
        }
    }

    fn shortcut(self) -> &'static str {
        match self {
            Self::SendRequest => "⌘ ↵",
            Self::NewRequest => "⌘ N",
            Self::SwitchEnvironment => "⌘ E",
            Self::DeleteEnvironment | Self::DeleteHistoryEntry => "",
            Self::OpenSettings => "⌘ ,",
        }
    }

    fn matches(self, query: &str) -> bool {
        let query = query.trim().to_ascii_lowercase();
        query.is_empty() || self.label().to_ascii_lowercase().contains(&query)
    }
}

fn previous_palette_index(selected: usize, count: usize) -> Option<usize> {
    (count > 0).then(|| {
        if selected == 0 {
            count - 1
        } else {
            selected - 1
        }
    })
}

fn next_palette_index(selected: usize, count: usize) -> Option<usize> {
    (count > 0).then(|| (selected + 1) % count)
}

#[cfg(test)]
mod command_palette_tests {
    use super::{
        HttpStreamEvent, PaletteCommand, RequestStreamEvent, Rope, formatting_stream_chunk,
        next_palette_index, open_variable_at_cursor, open_variable_at_rope_cursor,
        previous_palette_index, push_stream_event_batch, redact_secret_values,
    };

    #[test]
    fn command_filtering_is_case_insensitive_and_matches_labels() {
        assert!(PaletteCommand::NewRequest.matches("NEW"));
        assert!(PaletteCommand::SwitchEnvironment.matches("environment"));
        assert!(PaletteCommand::DeleteEnvironment.matches("delete environment"));
        assert!(PaletteCommand::DeleteHistoryEntry.matches("history"));
        assert!(!PaletteCommand::OpenSettings.matches("send"));
        assert!(
            PaletteCommand::ALL
                .into_iter()
                .all(|command| command.matches(""))
        );
    }

    #[test]
    fn arrow_navigation_wraps_and_handles_empty_results() {
        assert_eq!(next_palette_index(0, 4), Some(1));
        assert_eq!(next_palette_index(3, 4), Some(0));
        assert_eq!(previous_palette_index(0, 4), Some(3));
        assert_eq!(previous_palette_index(2, 4), Some(1));
        assert_eq!(next_palette_index(0, 0), None);
        assert_eq!(previous_palette_index(0, 0), None);
    }

    #[test]
    fn streaming_formatter_skips_layout_whitespace_before_first_content() {
        assert_eq!(formatting_stream_chunk("\n \t\r", 0), "");
        assert_eq!(formatting_stream_chunk("\n \tfirst", 0), "first");
        assert_eq!(formatting_stream_chunk("\nnext", 5), "\nnext");
    }

    #[test]
    fn adjacent_http_chunks_are_coalesced_before_reaching_the_ui() {
        let mut batch = Vec::new();
        push_stream_event_batch(
            &mut batch,
            RequestStreamEvent::Http(HttpStreamEvent::BodyChunk {
                text: "hello ".into(),
                total_bytes: 6,
            }),
        );
        push_stream_event_batch(
            &mut batch,
            RequestStreamEvent::Http(HttpStreamEvent::BodyChunk {
                text: "world".into(),
                total_bytes: 11,
            }),
        );

        assert_eq!(batch.len(), 1);
        let RequestStreamEvent::Http(HttpStreamEvent::BodyChunk { text, total_bytes }) = &batch[0]
        else {
            panic!("expected a coalesced HTTP body chunk");
        };
        assert_eq!(text, "hello world");
        assert_eq!(*total_bytes, 11);
    }

    #[test]
    fn variable_completion_only_opens_inside_a_placeholder() {
        assert_eq!(open_variable_at_cursor("https://{{ba", 12), Some(8));
        assert_eq!(open_variable_at_cursor("https://example.com", 19), None);
        assert_eq!(open_variable_at_cursor("{{base}}/", 9), None);
    }

    #[test]
    fn variable_completion_reads_only_the_rope_suffix() {
        let prefix = "x".repeat(1_000_000);
        let rope = Rope::from(format!("{prefix}{{{{ba"));
        assert_eq!(
            open_variable_at_rope_cursor(&rope, rope.len()),
            Some(prefix.len())
        );
    }

    #[test]
    fn secret_values_are_removed_from_transport_errors() {
        assert_eq!(
            redact_secret_values(
                "failed to request https://example.com?token=s3cr3t",
                &["s3cr3t".into()],
            ),
            "failed to request https://example.com?token=••••••"
        );
    }
}

fn creation_destination(section: WorkspaceSection) -> CreationDestination {
    match section {
        WorkspaceSection::Environments => CreationDestination::Environment,
        WorkspaceSection::Requests | WorkspaceSection::History => CreationDestination::Request,
    }
}

#[derive(Clone)]
struct ComponentKeyBindings(Vec<KeyBinding>);

impl Global for ComponentKeyBindings {}

fn install_key_bindings(config: &AppConfig, cx: &mut App) {
    let component_bindings = cx.global::<ComponentKeyBindings>().0.clone();
    cx.clear_key_bindings();
    cx.bind_keys(component_bindings);
    cx.bind_keys(configured_key_bindings(config));
}

#[derive(Clone)]
struct PairInputs {
    id: String,
    enabled: bool,
    key: Entity<InputState>,
    value: Entity<InputState>,
    key_template_state: Rc<RefCell<TemplateVisualState>>,
    value_template_state: Rc<RefCell<TemplateVisualState>>,
}

impl PairInputs {
    fn from_field(
        field: KeyValueField,
        request_id: &str,
        variable_names: &Rc<RefCell<Vec<(String, bool)>>>,
        window: &mut Window,
        cx: &mut Context<AlulaApp>,
    ) -> Self {
        let KeyValueField {
            id,
            enabled,
            key: key_value,
            value: value_value,
        } = field;
        let key_template_state = Rc::new(RefCell::new(template_visual_state(&key_value, None)));
        let value_template_state = Rc::new(RefCell::new(template_visual_state(&value_value, None)));
        let key_names = variable_names.clone();
        let key = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder("Key")
                .default_value(key_value);
            state.lsp.completion_provider =
                Some(Rc::new(VariableCompletionProvider::new(key_names)));
            state
        });
        let value_names = variable_names.clone();
        let value = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder("Value")
                .default_value(value_value);
            state.lsp.completion_provider =
                Some(Rc::new(VariableCompletionProvider::new(value_names)));
            state
        });
        for (input, visual_state) in [
            (&key, key_template_state.clone()),
            (&value, value_template_state.clone()),
        ] {
            let request_id = request_id.to_owned();
            cx.subscribe(input, move |this, input, event: &InputEvent, cx| {
                if !matches!(event, InputEvent::Change) {
                    return;
                }
                let value = input.read(cx).value();
                let environment = this.environments.environment_for_request(&request_id);
                *visual_state.borrow_mut() = template_visual_state(value.as_ref(), environment);
                this.persistence_dirty.store(true, Ordering::Release);
                cx.notify();
            })
            .detach();
        }
        Self {
            id,
            enabled,
            key,
            value,
            key_template_state,
            value_template_state,
        }
    }

    fn empty(
        request_id: &str,
        variable_names: &Rc<RefCell<Vec<(String, bool)>>>,
        window: &mut Window,
        cx: &mut Context<AlulaApp>,
    ) -> Self {
        Self::from_field(
            KeyValueField::empty(),
            request_id,
            variable_names,
            window,
            cx,
        )
    }

    fn refresh_template_state(&self, environment: Option<&alula::Environment>, cx: &App) {
        *self.key_template_state.borrow_mut() =
            template_visual_state(self.key.read(cx).value().as_ref(), environment);
        *self.value_template_state.borrow_mut() =
            template_visual_state(self.value.read(cx).value().as_ref(), environment);
    }

    fn to_field(&self, cx: &App) -> KeyValueField {
        KeyValueField {
            id: self.id.clone(),
            enabled: self.enabled,
            key: self.key.read(cx).value().to_string(),
            value: self.value.read(cx).value().to_string(),
        }
    }
}

struct WebSocketSession {
    state: WebSocketConnectionState,
    messages: VecDeque<WebSocketMessageSnapshot>,
    selected_sequence: Option<u64>,
    inspector: Entity<InputState>,
    captured_bytes: usize,
    dropped_messages: usize,
    detail: Option<String>,
}

impl WebSocketSession {
    fn new(window: &mut Window, cx: &mut Context<AlulaApp>) -> Self {
        Self {
            state: WebSocketConnectionState::Connecting,
            messages: VecDeque::new(),
            selected_sequence: None,
            inspector: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .soft_wrap(false)
            }),
            captured_bytes: 0,
            dropped_messages: 0,
            detail: None,
        }
    }

    fn push(&mut self, message: WebSocketMessageSnapshot) {
        self.captured_bytes = self.captured_bytes.saturating_add(message.body.len());
        while self.messages.len() >= MAX_WEBSOCKET_MESSAGES
            || (self.captured_bytes > MAX_WEBSOCKET_TRANSCRIPT_BYTES && !self.messages.is_empty())
        {
            if let Some(removed) = self.messages.pop_front() {
                self.captured_bytes = self.captured_bytes.saturating_sub(removed.body.len());
                self.dropped_messages = self.dropped_messages.saturating_add(1);
            }
        }
        self.selected_sequence = Some(message.sequence);
        self.messages.push_back(message);
    }

    fn sync_selected_inspector(&mut self, window: &mut Window, cx: &mut Context<AlulaApp>) {
        let Some(message) = self.selected() else {
            return;
        };
        let body = message.body.clone();
        self.inspector.update(cx, |inspector, cx| {
            inspector.set_value(body, window, cx);
        });
    }

    fn select(&mut self, sequence: u64, window: &mut Window, cx: &mut Context<AlulaApp>) {
        let Some(message) = self
            .messages
            .iter()
            .find(|message| message.sequence == sequence)
        else {
            return;
        };
        self.selected_sequence = Some(sequence);
        self.inspector.update(cx, |inspector, cx| {
            inspector.set_value(message.body.clone(), window, cx);
        });
    }

    fn selected(&self) -> Option<&WebSocketMessageSnapshot> {
        let selected = self.selected_sequence?;
        self.messages
            .iter()
            .find(|message| message.sequence == selected)
    }
}

enum RequestStreamEvent {
    Http(HttpStreamEvent),
    WebSocket(WebSocketStreamEvent),
}

fn push_stream_event_batch(batch: &mut Vec<RequestStreamEvent>, event: RequestStreamEvent) {
    match (batch.last_mut(), event) {
        (
            Some(RequestStreamEvent::Http(HttpStreamEvent::BodyChunk {
                text: buffered,
                total_bytes: buffered_total,
            })),
            RequestStreamEvent::Http(HttpStreamEvent::BodyChunk { text, total_bytes }),
        ) => {
            buffered.push_str(&text);
            *buffered_total = total_bytes;
        }
        (_, event) => batch.push(event),
    }
}

enum RequestExecutionResult {
    Http(ResponseBodyCache, ResponseSnapshot),
    WebSocket(ResponseSnapshot),
}

struct RequestTab {
    draft: RequestDraft,
    title: SharedString,
    method: Entity<SelectState<Vec<MethodOption>>>,
    url: Entity<InputState>,
    body: Entity<InputState>,
    parameters: Vec<PairInputs>,
    headers: Vec<PairInputs>,
    section: EditorSection,
    response: Option<ResponseSnapshot>,
    response_editors: Option<ResponseEditors>,
    response_revision: u64,
    response_view: ResponseViewMode,
    error: Option<String>,
    sending: bool,
    websocket: Option<WebSocketSession>,
    cancellation: Option<Arc<AtomicBool>>,
    variable_names: Rc<RefCell<Vec<(String, bool)>>>,
    url_template_state: TemplateVisualState,
    body_template_state: TemplateVisualState,
}

#[derive(Clone)]
struct VariableCompletionProvider {
    variable_names: Rc<RefCell<Vec<(String, bool)>>>,
}

impl VariableCompletionProvider {
    fn new(variable_names: Rc<RefCell<Vec<(String, bool)>>>) -> Self {
        Self { variable_names }
    }
}

impl CompletionProvider for VariableCompletionProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        _: &mut Context<InputState>,
    ) -> Task<anyhow::Result<CompletionResponse>> {
        let Some(start) = open_variable_at_rope_cursor(text, offset) else {
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        };
        let range = LspRange {
            start: text.offset_to_position(start),
            end: text.offset_to_position(offset),
        };
        let items = self
            .variable_names
            .borrow()
            .iter()
            .map(|(name, secret)| {
                let syntax = format!("{{{{{name}}}}}");
                CompletionItem {
                    label: syntax.clone(),
                    detail: Some(if *secret {
                        "Secret environment variable".into()
                    } else {
                        "Environment variable".into()
                    }),
                    kind: Some(CompletionItemKind::VARIABLE),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                        range,
                        new_text: syntax,
                    })),
                    ..Default::default()
                }
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut Context<InputState>) -> bool {
        new_text.is_empty()
            || new_text.chars().all(|character| {
                character == '{'
                    || character == '_'
                    || character == '-'
                    || character == '.'
                    || character.is_ascii_alphanumeric()
            })
    }
}

fn open_variable_at_cursor(source: &str, offset: usize) -> Option<usize> {
    let before = source.get(..offset)?;
    let start = before.rfind("{{")?;
    if before[start + 2..].contains("}}") {
        return None;
    }
    let partial = &before[start + 2..];
    (partial.is_empty()
        || partial.chars().enumerate().all(|(index, character)| {
            if index == 0 {
                character == '_' || character.is_ascii_alphabetic()
            } else {
                character == '_'
                    || character == '-'
                    || character == '.'
                    || character.is_ascii_alphanumeric()
            }
        }))
    .then_some(start)
}

fn open_variable_at_rope_cursor(text: &Rope, offset: usize) -> Option<usize> {
    const MAX_COMPLETION_PREFIX_CHARS: usize = 128;

    let reversed = text
        .chars_at(offset)
        .reversed()
        .take(MAX_COMPLETION_PREFIX_CHARS)
        .collect::<String>();
    let suffix = reversed.chars().rev().collect::<String>();
    let local_start = open_variable_at_cursor(&suffix, suffix.len())?;
    Some(offset - (suffix.len() - local_start))
}

#[derive(Clone)]
enum TemplateVisualState {
    Plain,
    Valid,
    Error(String),
}

fn template_visual_state(
    source: &str,
    environment: Option<&alula::Environment>,
) -> TemplateVisualState {
    let inspection = inspect_template(source, environment);
    if let Some(error) = inspection.errors.first() {
        TemplateVisualState::Error(error.to_string())
    } else if inspection.is_valid_reference() {
        TemplateVisualState::Valid
    } else {
        TemplateVisualState::Plain
    }
}

fn redact_secret_values(message: &str, secret_values: &[String]) -> String {
    secret_values
        .iter()
        .filter(|value| !value.is_empty())
        .fold(message.to_owned(), |redacted, value| {
            redacted.replace(value, "••••••")
        })
}

struct ResponseEditors {
    formatted_text: SharedString,
    formatted_markdown: SharedString,
    formatted_markdown_buffer: String,
    formatted_published: bool,
    formatted_ready: bool,
    stream_content_type: Option<String>,
    stream_language: Option<&'static str>,
    highlighted_stream_bytes: usize,
    published_stream_bytes: usize,
    raw: Entity<InputState>,
    complete: bool,
}

impl ResponseEditors {
    fn new(cache: ResponseBodyCache, window: &mut Window, cx: &mut Context<AlulaApp>) -> Self {
        let formatted = cache.formatted.display;
        let formatted_markdown = SharedString::from(cache.formatted.markdown);
        let raw = cache.raw;
        let raw_text = SharedString::from(raw.text);
        Self {
            formatted_text: SharedString::from(formatted.text),
            formatted_markdown,
            formatted_markdown_buffer: String::new(),
            formatted_published: false,
            formatted_ready: false,
            stream_content_type: None,
            stream_language: None,
            highlighted_stream_bytes: 0,
            published_stream_bytes: 0,
            raw: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .soft_wrap(false)
                    .default_value(raw_text.clone())
            }),
            complete: true,
        }
    }

    fn streaming(
        content_type: Option<String>,
        window: &mut Window,
        cx: &mut Context<AlulaApp>,
    ) -> Self {
        Self {
            formatted_text: SharedString::default(),
            formatted_markdown: SharedString::default(),
            formatted_markdown_buffer: String::new(),
            formatted_published: false,
            formatted_ready: false,
            stream_content_type: content_type,
            stream_language: None,
            highlighted_stream_bytes: 0,
            published_stream_bytes: 0,
            raw: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .soft_wrap(false)
            }),
            complete: false,
        }
    }

    fn append_stream_chunk(&mut self, text: &str) {
        if self.highlighted_stream_bytes >= STREAM_HIGHLIGHT_PREVIEW_BYTES || text.is_empty() {
            return;
        }

        // Whitespace-only leading chunks are skipped until the first visible
        // response character arrives, keeping the live and completed layouts
        // aligned without altering the raw response.
        let text = formatting_stream_chunk(text, self.highlighted_stream_bytes);
        if text.is_empty() {
            return;
        }

        let language = *self.stream_language.get_or_insert_with(|| {
            let detected = syntax_language(self.stream_content_type.as_deref(), text);
            if detected == "text" {
                match text.trim_start().as_bytes().first() {
                    Some(b'{') | Some(b'[') => "json",
                    Some(b'<') => "html",
                    _ => detected,
                }
            } else {
                detected
            }
        });
        let available = STREAM_HIGHLIGHT_PREVIEW_BYTES - self.highlighted_stream_bytes;
        let mut end = text.len().min(available);
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        if end == 0 {
            return;
        }

        let fragment = chunked_fenced_code_blocks(language, &text[..end]);
        if !self.formatted_markdown_buffer.is_empty() && !fragment.is_empty() {
            self.formatted_markdown_buffer.push_str("\n\n");
        }
        self.formatted_markdown_buffer.push_str(&fragment);
        self.highlighted_stream_bytes += end;
        let publish = self.formatted_markdown.is_empty()
            || self
                .highlighted_stream_bytes
                .saturating_sub(self.published_stream_bytes)
                >= STREAM_HIGHLIGHT_PUBLISH_INTERVAL_BYTES
            || self.highlighted_stream_bytes == STREAM_HIGHLIGHT_PREVIEW_BYTES;
        if publish {
            self.formatted_markdown = SharedString::from(self.formatted_markdown_buffer.clone());
            self.published_stream_bytes = self.highlighted_stream_bytes;
            self.formatted_ready = false;
        }
    }

    fn finish(&mut self, cache: ResponseBodyCache) {
        self.formatted_text = SharedString::from(cache.formatted.display.text);
        self.formatted_markdown = SharedString::from(cache.formatted.markdown);
        self.formatted_markdown_buffer = String::new();
        self.published_stream_bytes = 0;
        self.formatted_ready = false;
        self.complete = true;
    }
}

impl RequestTab {
    fn new(mut draft: RequestDraft, window: &mut Window, cx: &mut Context<AlulaApp>) -> Self {
        let variable_names = Rc::new(RefCell::new(Vec::new()));
        let title = SharedString::from(draft.display_name());
        let methods = HttpMethod::ALL
            .iter()
            .copied()
            .map(MethodOption)
            .collect::<Vec<_>>();
        let selected_method = HttpMethod::ALL
            .iter()
            .position(|method| *method == draft.method)
            .map(|row| IndexPath::default().row(row));
        let method = cx.new(|cx| SelectState::new(methods, selected_method, window, cx));
        let request_id = draft.id.clone();
        cx.subscribe(
            &method,
            move |this, _, event: &SelectEvent<Vec<MethodOption>>, cx| {
                let SelectEvent::Confirm(Some(value)) = event else {
                    return;
                };
                if let Some(tab) = this.tabs.iter_mut().find(|tab| tab.draft.id == request_id) {
                    tab.draft.method = *value;
                    this.persistence_dirty.store(true, Ordering::Release);
                    cx.notify();
                }
            },
        )
        .detach();
        let url_value = std::mem::take(&mut draft.url);
        let body_value = std::mem::take(&mut draft.body);
        let url_template_state = template_visual_state(&url_value, None);
        let body_template_state = template_visual_state(&body_value, None);
        let body_len = body_value.len();
        let parameter_values = std::mem::take(&mut draft.parameters);
        let header_values = std::mem::take(&mut draft.headers);
        let url_names = variable_names.clone();
        let url = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder("https://api.example.com/v1/resource")
                .default_value(url_value);
            state.lsp.completion_provider =
                Some(Rc::new(VariableCompletionProvider::new(url_names)));
            state
        });
        let body_names = variable_names.clone();
        let body = cx.new(|cx| {
            let state = InputState::new(window, cx);
            let mut state = if body_len <= MAX_INTERACTIVE_SYNTAX_BYTES {
                state.code_editor("json").line_number(false)
            } else {
                state.multi_line(true)
            };
            state = state.soft_wrap(false).default_value(body_value);
            state.lsp.completion_provider =
                Some(Rc::new(VariableCompletionProvider::new(body_names)));
            state
        });
        let url_request_id = draft.id.clone();
        cx.subscribe(&url, move |this, input, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let value = input.read(cx).value();
            let environment = this.environments.environment_for_request(&url_request_id);
            let state = template_visual_state(value.as_ref(), environment);
            if let Some(tab) = this
                .tabs
                .iter_mut()
                .find(|tab| tab.draft.id == url_request_id)
            {
                tab.url_template_state = state;
            }
            this.persistence_dirty.store(true, Ordering::Release);
            cx.notify();
        })
        .detach();
        let body_request_id = draft.id.clone();
        cx.subscribe(&body, move |this, input, event: &InputEvent, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let value = input.read(cx).value();
            let environment = this.environments.environment_for_request(&body_request_id);
            let state = template_visual_state(value.as_ref(), environment);
            if let Some(tab) = this
                .tabs
                .iter_mut()
                .find(|tab| tab.draft.id == body_request_id)
            {
                tab.body_template_state = state;
            }
            this.persistence_dirty.store(true, Ordering::Release);
            cx.notify();
        })
        .detach();
        let parameters = parameter_values
            .into_iter()
            .map(|field| PairInputs::from_field(field, &draft.id, &variable_names, window, cx))
            .collect();
        let headers = header_values
            .into_iter()
            .map(|field| PairInputs::from_field(field, &draft.id, &variable_names, window, cx))
            .collect();
        // The input entities take ownership of imported values. Keeping only the
        // lightweight metadata prevents a second full copy of large request bodies.
        Self {
            draft,
            title,
            method,
            url,
            body,
            parameters,
            headers,
            section: EditorSection::Parameters,
            response: None,
            response_editors: None,
            response_revision: 0,
            response_view: ResponseViewMode::Formatted,
            error: None,
            sending: false,
            websocket: None,
            cancellation: None,
            variable_names,
            url_template_state,
            body_template_state,
        }
    }

    fn set_environment_variables(&self, variables: &[EnvironmentVariable]) {
        *self.variable_names.borrow_mut() = variables
            .iter()
            .map(|variable| (variable.name.clone(), variable.secret))
            .collect();
    }

    fn refresh_environment(&mut self, environment: Option<&alula::Environment>, cx: &App) {
        self.set_environment_variables(
            environment
                .map(|environment| environment.variables.as_slice())
                .unwrap_or_default(),
        );
        self.url_template_state =
            template_visual_state(self.url.read(cx).value().as_ref(), environment);
        self.body_template_state =
            template_visual_state(self.body.read(cx).value().as_ref(), environment);
        for pair in self.parameters.iter().chain(&self.headers) {
            pair.refresh_template_state(environment, cx);
        }
    }

    fn snapshot(&self, cx: &App) -> RequestDraft {
        RequestDraft {
            id: self.draft.id.clone(),
            name: self.draft.name.clone(),
            method: self.draft.method,
            url: self.url.read(cx).value().to_string(),
            parameters: self
                .parameters
                .iter()
                .map(|pair| pair.to_field(cx))
                .collect(),
            headers: self.headers.iter().map(|pair| pair.to_field(cx)).collect(),
            body: self.body.read(cx).value().to_string(),
        }
    }

    fn websocket_hint(&self, cx: &App) -> bool {
        let url = self.url.read(cx).value();
        let direct_protocol = url.trim().split_once(':').is_some_and(|(scheme, _)| {
            scheme.eq_ignore_ascii_case("ws") || scheme.eq_ignore_ascii_case("wss")
        });
        direct_protocol
            || self.headers.iter().any(|header| {
                header.enabled
                    && header
                        .key
                        .read(cx)
                        .value()
                        .trim()
                        .eq_ignore_ascii_case("upgrade")
                    && header
                        .value
                        .read(cx)
                        .value()
                        .trim()
                        .eq_ignore_ascii_case("websocket")
            })
    }
}

struct AlulaApp {
    focus_handle: FocusHandle,
    new_request_focus: FocusHandle,
    sidebar_collapsed: bool,
    sidebar_hovered: Option<WorkspaceSection>,
    new_request_hovered: bool,
    command_hovered: bool,
    send_hovered: bool,
    copy_feedback_active: bool,
    copy_feedback_revision: u64,
    tabs: Vec<RequestTab>,
    active_tab: usize,
    request_tabs_scroll: ScrollHandle,
    theme_config: AppConfig,
    theme_path: PathBuf,
    theme_modified: Option<SystemTime>,
    workspace_section: WorkspaceSection,
    environments: EnvironmentStore,
    history: HistoryStore,
    state_paths: StatePaths,
    persistence_dirty: Arc<AtomicBool>,
    environment_name: Entity<InputState>,
    environment_search: Entity<InputState>,
    environment_request_search: Entity<InputState>,
    environment_variable_search: Entity<InputState>,
    selected_environment_id: Option<String>,
    environment_detail_tab: EnvironmentDetailTab,
    mcp_http: Option<McpHttpServer>,
    mcp_status: McpStatus,
    mcp_ui_tx: smol::channel::Sender<McpUiCall>,
    http_session: HttpSession,
}

struct CommandPaletteView {
    app: Entity<AlulaApp>,
    input: Entity<InputState>,
    selected: usize,
}

struct McpUiCall {
    name: String,
    arguments: Value,
    reply: mpsc::SyncSender<Value>,
}

#[derive(Clone)]
enum McpStatus {
    Ready { port: u16 },
    Error(SharedString),
}

impl CommandPaletteView {
    fn new(app: Entity<AlulaApp>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Type a command…"));
        cx.subscribe_in(
            &input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.selected = 0;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                _ => {}
            },
        )
        .detach();
        Self {
            app,
            input,
            selected: 0,
        }
    }

    fn filtered_commands(&self, cx: &App) -> Vec<PaletteCommand> {
        let query = self.input.read(cx).value();
        PaletteCommand::ALL
            .into_iter()
            .filter(|command| command.matches(query.as_ref()))
            .collect()
    }

    fn select_previous(&mut self, cx: &mut Context<Self>) {
        let count = self.filtered_commands(cx).len();
        if let Some(selected) = previous_palette_index(self.selected, count) {
            self.selected = selected;
            cx.notify();
        }
    }

    fn select_next(&mut self, cx: &mut Context<Self>) {
        let count = self.filtered_commands(cx).len();
        if let Some(selected) = next_palette_index(self.selected, count) {
            self.selected = selected;
            cx.notify();
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let commands = self.filtered_commands(cx);
        if let Some(command) = commands.get(self.selected).copied() {
            self.execute(command, window, cx);
        }
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.keystroke.key.as_str() {
            "up" => self.select_previous(cx),
            "down" => self.select_next(cx),
            "escape" => window.close_dialog(cx),
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    fn execute(&mut self, command: PaletteCommand, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            PaletteCommand::SendRequest => {
                self.app.update(cx, |this, cx| {
                    this.send_request(&ClickEvent::default(), window, cx)
                });
                window.close_dialog(cx);
            }
            PaletteCommand::NewRequest => {
                self.app.update(cx, |this, cx| {
                    this.add_request(&ClickEvent::default(), window, cx)
                });
                window.close_dialog(cx);
            }
            PaletteCommand::SwitchEnvironment => {
                self.app.update(cx, |this, cx| {
                    this.select_workspace_section(WorkspaceSection::Environments, cx)
                });
                window.close_dialog(cx);
            }
            PaletteCommand::DeleteEnvironment => {
                window.close_dialog(cx);
                self.app.update(cx, |this, cx| {
                    this.open_delete_environment_picker(window, cx)
                });
            }
            PaletteCommand::DeleteHistoryEntry => {
                window.close_dialog(cx);
                self.app
                    .update(cx, |this, cx| this.open_delete_history_picker(window, cx));
            }
            PaletteCommand::OpenSettings => {
                window.close_dialog(cx);
                self.app.update(cx, |this, cx| {
                    this.open_settings(&ClickEvent::default(), window, cx)
                });
            }
        }
    }
}

impl Render for CommandPaletteView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let commands = self.filtered_commands(cx);
        self.selected = self.selected.min(commands.len().saturating_sub(1));
        let palette = cx.entity();
        let mut items = div().pt(px(7.)).flex().flex_col().gap(px(2.));

        if commands.is_empty() {
            items = items.child(
                div()
                    .h(px(42.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(11.))
                    .text_color(cx.theme().muted_foreground)
                    .child("No commands found"),
            );
        } else {
            for (index, command) in commands.into_iter().enumerate() {
                let selected = index == self.selected;
                let hover_palette = palette.clone();
                items = items.child(
                    div()
                        .id(SharedString::from(format!("palette-command-{command:?}")))
                        .h(px(34.))
                        .px(px(9.))
                        .flex()
                        .items_center()
                        .gap(px(9.))
                        .rounded(px(7.))
                        .text_size(px(11.))
                        .text_color(if selected {
                            cx.theme().foreground
                        } else {
                            cx.theme().muted_foreground
                        })
                        .when(selected, |this| this.bg(cx.theme().accent))
                        .when(!selected, |this| {
                            this.hover(|this| {
                                this.bg(cx.theme().secondary.lighten(0.1))
                                    .text_color(cx.theme().foreground)
                            })
                        })
                        .cursor_pointer()
                        .child(
                            div()
                                .w(px(14.))
                                .text_center()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(if selected {
                                    cx.theme().primary
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(command.glyph()),
                        )
                        .child(command.label())
                        .child(
                            div()
                                .ml_auto()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(10.))
                                .text_color(cx.theme().muted_foreground.opacity(0.68))
                                .child(command.shortcut()),
                        )
                        .on_hover(move |hovered, _, cx| {
                            if *hovered {
                                hover_palette.update(cx, |this, cx| {
                                    if this.selected != index {
                                        this.selected = index;
                                        cx.notify();
                                    }
                                });
                            }
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.execute(command, window, cx)
                        })),
                );
            }
        }

        div()
            .id("command-palette-content")
            .relative()
            .w_full()
            .flex()
            .flex_col()
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(
                div()
                    .h(px(39.))
                    .px_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Input::new(&self.input)
                            .appearance(false)
                            .focus_bordered(false)
                            .h_full()
                            .w_full(),
                    ),
            )
            .child(items)
            .with_animation(
                "command-palette-enter",
                Animation::new(Duration::from_secs_f64(0.18))
                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                |this, delta| this.opacity(delta).top(px(-7.) * (1.0 - delta)),
            )
    }
}

impl AlulaApp {
    fn new(
        theme_config: AppConfig,
        theme_path: PathBuf,
        persisted: PersistedState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let theme_modified = fs::metadata(&theme_path)
            .and_then(|value| value.modified())
            .ok();
        let active_tab = persisted
            .workspace
            .requests
            .iter()
            .position(|request| request.id == persisted.workspace.active_request_id)
            .unwrap_or(0);
        let mut tabs: Vec<_> = persisted
            .workspace
            .requests
            .into_iter()
            .map(|request| RequestTab::new(request, window, cx))
            .collect();
        for tab in &mut tabs {
            let environment = persisted
                .environments
                .environment_for_request(&tab.draft.id);
            tab.refresh_environment(environment, cx);
        }
        let focus_handle = cx.focus_handle();
        let new_request_focus = cx.focus_handle();
        focus_handle.focus(window);
        let (mcp_ui_tx, mcp_ui_rx) = smol::channel::unbounded();
        let (mcp_http, mcp_status) =
            Self::launch_mcp_http(theme_config.agent.port, &theme_path, mcp_ui_tx.clone());
        let environment_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search environments…"));
        let environment_request_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search requests…"));
        let environment_variable_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search variables…"));
        for input in [
            &environment_search,
            &environment_request_search,
            &environment_variable_search,
        ] {
            cx.subscribe(input, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    cx.notify();
                }
            })
            .detach();
        }
        let app = Self {
            focus_handle,
            new_request_focus,
            sidebar_collapsed: false,
            sidebar_hovered: None,
            new_request_hovered: false,
            command_hovered: false,
            send_hovered: false,
            copy_feedback_active: false,
            copy_feedback_revision: 0,
            tabs,
            active_tab,
            request_tabs_scroll: ScrollHandle::new(),
            theme_config,
            state_paths: StatePaths::beside(&theme_path),
            theme_path,
            theme_modified,
            workspace_section: WorkspaceSection::Requests,
            environments: persisted.environments,
            history: persisted.history,
            persistence_dirty: Arc::new(AtomicBool::new(false)),
            environment_name: cx
                .new(|cx| InputState::new(window, cx).placeholder("Production, Staging, Local…")),
            environment_search,
            environment_request_search,
            environment_variable_search,
            selected_environment_id: None,
            environment_detail_tab: EnvironmentDetailTab::Requests,
            mcp_http,
            mcp_status,
            mcp_ui_tx,
            http_session: HttpSession::new(),
        };
        Self::watch_theme_file(app.theme_path.clone(), app.theme_modified, cx);
        Self::watch_persistence(app.persistence_dirty.clone(), cx);
        Self::watch_mcp_calls(mcp_ui_rx, window, cx);
        app.hydrate_secrets_in_background(cx);
        app
    }

    fn hydrate_secrets_in_background(&self, cx: &mut Context<Self>) {
        let accounts = self
            .environments
            .environments
            .iter()
            .flat_map(|environment| {
                environment
                    .variables
                    .iter()
                    .filter(|variable| variable.secret && variable.value.is_none())
                    .map(|variable| (environment.id.clone(), variable.id.clone()))
            })
            .collect::<Vec<_>>();
        if accounts.is_empty() {
            return;
        }

        let hydration = cx.background_executor().spawn(async move {
            accounts
                .into_iter()
                .map(|(environment_id, variable_id)| {
                    let value = load_secret(&environment_id, &variable_id).ok().flatten();
                    (environment_id, variable_id, value)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let values = hydration.await;
            let _ = this.update(cx, |this, cx| {
                for (environment_id, variable_id, value) in values {
                    let Some(value) = value else {
                        continue;
                    };
                    if let Some(variable) = this
                        .environments
                        .environments
                        .iter_mut()
                        .find(|environment| environment.id == environment_id)
                        .and_then(|environment| {
                            environment
                                .variables
                                .iter_mut()
                                .find(|variable| variable.id == variable_id)
                        })
                        && variable.value.is_none()
                    {
                        variable.value = Some(value);
                    }
                }
                for tab in &mut this.tabs {
                    let environment = this.environments.environment_for_request(&tab.draft.id);
                    tab.refresh_environment(environment, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn launch_mcp_http(
        port: u16,
        config_path: &std::path::Path,
        ui_tx: smol::channel::Sender<McpUiCall>,
    ) -> (Option<McpHttpServer>, McpStatus) {
        let handler: McpToolHandler = Arc::new(move |name, arguments| {
            let (reply_tx, reply_rx) = mpsc::sync_channel(1);
            if ui_tx
                .send_blocking(McpUiCall {
                    name: name.to_owned(),
                    arguments,
                    reply: reply_tx,
                })
                .is_err()
            {
                return reply_to_tool(AgentReply::error("Alula UI is not available"));
            }
            reply_rx
                .recv_timeout(Duration::from_secs(30))
                .unwrap_or_else(|_| {
                    reply_to_tool(AgentReply::error(
                        "Alula UI did not answer the MCP tool call",
                    ))
                })
        });
        match McpHttpServer::start(port, config_path.to_path_buf(), Some(handler)) {
            Ok(server) => {
                let port = server.local_addr().port();
                (Some(server), McpStatus::Ready { port })
            }
            Err(error) => (
                None,
                McpStatus::Error(SharedString::from(format!(
                    "MCP port {port} unavailable: {error}"
                ))),
            ),
        }
    }

    fn restart_mcp_http(&mut self) {
        self.mcp_http.take();
        let (server, status) = Self::launch_mcp_http(
            self.theme_config.agent.port,
            &self.theme_path,
            self.mcp_ui_tx.clone(),
        );
        self.mcp_http = server;
        self.mcp_status = status;
    }

    fn watch_mcp_calls(
        receiver: smol::channel::Receiver<McpUiCall>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(call) = receiver.recv().await {
                let response = cx
                    .update(|window, cx| {
                        this.update(cx, |this, cx| {
                            this.handle_mcp_tool(&call.name, call.arguments, window, cx)
                        })
                    })
                    .and_then(|response| response)
                    .unwrap_or_else(|error| {
                        reply_to_tool(AgentReply::error(format!(
                            "Alula UI update failed: {error:#}"
                        )))
                    });
                let _ = call.reply.send(response);
            }
        })
        .detach();
    }

    fn handle_mcp_tool(
        &mut self,
        name: &str,
        arguments: Value,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Value {
        let reply = match name {
            "list_requests" => {
                let requests = self
                    .tabs
                    .iter()
                    .map(|tab| tab.snapshot(cx))
                    .collect::<Vec<_>>();
                AgentReply::success("requests listed", requests)
            }
            "create_request" => {
                let mut request = RequestDraft::default();
                if let Some(name) = arguments.get("name").and_then(Value::as_str) {
                    request.name = name.to_owned();
                }
                if let Some(method) = arguments.get("method").and_then(Value::as_str) {
                    let Some(method) = parse_http_method(method) else {
                        return reply_to_tool(AgentReply::error(
                            "method is not a supported HTTP method",
                        ));
                    };
                    request.method = method;
                }
                if let Some(url) = arguments.get("url").and_then(Value::as_str) {
                    request.url = url.to_owned();
                }
                let request_id = request.id.clone();
                self.show_request_tab(request, window, cx);
                AgentReply::success(
                    "request created and selected",
                    json!({ "request_id": request_id }),
                )
            }
            "update_request" => {
                let Some(request_id) = arguments.get("request_id").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("request_id must be a string"));
                };
                let Some(patch) = arguments.get("patch").and_then(Value::as_object) else {
                    return reply_to_tool(AgentReply::error("patch must be an object"));
                };
                let Some(index) = self.tabs.iter().position(|tab| tab.draft.id == request_id)
                else {
                    return reply_to_tool(AgentReply::error("request not found"));
                };
                let headers = match patch.get("headers").map(parse_key_value_fields).transpose() {
                    Ok(headers) => headers,
                    Err(error) => return reply_to_tool(AgentReply::error(error)),
                };
                let parameters = match patch
                    .get("parameters")
                    .map(parse_key_value_fields)
                    .transpose()
                {
                    Ok(parameters) => parameters,
                    Err(error) => return reply_to_tool(AgentReply::error(error)),
                };
                let tab = &mut self.tabs[index];
                if let Some(name) = patch.get("name").and_then(Value::as_str) {
                    tab.draft.name = name.to_owned();
                }
                if let Some(method) = patch.get("method").and_then(Value::as_str) {
                    let Some(method) = parse_http_method(method) else {
                        return reply_to_tool(AgentReply::error(
                            "method is not a supported HTTP method",
                        ));
                    };
                    tab.draft.method = method;
                    tab.method.update(cx, |state, cx| {
                        state.set_selected_value(&method, window, cx)
                    });
                }
                if let Some(url) = patch.get("url").and_then(Value::as_str) {
                    tab.url
                        .update(cx, |state, cx| state.set_value(url.to_owned(), window, cx));
                }
                if let Some(body) = patch.get("body").and_then(Value::as_str) {
                    tab.body
                        .update(cx, |state, cx| state.set_value(body.to_owned(), window, cx));
                }
                if let Some(headers) = headers {
                    let variable_names = tab.variable_names.clone();
                    let request_id = tab.draft.id.clone();
                    tab.headers = headers
                        .into_iter()
                        .map(|field| {
                            PairInputs::from_field(field, &request_id, &variable_names, window, cx)
                        })
                        .collect();
                }
                if let Some(parameters) = parameters {
                    let variable_names = tab.variable_names.clone();
                    let request_id = tab.draft.id.clone();
                    tab.parameters = parameters
                        .into_iter()
                        .map(|field| {
                            PairInputs::from_field(field, &request_id, &variable_names, window, cx)
                        })
                        .collect();
                }
                tab.title = SharedString::from(tab.snapshot(cx).display_name());
                self.active_tab = index;
                self.workspace_section = WorkspaceSection::Requests;
                self.persistence_dirty.store(true, Ordering::Release);
                cx.notify();
                AgentReply::success("request updated and selected", tab.snapshot(cx))
            }
            "send_request" => {
                let Some(request_id) = arguments.get("request_id").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("request_id must be a string"));
                };
                let Some(index) = self.tabs.iter().position(|tab| tab.draft.id == request_id)
                else {
                    return reply_to_tool(AgentReply::error("request not found"));
                };
                if self.tabs[index].sending {
                    AgentReply::error("request is already running")
                } else {
                    self.active_tab = index;
                    self.show_selected_request(cx);
                    self.send_request(&ClickEvent::default(), window, cx);
                    AgentReply::success(
                        "request started; inspect history after completion",
                        json!({ "request_id": request_id }),
                    )
                }
            }
            "list_environments" => {
                AgentReply::success("environments listed", &self.environments.environments)
            }
            "create_environment" => {
                let Some(name) = arguments.get("name").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("name must be a string"));
                };
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::CreateEnvironment {
                        name: name.to_owned(),
                    },
                );
                if reply.ok {
                    self.persistence_dirty.store(true, Ordering::Release);
                }
                reply
            }
            "delete_environment" => {
                let Some(environment_id) = arguments.get("environment_id").and_then(Value::as_str)
                else {
                    return reply_to_tool(AgentReply::error("environment_id must be a string"));
                };
                let affected_request_ids = self
                    .environments
                    .environments
                    .iter()
                    .find(|environment| environment.id == environment_id)
                    .map(|environment| {
                        environment
                            .requests
                            .iter()
                            .map(|request| request.id.clone())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::DeleteEnvironment {
                        environment_id: environment_id.to_owned(),
                    },
                );
                if reply.ok {
                    if self.selected_environment_id.as_deref() == Some(environment_id) {
                        self.selected_environment_id = None;
                    }
                    for request_id in affected_request_ids {
                        self.refresh_request_variable_names(&request_id, cx);
                    }
                    self.persistence_dirty.store(true, Ordering::Release);
                    cx.notify();
                }
                reply
            }
            "set_environment_variable" => {
                let Some(environment_id) = arguments.get("environment_id").and_then(Value::as_str)
                else {
                    return reply_to_tool(AgentReply::error("environment_id must be a string"));
                };
                let Some(name) = arguments.get("name").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("name must be a string"));
                };
                let Some(value) = arguments.get("value").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("value must be a string"));
                };
                if arguments
                    .get("secret")
                    .is_some_and(|value| !value.is_boolean())
                {
                    return reply_to_tool(AgentReply::error("secret must be a boolean"));
                }
                let environment_id = environment_id.to_owned();
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::SetVariable {
                        environment_id: environment_id.clone(),
                        name: name.to_owned(),
                        value: value.to_owned(),
                        secret: arguments.get("secret").and_then(Value::as_bool),
                    },
                );
                if reply.ok {
                    let scoped_request_ids = self
                        .environments
                        .environments
                        .iter()
                        .find(|environment| environment.id == environment_id)
                        .map(|environment| {
                            environment
                                .requests
                                .iter()
                                .map(|request| request.id.clone())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    for request_id in scoped_request_ids {
                        self.refresh_request_variable_names(&request_id, cx);
                    }
                    self.persistence_dirty.store(true, Ordering::Release);
                    cx.notify();
                }
                reply
            }
            "assign_request_to_environment" => {
                let Some(environment_id) = arguments.get("environment_id").and_then(Value::as_str)
                else {
                    return reply_to_tool(AgentReply::error("environment_id must be a string"));
                };
                let Some(request_id) = arguments.get("request_id").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("request_id must be a string"));
                };
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::AssignRequest {
                        environment_id: environment_id.to_owned(),
                        request_id: request_id.to_owned(),
                    },
                );
                if reply.ok {
                    self.refresh_request_variable_names(request_id, cx);
                }
                if reply.ok {
                    self.persistence_dirty.store(true, Ordering::Release);
                }
                reply
            }
            "remove_request_from_environment" => {
                let Some(request_id) = arguments.get("request_id").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("request_id must be a string"));
                };
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::RemoveRequest {
                        request_id: request_id.to_owned(),
                    },
                );
                if reply.ok {
                    self.refresh_request_variable_names(request_id, cx);
                }
                if reply.ok {
                    self.persistence_dirty.store(true, Ordering::Release);
                }
                reply
            }
            "list_history" => apply_history_agent_command(
                &mut self.history,
                HistoryAgentCommand::ListHistory {
                    limit: arguments
                        .get("limit")
                        .and_then(Value::as_u64)
                        .map(|limit| limit as usize),
                },
            ),
            "get_history_entry" => {
                let Some(history_id) = arguments.get("history_id").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("history_id must be a string"));
                };
                apply_history_agent_command(
                    &mut self.history,
                    HistoryAgentCommand::GetHistoryEntry {
                        history_id: history_id.to_owned(),
                    },
                )
            }
            "delete_history_entry" => {
                let Some(history_id) = arguments.get("history_id").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("history_id must be a string"));
                };
                let reply = apply_history_agent_command(
                    &mut self.history,
                    HistoryAgentCommand::DeleteHistoryEntry {
                        history_id: history_id.to_owned(),
                    },
                );
                if reply.ok {
                    self.persistence_dirty.store(true, Ordering::Release);
                    cx.notify();
                }
                reply
            }
            "get_theme" => apply_theme_agent_command(
                &mut self.theme_config,
                &self.theme_path,
                ThemeAgentCommand::GetTheme,
            ),
            "get_theme_schema" => apply_theme_agent_command(
                &mut self.theme_config,
                &self.theme_path,
                ThemeAgentCommand::GetThemeSchema,
            ),
            "preview_theme" | "save_theme" => {
                let Some(theme_toml) = arguments.get("theme_toml").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("theme_toml must be a string"));
                };
                let command = if name == "preview_theme" {
                    ThemeAgentCommand::PreviewTheme {
                        theme_toml: theme_toml.to_owned(),
                    }
                } else {
                    ThemeAgentCommand::SaveTheme {
                        theme_toml: theme_toml.to_owned(),
                    }
                };
                let reply =
                    apply_theme_agent_command(&mut self.theme_config, &self.theme_path, command);
                if reply.ok {
                    let _ = apply_theme(&self.theme_config, cx);
                    self.theme_modified = fs::metadata(&self.theme_path)
                        .and_then(|metadata| metadata.modified())
                        .ok();
                    cx.notify();
                }
                reply
            }
            "import_theme" => {
                let Some(path) = arguments.get("path").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("path must be a string"));
                };
                let reply = apply_theme_agent_command(
                    &mut self.theme_config,
                    &self.theme_path,
                    ThemeAgentCommand::ImportTheme {
                        path: PathBuf::from(path),
                        save: arguments.get("save").and_then(Value::as_bool),
                    },
                );
                if reply.ok {
                    let _ = apply_theme(&self.theme_config, cx);
                    cx.notify();
                }
                reply
            }
            _ => AgentReply::error(format!("unknown MCP tool: {name}")),
        };
        reply_to_tool(reply)
    }

    fn workspace_snapshot(&self, cx: &App) -> Workspace {
        let requests = self.tabs.iter().map(|tab| tab.snapshot(cx)).collect();
        Workspace {
            requests,
            active_request_id: self.tabs[self.active_tab].draft.id.clone(),
        }
    }

    fn watch_persistence(persistence_dirty: Arc<AtomicBool>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(PERSISTENCE_POLL_INTERVAL).await;
                // The common idle path never enters the app entity. This keeps
                // the UI executor free between actual input and paint work.
                if !persistence_dirty.swap(false, Ordering::AcqRel) {
                    continue;
                }

                Timer::after(PERSISTENCE_QUIET_PERIOD).await;
                let state = match this.update(cx, |this, cx| {
                    if persistence_dirty.load(Ordering::Acquire) {
                        return None;
                    }
                    let workspace = this.workspace_snapshot(cx);
                    this.environments.sync_open_requests(&workspace.requests);
                    Some((
                        PersistedState {
                            workspace,
                            history: this.history.clone(),
                            environments: this.environments.clone(),
                        },
                        this.state_paths.clone(),
                    ))
                }) {
                    Ok(state) => state,
                    Err(_) => break,
                };
                let Some((state, paths)) = state else {
                    continue;
                };
                cx.background_executor()
                    .spawn(async move {
                        if let Err(error) = state.save(&paths) {
                            eprintln!("could not persist Alula state: {error:#}");
                        }
                    })
                    .detach();
            }
        })
        .detach();
    }

    fn watch_theme_file(
        theme_path: PathBuf,
        mut observed_modified: Option<SystemTime>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_secs(1)).await;
                let path = theme_path.clone();
                let modified = cx
                    .background_executor()
                    .spawn(
                        async move { fs::metadata(path).and_then(|value| value.modified()).ok() },
                    )
                    .await;
                if modified.is_none() || modified == observed_modified {
                    continue;
                }
                observed_modified = modified;
                let path = theme_path.clone();
                let config = cx
                    .background_executor()
                    .spawn(async move { AppConfig::load(&path) })
                    .await;
                if this
                    .update(cx, |this, cx| {
                        this.theme_modified = modified;
                        if let Ok(config) = config {
                            this.theme_config = config.clone();
                            let _ = apply_theme(&config, cx);
                            install_key_bindings(&config, cx);
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn open_settings(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let config =
            AppConfig::load(&self.theme_path).unwrap_or_else(|_| self.theme_config.clone());
        let path = self.theme_path.clone();
        let settings = cx.new(|cx| SettingsView::new(config, path, window, cx));
        let viewport = window.viewport_size();
        let save = settings.clone();
        let restore = settings.clone();
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(Label::new("Settings").font_weight(FontWeight::SEMIBOLD))
                .w(viewport.width)
                .h(viewport.height)
                // Dialog entrance settles 30 px below its configured top.
                .margin_top(px(-30.))
                .p_4()
                .rounded_none()
                .border_0()
                .overlay(false)
                .child(settings.clone())
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Save settings")
                        .cancel_text("Cancel"),
                )
                .on_ok({
                    let save = save.clone();
                    move |_, _, cx| save.update(cx, |settings, cx| settings.save(cx))
                })
                .on_cancel({
                    let restore = restore.clone();
                    move |_, _, cx| restore.update(cx, |settings, cx| settings.restore(cx))
                })
                .on_close({
                    let app = app.clone();
                    move |_, _, cx| {
                        app.update(cx, |this, cx| this.reload_settings(cx));
                    }
                })
        });
        cx.notify();
    }

    fn reload_settings(&mut self, cx: &mut Context<Self>) {
        let path = config_path();
        if let Ok(config) = AppConfig::load(&path) {
            self.theme_path = path.clone();
            self.state_paths = StatePaths::beside(&path);
            self.theme_modified = fs::metadata(&path).and_then(|value| value.modified()).ok();
            self.theme_config = config.clone();
            let _ = apply_theme(&config, cx);
            install_key_bindings(&config, cx);
            self.restart_mcp_http();
        }
        cx.notify();
    }

    fn shortcuts_blocked(window: &mut Window, cx: &mut App) -> bool {
        window.has_active_dialog(cx)
    }

    fn shortcut_create_new(&mut self, _: &CreateNew, window: &mut Window, cx: &mut Context<Self>) {
        if Self::shortcuts_blocked(window, cx) {
            return;
        }
        match creation_destination(self.workspace_section) {
            CreationDestination::Environment => {
                self.open_environment_dialog(&ClickEvent::default(), window, cx)
            }
            CreationDestination::Request => self.add_request(&ClickEvent::default(), window, cx),
        }
    }

    fn shortcut_close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        if Self::shortcuts_blocked(window, cx)
            || self.workspace_section != WorkspaceSection::Requests
        {
            return;
        }
        self.close_request(self.active_tab, cx);
    }

    fn cycle_tab(&mut self, direction: isize, cx: &mut Context<Self>) {
        if self.tabs.len() > 1 {
            let length = self.tabs.len() as isize;
            self.active_tab = (self.active_tab as isize + direction).rem_euclid(length) as usize;
        }
        self.show_selected_request(cx);
    }

    fn shortcut_next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        if !Self::shortcuts_blocked(window, cx) {
            self.cycle_tab(1, cx);
        }
    }

    fn shortcut_previous_tab(
        &mut self,
        _: &PreviousTab,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.cycle_tab(-1, cx);
        }
    }

    fn shortcut_send_request(
        &mut self,
        _: &SendRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if Self::shortcuts_blocked(window, cx) {
            return;
        }
        self.workspace_section = WorkspaceSection::Requests;
        self.send_request(&ClickEvent::default(), window, cx);
    }

    fn show_editor_section(&mut self, section: EditorSection, cx: &mut Context<Self>) {
        self.workspace_section = WorkspaceSection::Requests;
        self.select_section(section, cx);
    }

    fn shortcut_show_parameters(
        &mut self,
        _: &ShowParameters,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.show_editor_section(EditorSection::Parameters, cx);
        }
    }

    fn shortcut_show_headers(
        &mut self,
        _: &ShowHeaders,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.show_editor_section(EditorSection::Headers, cx);
        }
    }

    fn shortcut_show_body(&mut self, _: &ShowBody, window: &mut Window, cx: &mut Context<Self>) {
        if !Self::shortcuts_blocked(window, cx) {
            self.show_editor_section(EditorSection::Body, cx);
        }
    }

    fn shortcut_copy_response(
        &mut self,
        _: &CopyResponseBody,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.copy_response(&ClickEvent::default(), window, cx);
        }
    }

    fn add_shortcut_pair(
        &mut self,
        section: EditorSection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_editor_section(section, cx);
        self.add_pair(window, cx);
    }

    fn shortcut_add_parameter(
        &mut self,
        _: &AddParameter,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.add_shortcut_pair(EditorSection::Parameters, window, cx);
        }
    }

    fn shortcut_add_header(&mut self, _: &AddHeader, window: &mut Window, cx: &mut Context<Self>) {
        if !Self::shortcuts_blocked(window, cx) {
            self.add_shortcut_pair(EditorSection::Headers, window, cx);
        }
    }

    fn shortcut_show_requests(
        &mut self,
        _: &ShowRequests,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.select_workspace_section(WorkspaceSection::Requests, cx);
        }
    }

    fn shortcut_show_environments(
        &mut self,
        _: &ShowEnvironments,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.select_workspace_section(WorkspaceSection::Environments, cx);
        }
    }

    fn shortcut_show_history(
        &mut self,
        _: &ShowHistory,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.select_workspace_section(WorkspaceSection::History, cx);
        }
    }

    fn shortcut_open_settings(
        &mut self,
        _: &OpenSettings,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.open_settings(&ClickEvent::default(), window, cx);
        }
    }

    fn shortcut_open_command_palette(
        &mut self,
        _: &OpenCommandPalette,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.open_command_menu(&ClickEvent::default(), window, cx);
        }
    }

    fn shortcut_focus_url(&mut self, _: &FocusUrl, window: &mut Window, cx: &mut Context<Self>) {
        if Self::shortcuts_blocked(window, cx) {
            return;
        }
        self.workspace_section = WorkspaceSection::Requests;
        self.tabs[self.active_tab]
            .url
            .update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    fn shortcut_show_formatted_response(
        &mut self,
        _: &ShowFormattedResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.workspace_section = WorkspaceSection::Requests;
            self.set_response_view(ResponseViewMode::Formatted, cx);
        }
    }

    fn shortcut_show_raw_response(
        &mut self,
        _: &ShowRawResponse,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !Self::shortcuts_blocked(window, cx) {
            self.workspace_section = WorkspaceSection::Requests;
            self.set_response_view(ResponseViewMode::Raw, cx);
        }
    }

    fn add_request(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs
            .push(RequestTab::new(RequestDraft::default(), window, cx));
        self.active_tab = self.tabs.len() - 1;
        self.request_tabs_scroll.scroll_to_item(self.active_tab);
        self.workspace_section = WorkspaceSection::Requests;
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn close_request(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.len() == 1 {
            return;
        }
        if let Some(cancellation) = self.tabs[index].cancellation.take() {
            cancellation.store(true, Ordering::Release);
        }
        self.tabs.remove(index);
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn select_workspace_section(&mut self, section: WorkspaceSection, cx: &mut Context<Self>) {
        self.workspace_section = section;
        cx.notify();
    }

    fn toggle_sidebar(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        cx.notify();
    }

    fn open_command_menu(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let app = cx.entity();
        let palette = cx.new(|cx| CommandPaletteView::new(app, window, cx));
        let focused_input = palette.read(cx).input.clone();
        // Dialog's built-in entrance settles 30 px below its configured top.
        // Offset that distance so the palette lands at the mockup's 15vh.
        let margin_top = window.viewport_size().height * 0.15 - px(30.);
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .w(px(520.))
                .margin_top(margin_top)
                .p_2()
                .close_button(false)
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.38))
                .child(palette.clone())
        });
        focused_input.update(cx, |input, cx| input.focus(window, cx));
    }

    #[allow(dead_code)]

    fn open_environment_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.environment_name.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        let input = self.environment_name.clone();
        let save_input = input.clone();
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(Label::new("New environment").font_weight(FontWeight::SEMIBOLD))
                .w(px(440.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            Label::new("Name")
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD),
                        )
                        .child(Input::new(&input).w_full()),
                )
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Create environment")
                        .cancel_text("Cancel"),
                )
                .on_ok({
                    let save_input = save_input.clone();
                    let app = app.clone();
                    move |_, _, cx| {
                        let name = save_input.read(cx).value().trim().to_owned();
                        if name.is_empty() {
                            return false;
                        }
                        app.update(cx, |this, cx| {
                            this.environments.create(name);
                            this.persistence_dirty.store(true, Ordering::Release);
                            cx.notify();
                        });
                        true
                    }
                })
        });
    }

    fn open_delete_environment_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let environments = self.environments.environments.clone();
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let mut choices = div()
                .max_h(px(360.))
                .overflow_y_scrollbar()
                .flex()
                .flex_col()
                .gap_1();
            if environments.is_empty() {
                choices = choices.child(
                    Label::new("There are no environments to delete.")
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                );
            } else {
                for environment in &environments {
                    let row_app = app.clone();
                    let environment_id = environment.id.clone();
                    let environment_name = environment.name.clone();
                    let request_count = environment.requests.len();
                    choices = choices.child(
                        Button::new(SharedString::from(format!(
                            "palette-delete-environment-{environment_id}"
                        )))
                        .ghost()
                        .w_full()
                        .label(format!(
                            "{}  ·  {} request{}",
                            environment.name,
                            request_count,
                            if request_count == 1 { "" } else { "s" }
                        ))
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            row_app.update(cx, |this, cx| {
                                this.confirm_delete_environment(
                                    environment_id.clone(),
                                    environment_name.clone(),
                                    window,
                                    cx,
                                )
                            });
                        }),
                    );
                }
            }
            dialog
                .title(Label::new("Delete environment").font_weight(FontWeight::SEMIBOLD))
                .w(px(480.))
                .child(choices)
        });
    }

    fn confirm_delete_environment(
        &mut self,
        environment_id: String,
        environment_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let delete_app = app.clone();
            let delete_id = environment_id.clone();
            let deleted_name = environment_name.clone();
            dialog
                .title(Label::new("Delete environment?").font_weight(FontWeight::SEMIBOLD))
                .w(px(480.))
                .child(
                    Label::new(format!(
                        "“{environment_name}” and all of its saved requests, variables, and secrets will be permanently deleted."
                    ))
                    .text_sm(),
                )
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete environment")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel"),
                )
                .on_ok(move |_, window, cx| {
                    delete_app.update(cx, |this, cx| {
                        let affected_request_ids = this
                            .environments
                            .environments
                            .iter()
                            .find(|environment| environment.id == delete_id)
                            .map(|environment| {
                                environment
                                    .requests
                                    .iter()
                                    .map(|request| request.id.clone())
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        let workspace = this.workspace_snapshot(cx);
                        let reply = apply_environment_agent_command(
                            &workspace,
                            &mut this.environments,
                            EnvironmentAgentCommand::DeleteEnvironment {
                                environment_id: delete_id.clone(),
                            },
                        );
                        if reply.ok {
                            if this.selected_environment_id.as_deref() == Some(delete_id.as_str()) {
                                this.selected_environment_id = None;
                            }
                            for request_id in affected_request_ids {
                                this.refresh_request_variable_names(&request_id, cx);
                            }
                            this.persistence_dirty.store(true, Ordering::Release);
                            window.push_notification(
                                Notification::success(format!(
                                    "Deleted environment “{deleted_name}”"
                                )),
                                cx,
                            );
                            cx.notify();
                            true
                        } else {
                            window.push_notification(Notification::error(reply.message), cx);
                            false
                        }
                    })
                })
        });
    }

    fn open_delete_history_picker(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.history.entries.iter().cloned().collect::<Vec<_>>();
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let mut choices = div()
                .max_h(px(360.))
                .overflow_y_scrollbar()
                .flex()
                .flex_col()
                .gap_1();
            if entries.is_empty() {
                choices = choices.child(
                    Label::new("There are no history entries to delete.")
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                );
            } else {
                for entry in &entries {
                    let row_app = app.clone();
                    let history_id = entry.id.clone();
                    let display_name = entry.request.display_name();
                    choices = choices.child(
                        Button::new(SharedString::from(format!(
                            "palette-delete-history-{history_id}"
                        )))
                        .ghost()
                        .w_full()
                        .label(format!(
                            "{}  ·  {}  ·  {}",
                            entry.request.method.as_str(),
                            display_name,
                            relative_history_time(entry.sent_at_unix_ms)
                        ))
                        .on_click(move |_, window, cx| {
                            window.close_dialog(cx);
                            row_app.update(cx, |this, cx| {
                                this.confirm_delete_history_entry(
                                    history_id.clone(),
                                    display_name.clone(),
                                    window,
                                    cx,
                                )
                            });
                        }),
                    );
                }
            }
            dialog
                .title(Label::new("Delete history entry").font_weight(FontWeight::SEMIBOLD))
                .w(px(520.))
                .child(choices)
        });
    }

    fn confirm_delete_history_entry(
        &mut self,
        history_id: String,
        display_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let delete_app = app.clone();
            let delete_id = history_id.clone();
            dialog
                .title(Label::new("Delete history entry?").font_weight(FontWeight::SEMIBOLD))
                .w(px(460.))
                .child(
                    Label::new(format!(
                        "The recorded execution for “{display_name}” will be permanently deleted."
                    ))
                    .text_sm(),
                )
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Delete history entry")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("Cancel"),
                )
                .on_ok(move |_, window, cx| {
                    delete_app.update(cx, |this, cx| {
                        let reply = apply_history_agent_command(
                            &mut this.history,
                            HistoryAgentCommand::DeleteHistoryEntry {
                                history_id: delete_id.clone(),
                            },
                        );
                        if reply.ok {
                            this.persistence_dirty.store(true, Ordering::Release);
                            window.push_notification(
                                Notification::success("History entry deleted"),
                                cx,
                            );
                            cx.notify();
                            true
                        } else {
                            window.push_notification(Notification::error(reply.message), cx);
                            false
                        }
                    })
                })
        });
    }

    fn open_environment_variable_dialog(
        &mut self,
        environment_id: String,
        variable_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self
            .environments
            .environments
            .iter()
            .find(|environment| environment.id == environment_id)
            .and_then(|environment| {
                variable_id.as_ref().and_then(|variable_id| {
                    environment
                        .variables
                        .iter()
                        .find(|variable| variable.id == *variable_id)
                })
            })
            .cloned();
        let existing_name = existing
            .as_ref()
            .map(|variable| variable.name.clone())
            .unwrap_or_default();
        let existing_value = existing
            .as_ref()
            .and_then(|variable| variable.value.clone())
            .unwrap_or_default();
        let existing_secret = existing.as_ref().is_some_and(|variable| variable.secret);
        let name = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("api_url, token, account_id…")
                .default_value(existing_name.clone())
        });
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Value")
                .masked(existing_secret)
                .default_value(existing_value.clone())
        });
        let secret = Rc::new(Cell::new(existing_secret));
        let editing = existing.is_some();
        let save_name = name.clone();
        let save_value = value.clone();
        let save_secret = secret.clone();
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let save_name = save_name.clone();
            let save_value = save_value.clone();
            let save_secret = save_secret.clone();
            let toggle_value = value.clone();
            let toggle_secret = secret.clone();
            dialog
                .title(
                    Label::new(if editing { "Edit variable" } else { "New variable" })
                        .font_weight(FontWeight::SEMIBOLD),
                )
                .w(px(480.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(Label::new("Name").text_sm().font_weight(FontWeight::SEMIBOLD))
                                .child(Input::new(&name).w_full()),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(Label::new("Value").text_sm().font_weight(FontWeight::SEMIBOLD))
                                .child(Input::new(&value).w_full().mask_toggle()),
                        )
                        .child(
                            Checkbox::new("environment-variable-secret")
                                .small()
                                .checked(existing_secret)
                                .label("Store securely as secret")
                                .on_click(move |checked, window, cx| {
                                    toggle_secret.set(*checked);
                                    toggle_value.update(cx, |input, cx| {
                                        input.set_masked(*checked, window, cx)
                                    });
                                }),
                        )
                        .child(
                            Label::new("Use it in URLs, parameters, headers, or bodies as {{variable_name}}")
                                .text_xs()
                                .text_color(cx.theme().muted_foreground),
                        ),
                )
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(if editing { "Save variable" } else { "Add variable" })
                        .cancel_text("Cancel"),
                )
                .on_ok({
                    let app = app.clone();
                    let environment_id = environment_id.clone();
                    let variable_id = variable_id.clone();
                    move |_, window, cx| {
                        let variable_name = save_name.read(cx).value().trim().to_owned();
                        let variable_value = save_value.read(cx).value().to_string();
                        let secret = save_secret.get();
                        if !valid_variable_name(&variable_name) {
                            window.push_notification(
                                Notification::error("Variable names must start with a letter or underscore and contain only letters, numbers, _, -, or ."),
                                cx,
                            );
                            return false;
                        }
                        let result = app.update(cx, |this, cx| {
                            let Some(environment) = this
                                .environments
                                .environments
                                .iter_mut()
                                .find(|environment| environment.id == environment_id)
                            else {
                                anyhow::bail!("environment no longer exists");
                            };
                            if environment.variables.iter().any(|variable| {
                                variable.name == variable_name
                                    && variable_id.as_ref() != Some(&variable.id)
                            }) {
                                anyhow::bail!("a variable named `{variable_name}` already exists");
                            }
                            let old = variable_id.as_ref().and_then(|variable_id| {
                                environment.variables.iter().find(|variable| variable.id == *variable_id)
                            }).cloned();
                            let mut variable = if secret {
                                EnvironmentVariable::secret(variable_name, Some(variable_value))
                            } else {
                                EnvironmentVariable::public(variable_name, variable_value)
                            };
                            if let Some(variable_id) = variable_id.as_ref() {
                                variable.id = variable_id.clone();
                            }
                            let id = variable.id.clone();
                            if secret {
                                store_secret(
                                    &environment_id,
                                    &id,
                                    variable.value.as_deref().unwrap_or_default(),
                                )?;
                            } else if old.as_ref().is_some_and(|variable| variable.secret) {
                                delete_secret(&environment_id, &id)?;
                            }
                            if let Some(position) = environment.variables.iter().position(|item| item.id == id) {
                                environment.variables[position] = variable;
                            } else {
                                environment.variables.push(variable);
                            }
                            let scoped_request_ids = environment
                                .requests
                                .iter()
                                .map(|request| request.id.clone())
                                .collect::<Vec<_>>();
                            for request_id in scoped_request_ids {
                                this.refresh_request_variable_names(&request_id, cx);
                            }
                            this.persistence_dirty.store(true, Ordering::Release);
                            cx.notify();
                            Ok::<(), anyhow::Error>(())
                        });
                        match result {
                            Ok(()) => true,
                            Err(error) => {
                                window.push_notification(Notification::error(format!("Could not save variable: {error:#}")), cx);
                                false
                            }
                        }
                    }
                })
        });
    }

    fn remove_environment_variable(
        &mut self,
        environment_id: &str,
        variable_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(environment) = self
            .environments
            .environments
            .iter_mut()
            .find(|environment| environment.id == environment_id)
        else {
            return;
        };
        if let Some(variable) = environment
            .variables
            .iter()
            .find(|variable| variable.id == variable_id)
            .cloned()
            && variable.secret
            && let Err(error) = delete_secret(environment_id, variable_id)
        {
            window.push_notification(
                Notification::error(format!("Could not remove secret: {error:#}")),
                cx,
            );
            return;
        }
        environment
            .variables
            .retain(|variable| variable.id != variable_id);
        let scoped_request_ids = environment
            .requests
            .iter()
            .map(|request| request.id.clone())
            .collect::<Vec<_>>();
        for request_id in scoped_request_ids {
            self.refresh_request_variable_names(&request_id, cx);
        }
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn assign_request_to_environment(
        &mut self,
        request_id: &str,
        environment_id: &str,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.draft.id == request_id) else {
            return;
        };
        let request = tab.snapshot(cx);
        if self.environments.assign(environment_id, request).is_ok() {
            self.refresh_request_variable_names(request_id, cx);
            self.persistence_dirty.store(true, Ordering::Release);
            cx.notify();
        }
    }

    fn remove_request_from_environment(&mut self, request_id: &str, cx: &mut Context<Self>) {
        if self.environments.remove_request(request_id) {
            self.refresh_request_variable_names(request_id, cx);
            self.persistence_dirty.store(true, Ordering::Release);
            cx.notify();
        }
    }

    fn refresh_request_variable_names(&mut self, request_id: &str, cx: &App) {
        let environment = self.environments.environment_for_request(request_id);
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.draft.id == request_id) {
            tab.refresh_environment(environment, cx);
        }
    }

    fn show_request_tab(
        &mut self,
        request: RequestDraft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.draft.id == request.id) {
            self.active_tab = index;
        } else {
            self.tabs.push(RequestTab::new(request, window, cx));
            self.active_tab = self.tabs.len() - 1;
        }
        let request_id = self.tabs[self.active_tab].draft.id.clone();
        self.refresh_request_variable_names(&request_id, cx);
        self.workspace_section = WorkspaceSection::Requests;
        self.request_tabs_scroll.scroll_to_item(self.active_tab);
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn select_open_request(&mut self, request_id: &str) -> bool {
        let Some(index) = self.tabs.iter().position(|tab| tab.draft.id == request_id) else {
            return false;
        };
        self.active_tab = index;
        true
    }

    fn open_environment_request(
        &mut self,
        environment_id: &str,
        request_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.select_open_request(request_id) {
            self.show_selected_request(cx);
            return;
        }
        let request = self
            .environments
            .environments
            .iter()
            .find(|environment| environment.id == environment_id)
            .and_then(|environment| {
                environment
                    .requests
                    .iter()
                    .find(|request| request.id == request_id)
            })
            .cloned();
        if let Some(request) = request {
            self.show_request_tab(request, window, cx);
        }
    }

    fn open_history_request(
        &mut self,
        history_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let request_id = self
            .history
            .entries
            .iter()
            .find(|entry| entry.id == history_id)
            .map(|entry| entry.request.id.clone());
        if request_id
            .as_deref()
            .is_some_and(|request_id| self.select_open_request(request_id))
        {
            self.show_selected_request(cx);
            return;
        }
        let request = self
            .history
            .entries
            .iter()
            .find(|entry| entry.id == history_id)
            .map(|entry| entry.request.clone());
        if let Some(request) = request {
            self.show_request_tab(request, window, cx);
        }
    }

    fn show_selected_request(&mut self, cx: &mut Context<Self>) {
        self.workspace_section = WorkspaceSection::Requests;
        self.request_tabs_scroll.scroll_to_item(self.active_tab);
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn set_response_view(&mut self, mode: ResponseViewMode, cx: &mut Context<Self>) {
        self.tabs[self.active_tab].response_view = mode;
        cx.notify();
    }

    fn copy_response(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let tab = &self.tabs[self.active_tab];
        let Some(response) = &tab.response else {
            return;
        };
        let text = tab
            .response_editors
            .as_ref()
            .map(|editors| match (tab.response_view, editors.complete) {
                (ResponseViewMode::Formatted, true) => editors.formatted_text.to_string(),
                (ResponseViewMode::Headers, _) => response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                (ResponseViewMode::Messages, _) => tab
                    .websocket
                    .as_ref()
                    .map(|session| session.inspector.read(cx).value().to_string())
                    .unwrap_or_default(),
                _ => editors.raw.read(cx).value().to_string(),
            })
            .unwrap_or_else(|| match tab.response_view {
                ResponseViewMode::Headers => response
                    .headers
                    .iter()
                    .map(|(name, value)| format!("{name}: {value}"))
                    .collect::<Vec<_>>()
                    .join("\n"),
                ResponseViewMode::Messages => tab
                    .websocket
                    .as_ref()
                    .map(|session| session.inspector.read(cx).value().to_string())
                    .unwrap_or_default(),
                ResponseViewMode::Formatted | ResponseViewMode::Raw => response.body.clone(),
            });
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copy_feedback_active = true;
        self.copy_feedback_revision = self.copy_feedback_revision.wrapping_add(1);
        let feedback_revision = self.copy_feedback_revision;
        cx.notify();
        cx.spawn(async move |this, cx| {
            Timer::after(Duration::from_millis(1_200)).await;
            let _ = this.update(cx, |this, cx| {
                if this.copy_feedback_revision == feedback_revision {
                    this.copy_feedback_active = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn select_section(&mut self, section: EditorSection, cx: &mut Context<Self>) {
        self.tabs[self.active_tab].section = section;
        cx.notify();
    }

    fn add_pair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let variable_names = self.tabs[self.active_tab].variable_names.clone();
        let request_id = self.tabs[self.active_tab].draft.id.clone();
        let pair = PairInputs::empty(&request_id, &variable_names, window, cx);
        let tab = &mut self.tabs[self.active_tab];
        match tab.section {
            EditorSection::Parameters => tab.parameters.push(pair),
            EditorSection::Headers => tab.headers.push(pair),
            EditorSection::Body => {}
        }
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn toggle_pair(&mut self, index: usize, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active_tab];
        let pairs = match tab.section {
            EditorSection::Parameters => &mut tab.parameters,
            EditorSection::Headers => &mut tab.headers,
            EditorSection::Body => return,
        };
        if let Some(pair) = pairs.get_mut(index) {
            pair.enabled = !pair.enabled;
        }
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn remove_pair(&mut self, index: usize, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active_tab];
        let pairs = match tab.section {
            EditorSection::Parameters => &mut tab.parameters,
            EditorSection::Headers => &mut tab.headers,
            EditorSection::Body => return,
        };
        if index < pairs.len() {
            pairs.remove(index);
        }
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn apply_http_stream_event(
        &mut self,
        request_id: &str,
        response_revision: u64,
        event: HttpStreamEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|tab| tab.draft.id == request_id && tab.response_revision == response_revision)
        else {
            return;
        };

        match event {
            HttpStreamEvent::Started(response) => {
                let content_type = response.content_type.clone();
                tab.response = Some(response);
                tab.response_editors = Some(ResponseEditors::streaming(content_type, window, cx));
            }
            HttpStreamEvent::BodyChunk { text, total_bytes } => {
                let Some(response) = tab.response.as_mut() else {
                    return;
                };
                response.size_bytes = total_bytes;
                if let Some(editors) = tab.response_editors.as_mut() {
                    editors.append_stream_chunk(&text);
                    editors.raw.update(cx, |raw, cx| {
                        raw.append_value(SharedString::from(text), window, cx);
                    });
                }
            }
            HttpStreamEvent::Completed {
                elapsed_ms,
                total_bytes,
            } => {
                tab.sending = false;
                if let Some(response) = tab.response.as_mut() {
                    response.elapsed_ms = elapsed_ms;
                    response.size_bytes = total_bytes;
                }
            }
        }
    }

    fn apply_websocket_stream_event(
        &mut self,
        request_id: &str,
        response_revision: u64,
        event: WebSocketStreamEvent,
    ) -> bool {
        let Some(tab) = self
            .tabs
            .iter_mut()
            .find(|tab| tab.draft.id == request_id && tab.response_revision == response_revision)
        else {
            return false;
        };
        let Some(session) = tab.websocket.as_mut() else {
            return false;
        };

        match event {
            WebSocketStreamEvent::Started(response) => {
                session.state = WebSocketConnectionState::Open;
                tab.response = Some(response);
                false
            }
            WebSocketStreamEvent::Message(message) => {
                if let Some(response) = tab.response.as_mut() {
                    response.size_bytes = response.size_bytes.saturating_add(message.size_bytes);
                }
                session.push(message);
                true
            }
            WebSocketStreamEvent::Closed {
                elapsed_ms,
                total_bytes,
                stopped,
                detail,
            } => {
                tab.sending = false;
                tab.cancellation = None;
                session.state = WebSocketConnectionState::Closed;
                session.detail = detail.or_else(|| stopped.then(|| "Stopped by user".into()));
                if let Some(response) = tab.response.as_mut() {
                    response.elapsed_ms = elapsed_ms;
                    response.size_bytes = total_bytes;
                }
                false
            }
        }
    }

    fn stop_websocket(&mut self, _: &ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let tab = &mut self.tabs[self.active_tab];
        let Some(cancellation) = tab.cancellation.as_ref() else {
            return;
        };
        cancellation.store(true, Ordering::Release);
        if let Some(session) = tab.websocket.as_mut() {
            session.state = WebSocketConnectionState::Stopping;
            session.detail = Some("Closing the connection…".into());
        }
        cx.notify();
    }

    fn select_websocket_message(
        &mut self,
        sequence: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self.tabs[self.active_tab].websocket.as_mut() {
            session.select(sequence, window, cx);
            cx.notify();
        }
    }

    fn send_request(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.active_tab;
        if self.tabs[index].sending {
            return;
        }
        let history_request = self.tabs[index].snapshot(cx);
        let environment = self
            .environments
            .environment_for_request(&history_request.id);
        let secret_values = environment
            .into_iter()
            .flat_map(|environment| &environment.variables)
            .filter(|variable| variable.secret)
            .filter_map(|variable| variable.value.clone())
            .collect::<Vec<_>>();
        let request = match resolve_request(&history_request, environment) {
            Ok(request) => request,
            Err(errors) => {
                self.tabs[index].error = Some(errors.join("\n"));
                self.tabs[index].response = None;
                self.tabs[index].response_editors = None;
                cx.notify();
                return;
            }
        };
        let websocket_request = is_websocket_request(&request);
        let cancellation = websocket_request.then(|| Arc::new(AtomicBool::new(false)));
        let request_id = history_request.id.clone();
        self.tabs[index].title = SharedString::from(history_request.display_name());
        self.tabs[index].sending = true;
        self.tabs[index].response = None;
        self.tabs[index].response_editors = None;
        self.tabs[index].websocket = websocket_request.then(|| WebSocketSession::new(window, cx));
        self.tabs[index].cancellation = cancellation.clone();
        if websocket_request {
            self.tabs[index].response_view = ResponseViewMode::Messages;
        }
        self.tabs[index].error = None;
        self.tabs[index].response_revision = self.tabs[index].response_revision.wrapping_add(1);
        self.persistence_dirty.store(true, Ordering::Release);
        let response_revision = self.tabs[index].response_revision;
        let http_session = self.http_session.clone();
        cx.notify();

        // Bound queued chunks so a fast server cannot buffer a second complete
        // response in memory while the UI thread is painting a slower view.
        let (event_tx, event_rx) = smol::channel::bounded(STREAM_EVENT_BUFFER);
        let task = cx.background_executor().spawn(async move {
            if websocket_request {
                WebSocketExecutor::execute_streaming_with_cookie_jar(
                    &request,
                    cancellation.expect("WebSocket cancellation handle should exist"),
                    http_session.cookie_jar(),
                    |event| {
                        let _ = event_tx.send_blocking(RequestStreamEvent::WebSocket(event));
                    },
                )
                .map(RequestExecutionResult::WebSocket)
            } else {
                http_session
                    .execute_streaming(&request, |event| {
                        let _ = event_tx.send_blocking(RequestStreamEvent::Http(event));
                    })
                    .map(|response| {
                        let ResponseSnapshot {
                            status,
                            status_text,
                            elapsed_ms,
                            size_bytes,
                            headers,
                            body,
                            content_type,
                        } = response;
                        let cache = ResponseBodyCache::from_owned(body, content_type.as_deref());
                        let metadata = ResponseSnapshot {
                            status,
                            status_text,
                            elapsed_ms,
                            size_bytes,
                            headers,
                            body: String::new(),
                            content_type,
                        };
                        RequestExecutionResult::Http(cache, metadata)
                    })
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = event_rx.recv().await {
                let mut events = Vec::with_capacity(STREAM_UI_BATCH_SIZE);
                push_stream_event_batch(&mut events, event);
                let mut drained = 1;
                while drained < STREAM_UI_BATCH_SIZE {
                    let Ok(event) = event_rx.try_recv() else {
                        break;
                    };
                    push_stream_event_batch(&mut events, event);
                    drained += 1;
                }
                let _ = cx.update(|window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        let mut websocket_message_received = false;
                        for event in events {
                            match event {
                                RequestStreamEvent::Http(event) => this.apply_http_stream_event(
                                    &request_id,
                                    response_revision,
                                    event,
                                    window,
                                    cx,
                                ),
                                RequestStreamEvent::WebSocket(event) => {
                                    websocket_message_received |= this
                                        .apply_websocket_stream_event(
                                            &request_id,
                                            response_revision,
                                            event,
                                        );
                                }
                            }
                        }
                        if websocket_message_received
                            && let Some(session) = this.tabs.iter_mut().find_map(|tab| {
                                (tab.draft.id == request_id
                                    && tab.response_revision == response_revision)
                                    .then_some(tab.websocket.as_mut())
                                    .flatten()
                            })
                        {
                            session.sync_selected_inspector(window, cx);
                        }
                        cx.notify();
                    });
                });
                // An already-buffered next event must not monopolize the UI
                // executor. Yield so the first highlighted chunk can paint
                // before more network data or final formatting is applied.
                smol::future::yield_now().await;
            }

            let result = task.await;
            let _ = cx.update(|window, cx| {
                let _ = this.update(cx, |this, cx| {
                    match result {
                        Ok(RequestExecutionResult::Http(cache, response)) => {
                            this.history
                                .push(HistoryEntry::success(history_request.clone(), &response));
                            if let Some(tab) = this.tabs.iter_mut().find(|tab| {
                                tab.draft.id == request_id
                                    && tab.response_revision == response_revision
                            }) {
                                tab.sending = false;
                                tab.cancellation = None;
                                if let Some(editors) = tab.response_editors.as_mut() {
                                    editors.finish(cache);
                                } else {
                                    tab.response_editors =
                                        Some(ResponseEditors::new(cache, window, cx));
                                }
                                if let Some(response) = tab.response.as_mut() {
                                    response.body = String::new();
                                }
                            }
                        }
                        Ok(RequestExecutionResult::WebSocket(response)) => {
                            this.history
                                .push(HistoryEntry::success(history_request.clone(), &response));
                            if let Some(tab) = this.tabs.iter_mut().find(|tab| {
                                tab.draft.id == request_id
                                    && tab.response_revision == response_revision
                            }) {
                                tab.sending = false;
                                tab.cancellation = None;
                                if let Some(session) = tab.websocket.as_mut()
                                    && !matches!(session.state, WebSocketConnectionState::Closed)
                                {
                                    session.state = WebSocketConnectionState::Closed;
                                }
                            }
                        }
                        Err(error) => {
                            let message =
                                redact_secret_values(&format!("{error:#}"), &secret_values);
                            this.history.push(HistoryEntry::failure(
                                history_request.clone(),
                                message.clone(),
                            ));
                            if let Some(tab) = this.tabs.iter_mut().find(|tab| {
                                tab.draft.id == request_id
                                    && tab.response_revision == response_revision
                            }) {
                                tab.sending = false;
                                tab.cancellation = None;
                                if let Some(session) = tab.websocket.as_mut() {
                                    session.state = WebSocketConnectionState::Failed;
                                    session.detail = Some(message.clone());
                                    if tab.response.is_none() {
                                        tab.error = Some(message);
                                    }
                                } else {
                                    tab.error = Some(message);
                                }
                            }
                        }
                    }
                    this.persistence_dirty.store(true, Ordering::Release);
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> Div {
        let collapsed = self.sidebar_collapsed;
        let command_hovered = self.command_hovered;
        let command_hover_app = cx.entity();
        let command_hover_start = if command_hovered { px(0.) } else { px(-1.) };
        let command_hover_target = if command_hovered { px(-1.) } else { px(0.) };
        let sidebar_target = if collapsed { px(0.) } else { px(224.) };
        let sidebar_start = if collapsed { px(224.) } else { px(0.) };
        let brand = div()
            .h_full()
            .w(sidebar_target)
            .flex_shrink_0()
            .overflow_hidden()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .w(px(224.))
                    .h_full()
                    .px(px(15.))
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .size(px(27.))
                            .flex_shrink_0()
                            .rounded(px(8.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .child("A"),
                    )
                    .child(
                        Label::new("alula")
                            .ml(px(10.))
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD),
                    )
                    .child(
                        div()
                            .ml_auto()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(10.))
                            .text_color(cx.theme().muted_foreground.opacity(0.68))
                            .child(env!("CARGO_PKG_VERSION")),
                    ),
            )
            .with_animation(
                SharedString::from(format!("brand-collapse-{collapsed}")),
                Animation::new(Duration::from_secs_f64(0.18))
                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                move |this, delta| {
                    let width = sidebar_start + (sidebar_target - sidebar_start) * delta;
                    this.w(width)
                        .opacity(if collapsed { 1.0 - delta } else { delta })
                },
            );

        let mcp_status = match &self.mcp_status {
            McpStatus::Ready { port } => div()
                .h(px(27.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_full()
                .border_1()
                .border_color(cx.theme().success.opacity(0.24))
                .bg(cx.theme().success.opacity(0.08))
                .text_color(cx.theme().success)
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(px(10.))
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(cx.theme().success)
                        .opacity(0.88),
                )
                .child(format!("MCP :{port}")),
            McpStatus::Error(error) => div()
                .h(px(27.))
                .px_2()
                .flex()
                .items_center()
                .rounded_full()
                .border_1()
                .border_color(cx.theme().danger.opacity(0.3))
                .bg(cx.theme().danger.opacity(0.08))
                .text_color(cx.theme().danger)
                .text_size(px(10.))
                .child(error.clone()),
        };

        div()
            .h(px(52.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(brand)
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .h_full()
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Button::new("sidebar-toggle")
                            .ghost()
                            .small()
                            .compact()
                            .label("☰")
                            .tooltip("Toggle sidebar")
                            .on_click(cx.listener(Self::toggle_sidebar)),
                    )
                    .child(
                        Label::new("Workspace")
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        Label::new("/")
                            .text_xs()
                            .text_color(cx.theme().muted_foreground.opacity(0.55)),
                    )
                    .child(
                        Label::new(match self.workspace_section {
                            WorkspaceSection::Requests => "Requests",
                            WorkspaceSection::Environments => "Environments",
                            WorkspaceSection::History => "History",
                        })
                        .text_xs()
                        .font_weight(FontWeight::MEDIUM),
                    ),
            )
            .child(
                div()
                    .pr_3()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .id("quick-actions")
                            .relative()
                            .h(px(30.))
                            .px(px(9.))
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .rounded(px(6.))
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().sidebar)
                            .text_size(px(11.))
                            .text_color(cx.theme().muted_foreground)
                            .cursor_pointer()
                            .hover(|this| {
                                this.border_color(cx.theme().muted_foreground.opacity(0.42))
                                    .bg(cx.theme().muted)
                            })
                            .child("Search or run a command")
                            .child(
                                div()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(10.))
                                    .text_color(cx.theme().muted_foreground.opacity(0.62))
                                    .child("⌘ K"),
                            )
                            .on_click(cx.listener(Self::open_command_menu))
                            .on_hover(move |hovered, _, cx| {
                                command_hover_app.update(cx, |this, cx| {
                                    if this.command_hovered != *hovered {
                                        this.command_hovered = *hovered;
                                        cx.notify();
                                    }
                                });
                            })
                            .with_animation(
                                SharedString::from(format!("command-hover-{command_hovered}")),
                                Animation::new(Duration::from_secs_f64(0.12))
                                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                move |this, delta| {
                                    this.top(
                                        command_hover_start
                                            + (command_hover_target - command_hover_start) * delta,
                                    )
                                },
                            ),
                    )
                    .child(mcp_status)
                    .child(
                        Button::new("settings")
                            .ghost()
                            .small()
                            .compact()
                            .icon(IconName::Settings)
                            .tooltip("Settings")
                            .on_click(cx.listener(Self::open_settings)),
                    ),
            )
    }

    fn render_sidebar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = self.sidebar_collapsed;
        let target = if collapsed { px(0.) } else { px(224.) };
        let start = if collapsed { px(224.) } else { px(0.) };
        let app = cx.entity();
        let requests_app = app.clone();
        let environments_app = app.clone();
        let history_app = app.clone();
        let new_request_hovered = self.new_request_hovered;
        let new_request_focused = self.new_request_focus.is_focused(window);
        let new_request_focus = self.new_request_focus.clone().tab_stop(true);
        let new_request_hover_app = app.clone();
        let new_request_key_app = app.clone();
        let new_request_hover_start = if new_request_hovered { px(0.) } else { px(-1.) };
        let new_request_hover_target = if new_request_hovered { px(-1.) } else { px(0.) };

        let nav_item =
            |section: WorkspaceSection,
             label: &'static str,
             icon: IconName,
             active: bool,
             count: usize,
             on_click: Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>| {
                let hovered = self.sidebar_hovered == Some(section);
                let hover_app = app.clone();
                let hover_start = if hovered { px(0.) } else { px(1.) };
                let hover_target = if hovered { px(1.) } else { px(0.) };
                div()
                    .id(SharedString::from(format!("sidebar-{label}")))
                    .relative()
                    .w_full()
                    .h(px(31.))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .text_size(px(12.))
                    .text_color(if active {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground
                    })
                    .when(active, |this| {
                        this.bg(cx.theme().accent).child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .w(px(2.))
                                .h(px(14.))
                                .rounded_full()
                                .bg(cx.theme().primary),
                        )
                    })
                    .when(!active, |this| {
                        this.hover(|this| {
                            this.bg(cx.theme().muted).text_color(cx.theme().foreground)
                        })
                    })
                    .child(
                        div()
                            .w(px(18.))
                            .mr_3()
                            .flex_shrink_0()
                            .flex()
                            .justify_center()
                            .child(Icon::new(icon).size_3p5().text_color(if active {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground.opacity(0.72)
                            })),
                    )
                    .child(label)
                    .child(
                        div()
                            .ml_auto()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(10.))
                            .text_color(cx.theme().muted_foreground.opacity(0.64))
                            .child(count.to_string()),
                    )
                    .on_click(move |event, window, cx| on_click(event, window, cx))
                    .on_hover(move |is_hovered, _, cx| {
                        hover_app.update(cx, |this, cx| {
                            let next = if *is_hovered {
                                Some(section)
                            } else if this.sidebar_hovered == Some(section) {
                                None
                            } else {
                                this.sidebar_hovered
                            };
                            if this.sidebar_hovered != next {
                                this.sidebar_hovered = next;
                                cx.notify();
                            }
                        });
                    })
                    .with_animation(
                        SharedString::from(format!("sidebar-hover-{label}-{hovered}")),
                        Animation::new(Duration::from_secs_f64(0.12))
                            .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                        move |this, delta| {
                            this.left(hover_start + (hover_target - hover_start) * delta)
                        },
                    )
            };

        let mut open_requests = div().mt_1().flex().flex_col().gap(px(2.));
        for (index, tab) in self.tabs.iter().enumerate() {
            let tab_app = app.clone();
            let method = tab.draft.method;
            let label = tab.title.clone();
            open_requests = open_requests.child(
                div()
                    .id(SharedString::from(format!(
                        "sidebar-request-{}",
                        tab.draft.id
                    )))
                    .w_full()
                    .h(px(29.))
                    .px_2()
                    .pl(px(22.))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(cx.theme().radius)
                    .hover(|this| this.bg(cx.theme().muted))
                    .child(
                        div()
                            .w(px(27.))
                            .flex_shrink_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(8.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(method_color(method, cx))
                            .child(if method == HttpMethod::Delete {
                                "DEL"
                            } else {
                                method.as_str()
                            }),
                    )
                    .child(
                        Label::new(label)
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .on_click(move |_, _, cx| {
                        tab_app.update(cx, |this, cx| {
                            this.active_tab = index.min(this.tabs.len().saturating_sub(1));
                            this.show_selected_request(cx);
                        });
                    }),
            );
        }

        let sidebar = div()
            .w(target)
            .h_full()
            .min_h_0()
            .flex_shrink_0()
            .overflow_hidden()
            .border_r_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(
                div()
                    .w(px(224.))
                    .h_full()
                    .p_2()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .id("new-request-sidebar")
                            .relative()
                            .track_focus(&new_request_focus)
                            .h(px(34.))
                            .w_full()
                            .px(px(10.))
                            .flex()
                            .items_center()
                            .rounded(px(9.))
                            .border_1()
                            .border_color(if new_request_hovered {
                                cx.theme().primary.opacity(0.5)
                            } else {
                                cx.theme().border
                            })
                            .bg(if new_request_hovered {
                                cx.theme().secondary.lighten(0.1)
                            } else {
                                cx.theme().muted
                            })
                            .when(new_request_focused, |this| {
                                this.child(
                                    div()
                                        .absolute()
                                        .top(px(-4.))
                                        .left(px(-4.))
                                        .right(px(-4.))
                                        .bottom(px(-4.))
                                        .rounded(px(13.))
                                        .border(px(3.))
                                        .border_color(cx.theme().primary.opacity(0.12)),
                                )
                            })
                            .cursor_pointer()
                            .child(
                                div()
                                    .w(px(18.))
                                    .mr_2()
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(
                                        Icon::new(IconName::Plus)
                                            .size(px(15.))
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(cx.theme().muted_foreground)
                                    .child("New request"),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_size(px(9.))
                                    .text_color(cx.theme().muted_foreground.opacity(0.68))
                                    .child("⌘ N"),
                            )
                            .on_click(cx.listener(Self::add_request))
                            .on_key_down(move |event, window, cx| {
                                if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                                    new_request_key_app.update(cx, |this, cx| {
                                        this.add_request(&ClickEvent::default(), window, cx)
                                    });
                                    window.prevent_default();
                                    cx.stop_propagation();
                                }
                            })
                            .on_hover(move |hovered, _, cx| {
                                new_request_hover_app.update(cx, |this, cx| {
                                    if this.new_request_hovered != *hovered {
                                        this.new_request_hovered = *hovered;
                                        cx.notify();
                                    }
                                });
                            })
                            .with_animation(
                                SharedString::from(format!(
                                    "new-request-hover-{new_request_hovered}"
                                )),
                                Animation::new(Duration::from_secs_f64(0.12))
                                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                move |this, delta| {
                                    this.top(
                                        new_request_hover_start
                                            + (new_request_hover_target - new_request_hover_start)
                                                * delta,
                                    )
                                },
                            ),
                    )
                    .child(
                        div()
                            .mt_4()
                            .px_2()
                            .pb_2()
                            .text_size(px(9.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(cx.theme().muted_foreground.opacity(0.64))
                            .child("WORKSPACE"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(2.))
                            .child(nav_item(
                                WorkspaceSection::Requests,
                                "Requests",
                                IconName::SquareTerminal,
                                self.workspace_section == WorkspaceSection::Requests,
                                self.tabs.len(),
                                Box::new(move |_, _, cx| {
                                    requests_app.update(cx, |this, cx| {
                                        this.select_workspace_section(
                                            WorkspaceSection::Requests,
                                            cx,
                                        )
                                    });
                                }),
                            ))
                            .child(nav_item(
                                WorkspaceSection::Environments,
                                "Environments",
                                IconName::Globe,
                                self.workspace_section == WorkspaceSection::Environments,
                                self.environments.environments.len(),
                                Box::new(move |_, _, cx| {
                                    environments_app.update(cx, |this, cx| {
                                        this.select_workspace_section(
                                            WorkspaceSection::Environments,
                                            cx,
                                        )
                                    });
                                }),
                            ))
                            .child(nav_item(
                                WorkspaceSection::History,
                                "History",
                                IconName::Redo2,
                                self.workspace_section == WorkspaceSection::History,
                                self.history.entries.len(),
                                Box::new(move |_, _, cx| {
                                    history_app.update(cx, |this, cx| {
                                        this.select_workspace_section(WorkspaceSection::History, cx)
                                    });
                                }),
                            )),
                    )
                    .child(
                        div()
                            .mt_4()
                            .flex_1()
                            .min_h_0()
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .px_2()
                                    .pb_1()
                                    .flex_shrink_0()
                                    .text_size(px(9.))
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(cx.theme().muted_foreground.opacity(0.64))
                                    .child("OPEN REQUESTS"),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_h_0()
                                    .overflow_y_scrollbar()
                                    .child(open_requests),
                            ),
                    )
                    .child(
                        div()
                            .mt_3()
                            .flex_shrink_0()
                            .p_3()
                            .rounded(cx.theme().radius)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background.opacity(0.35))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(11.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().foreground)
                                    .child(div().size(px(6.)).rounded_full().bg(cx.theme().success))
                                    .child("Agent access ready"),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .text_size(px(10.))
                                    .line_height(relative(1.45))
                                    .text_color(cx.theme().muted_foreground.opacity(0.72))
                                    .child(
                                        "Inspect, edit, and send requests through typed MCP tools.",
                                    ),
                            )
                            .child(
                                div()
                                    .mt_2()
                                    .text_size(px(10.))
                                    .text_color(cx.theme().primary)
                                    .child(format!("{} tool contracts →", alula::MCP_TOOL_COUNT)),
                            ),
                    ),
            );

        sidebar.with_animation(
            SharedString::from(format!("sidebar-collapse-{collapsed}")),
            Animation::new(Duration::from_secs_f64(0.18))
                .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
            move |this, delta| {
                let width = start + (target - start) * delta;
                this.w(width)
                    .opacity(if collapsed { 1.0 - delta } else { delta })
            },
        )
    }

    fn render_request_tabs(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let environments = Arc::new(
            self.environments
                .environments
                .iter()
                .map(|environment| (environment.id.clone(), environment.name.clone()))
                .collect::<Vec<_>>(),
        );
        let mut bar = div()
            .id("request-tabs")
            .h_full()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .overflow_x_scroll()
            .track_scroll(&self.request_tabs_scroll)
            .bg(cx.theme().sidebar);
        for (index, tab) in self.tabs.iter().enumerate() {
            let method = tab.draft.method.as_str();
            let label = tab.title.clone();
            let selected = index == self.active_tab;
            let request_id = tab.draft.id.clone();
            let assigned_environment = self
                .environments
                .environment_for_request(&request_id)
                .map(|environment| environment.id.clone());
            let environments = environments.clone();
            let menu_app = app.clone();
            let close_app = app.clone();
            let select_app = app.clone();
            let tab_scroll = self.request_tabs_scroll.clone();
            let tab_content = div()
                .id(("request-tab", index))
                .relative()
                .w(px(190.))
                .h_full()
                .flex_shrink_0()
                .min_w_0()
                .px_3()
                .flex()
                .items_center()
                .gap_2()
                .border_r_1()
                .border_color(cx.theme().border)
                .bg(if selected {
                    cx.theme().background
                } else {
                    cx.theme().sidebar
                })
                .text_color(if selected {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .cursor_pointer()
                .when(!selected, |this| {
                    this.hover(|this| this.bg(cx.theme().muted))
                })
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(method_color(tab.draft.method, cx))
                        .child(method),
                )
                .child(
                    Label::new(label)
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .line_height(rems(1.25)),
                )
                .child(
                    Button::new(("close-request", index))
                        .ghost()
                        .xsmall()
                        .compact()
                        .flex_shrink_0()
                        .icon(IconName::Close)
                        .tooltip("Close request")
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            close_app.update(cx, |this, cx| this.close_request(index, cx));
                        }),
                )
                .when(selected, |this| {
                    this.child(
                        div()
                            .absolute()
                            .left(px(0.))
                            .right(px(0.))
                            .bottom(px(0.))
                            .h(px(2.))
                            .bg(cx.theme().primary)
                            .with_animation(
                                SharedString::from(format!("request-tab-indicator-{index}")),
                                Animation::new(Duration::from_secs_f64(0.18))
                                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                |this, delta| this.opacity(delta),
                            ),
                    )
                })
                .on_click(move |_, _, cx| {
                    tab_scroll.scroll_to_item(index);
                    select_app.update(cx, |this, cx| {
                        if index < this.tabs.len() {
                            this.active_tab = index;
                            this.persistence_dirty.store(true, Ordering::Release);
                            cx.notify();
                        }
                    });
                })
                .context_menu(move |mut menu, _, _| {
                    menu = menu.label("Add to environment");
                    if environments.is_empty() {
                        menu = menu.item(PopupMenuItem::new("No environments yet").disabled(true));
                    } else {
                        for (environment_id, name) in environments.iter() {
                            let app = menu_app.clone();
                            let request_id = request_id.clone();
                            let environment_id = environment_id.clone();
                            menu = menu.item(
                                PopupMenuItem::new(name.clone())
                                    .checked(assigned_environment.as_ref() == Some(&environment_id))
                                    .on_click(move |_, _, cx| {
                                        app.update(cx, |this, cx| {
                                            this.assign_request_to_environment(
                                                &request_id,
                                                &environment_id,
                                                cx,
                                            )
                                        });
                                    }),
                            );
                        }
                    }
                    if assigned_environment.is_some() {
                        let app = menu_app.clone();
                        let request_id = request_id.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::new("Remove from environment").on_click(
                                move |_, _, cx| {
                                    app.update(cx, |this, cx| {
                                        this.remove_request_from_environment(&request_id, cx)
                                    });
                                },
                            ),
                        );
                    }
                    menu
                });
            bar = bar.child(tab_content);
        }
        div()
            .h(px(42.))
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .child(bar)
            .child(
                div().px_1().flex_shrink_0().child(
                    Button::new("add-request-tab")
                        .ghost()
                        .small()
                        .compact()
                        .icon(IconName::Plus)
                        .tooltip("New request")
                        .on_click(cx.listener(Self::add_request)),
                ),
            )
    }

    fn render_requests_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(self.render_request_tabs(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .child(self.render_request_builder(window, cx))
                    .child(self.render_response(window, cx)),
            )
    }

    /// Retain only response TextView element state off-page. Rebuilding the
    /// complete request composer behind History or Environments made every
    /// interaction there pay for hidden inputs, tabs, menus, and validation.
    fn render_request_keepalive(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let (active_view, keepalive_views) = self.build_formatted_response_views(window, cx);
        div()
            .hidden()
            .children(active_view)
            .children(keepalive_views)
    }

    fn open_environment_details(
        &mut self,
        environment_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_environment_id = Some(environment_id);
        self.environment_detail_tab = EnvironmentDetailTab::Requests;
        self.environment_request_search.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        self.environment_variable_search.update(cx, |input, cx| {
            input.set_value("", window, cx);
        });
        cx.notify();
    }

    fn close_environment_details(&mut self, cx: &mut Context<Self>) {
        self.selected_environment_id = None;
        cx.notify();
    }

    fn render_environment_variable_rows(
        &self,
        environment: &alula::Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let app = cx.entity();
        let query = self
            .environment_variable_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let variables = environment
            .variables
            .iter()
            .filter(|variable| {
                query.is_empty()
                    || variable.name.to_ascii_lowercase().contains(&query)
                    || (!variable.secret
                        && variable
                            .value
                            .as_deref()
                            .unwrap_or_default()
                            .to_ascii_lowercase()
                            .contains(&query))
            })
            .collect::<Vec<_>>();
        if variables.is_empty() {
            return empty_state(
                if query.is_empty() {
                    "No variables yet"
                } else {
                    "No matching variables"
                },
                if query.is_empty() {
                    "Add a variable for use in request templates"
                } else {
                    "Try a different search"
                },
                cx,
            );
        }

        let mut rows = div().flex().flex_col().gap_1();
        for variable in variables {
            let edit_app = app.clone();
            let edit_environment_id = environment.id.clone();
            let edit_variable_id = variable.id.clone();
            let remove_app = app.clone();
            let remove_environment_id = environment.id.clone();
            let remove_variable_id = variable.id.clone();
            let display_value = if variable.secret {
                "••••••••".to_owned()
            } else {
                variable.value.clone().unwrap_or_default()
            };
            rows = rows.child(
                div()
                    .w_full()
                    .h(px(42.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .child(
                        div()
                            .w(px(180.))
                            .flex_shrink_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_xs()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().primary)
                            .child(format!("{{{{{}}}}}", variable.name)),
                    )
                    .child(
                        Tag::secondary()
                            .small()
                            .rounded_full()
                            .child(if variable.secret { "Secret" } else { "Public" }),
                    )
                    .child(
                        Label::new(display_value)
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "edit-environment-variable-{}",
                            variable.id
                        )))
                        .ghost()
                        .small()
                        .label("Edit")
                        .on_click(move |_, window, cx| {
                            edit_app.update(cx, |this, cx| {
                                this.open_environment_variable_dialog(
                                    edit_environment_id.clone(),
                                    Some(edit_variable_id.clone()),
                                    window,
                                    cx,
                                )
                            });
                        }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "remove-environment-variable-{}",
                            variable.id
                        )))
                        .ghost()
                        .small()
                        .compact()
                        .icon(IconName::Close)
                        .tooltip("Remove variable")
                        .on_click(move |_, window, cx| {
                            remove_app.update(cx, |this, cx| {
                                this.remove_environment_variable(
                                    &remove_environment_id,
                                    &remove_variable_id,
                                    window,
                                    cx,
                                )
                            });
                        }),
                    ),
            );
        }
        rows
    }

    fn render_environment_request_rows(
        &self,
        environment: &alula::Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let app = cx.entity();
        let query = self
            .environment_request_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let requests = environment
            .requests
            .iter()
            .filter(|request| {
                query.is_empty()
                    || request.display_name().to_ascii_lowercase().contains(&query)
                    || request.url.to_ascii_lowercase().contains(&query)
                    || request
                        .method
                        .as_str()
                        .to_ascii_lowercase()
                        .contains(&query)
            })
            .collect::<Vec<_>>();
        if requests.is_empty() {
            return empty_state(
                if query.is_empty() {
                    "No requests yet"
                } else {
                    "No matching requests"
                },
                if query.is_empty() {
                    "Right-click a request tab to add it here"
                } else {
                    "Try a different search"
                },
                cx,
            );
        }

        let mut rows = div().flex().flex_col().gap_1();
        for request in requests {
            let open_app = app.clone();
            let open_environment_id = environment.id.clone();
            let open_request_id = request.id.clone();
            rows = rows.child(
                div()
                    .w_full()
                    .h(px(52.))
                    .px_3()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .hover(|this| {
                        this.border_color(cx.theme().muted_foreground.opacity(0.32))
                            .bg(cx.theme().muted)
                    })
                    .child(method_badge(request.method, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                Label::new(request.display_name())
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD),
                            )
                            .child(
                                Label::new(request.url.clone())
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "open-environment-request-{}",
                            request.id
                        )))
                        .outline()
                        .small()
                        .label("Open")
                        .on_click(move |_, window, cx| {
                            open_app.update(cx, |this, cx| {
                                this.open_environment_request(
                                    &open_environment_id,
                                    &open_request_id,
                                    window,
                                    cx,
                                )
                            });
                        }),
                    ),
            );
        }
        rows
    }

    fn render_environment_detail(
        &self,
        environment: &alula::Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let app = cx.entity();
        let back_app = app.clone();
        let delete_app = app.clone();
        let delete_id = environment.id.clone();
        let delete_name = environment.name.clone();
        let requests_selected = self.environment_detail_tab == EnvironmentDetailTab::Requests;
        let variables_selected = self.environment_detail_tab == EnvironmentDetailTab::Variables;
        let request_tab_app = app.clone();
        let variable_tab_app = app.clone();
        let (search, rows, add_variable) = if requests_selected {
            (
                Input::new(&self.environment_request_search)
                    .prefix(IconName::Search)
                    .w_full(),
                self.render_environment_request_rows(environment, cx),
                None,
            )
        } else {
            let add_app = app.clone();
            let add_environment_id = environment.id.clone();
            (
                Input::new(&self.environment_variable_search)
                    .prefix(IconName::Search)
                    .w_full(),
                self.render_environment_variable_rows(environment, cx),
                Some(
                    Button::new("environment-detail-add-variable")
                        .primary()
                        .small()
                        .icon(IconName::Plus)
                        .label("Add variable")
                        .on_click(move |_, window, cx| {
                            add_app.update(cx, |this, cx| {
                                this.open_environment_variable_dialog(
                                    add_environment_id.clone(),
                                    None,
                                    window,
                                    cx,
                                )
                            });
                        }),
                ),
            )
        };

        div().size_full().min_h_0().p_4().child(
            div()
                .size_full()
                .min_h_0()
                .overflow_hidden()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().sidebar)
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(64.))
                        .px_4()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("back-to-environments")
                                .ghost()
                                .small()
                                .icon(IconName::ArrowLeft)
                                .label("Back")
                                .on_click(move |_, _, cx| {
                                    back_app
                                        .update(cx, |this, cx| this.close_environment_details(cx));
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    Label::new(environment.name.clone())
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_lg(),
                                )
                                .child(
                                    Label::new(format!(
                                        "{} requests · {} variables",
                                        environment.requests.len(),
                                        environment.variables.len()
                                    ))
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground),
                                ),
                        )
                        .child(
                            Button::new("delete-environment-detail")
                                .ml_auto()
                                .danger()
                                .small()
                                .icon(IconName::Delete)
                                .label("Delete")
                                .on_click(move |_, window, cx| {
                                    delete_app.update(cx, |this, cx| {
                                        this.confirm_delete_environment(
                                            delete_id.clone(),
                                            delete_name.clone(),
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        ),
                )
                .child(
                    div()
                        .h(px(46.))
                        .px_4()
                        .flex_shrink_0()
                        .flex()
                        .items_end()
                        .gap_1()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("environment-requests-tab")
                                .ghost()
                                .h(px(36.))
                                .rounded_b_none()
                                .label(format!("Requests ({})", environment.requests.len()))
                                .when(requests_selected, |this| {
                                    this.bg(cx.theme().accent).text_color(cx.theme().foreground)
                                })
                                .on_click(move |_, _, cx| {
                                    request_tab_app.update(cx, |this, cx| {
                                        this.environment_detail_tab =
                                            EnvironmentDetailTab::Requests;
                                        cx.notify();
                                    });
                                }),
                        )
                        .child(
                            Button::new("environment-variables-tab")
                                .ghost()
                                .h(px(36.))
                                .rounded_b_none()
                                .label(format!("Variables ({})", environment.variables.len()))
                                .when(variables_selected, |this| {
                                    this.bg(cx.theme().accent).text_color(cx.theme().foreground)
                                })
                                .on_click(move |_, _, cx| {
                                    variable_tab_app.update(cx, |this, cx| {
                                        this.environment_detail_tab =
                                            EnvironmentDetailTab::Variables;
                                        cx.notify();
                                    });
                                }),
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_3()
                        .bg(cx.theme().background)
                        .child(
                            div()
                                .w_full()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().w(px(360.)).child(search))
                                .children(add_variable),
                        )
                        .child(div().flex_1().min_h_0().overflow_y_scrollbar().child(rows)),
                ),
        )
    }

    fn render_environment_index(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let query = self
            .environment_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let environments = self
            .environments
            .environments
            .iter()
            .filter(|environment| {
                query.is_empty() || environment.name.to_ascii_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
        let mut content = div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .bg(cx.theme().background);
        if environments.is_empty() {
            content = content.child(empty_state(
                if self.environments.environments.is_empty() {
                    "No environments yet"
                } else {
                    "No matching environments"
                },
                if self.environments.environments.is_empty() {
                    "Create one, then right-click a request tab to add it"
                } else {
                    "Try a different search"
                },
                cx,
            ));
        } else {
            for environment in environments {
                let open_app = app.clone();
                let open_id = environment.id.clone();
                let view_app = app.clone();
                let view_id = environment.id.clone();
                let delete_app = app.clone();
                let delete_id = environment.id.clone();
                let delete_name = environment.name.clone();
                content = content.child(
                    div()
                        .id(SharedString::from(format!(
                            "environment-card-{}",
                            environment.id
                        )))
                        .w_full()
                        .h(px(68.))
                        .px_4()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_3()
                        .cursor_pointer()
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(cx.theme().border)
                        .bg(cx.theme().sidebar)
                        .hover(|this| {
                            this.border_color(cx.theme().primary.opacity(0.36))
                                .bg(cx.theme().muted)
                        })
                        .child(div().size(px(8.)).rounded_full().bg(cx.theme().primary))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    Label::new(environment.name.clone())
                                        .truncate()
                                        .font_weight(FontWeight::SEMIBOLD),
                                )
                                .child(
                                    Label::new(format!(
                                        "{} variables · {} requests",
                                        environment.variables.len(),
                                        environment.requests.len()
                                    ))
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground),
                                ),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "delete-environment-{}",
                                environment.id
                            )))
                            .ghost()
                            .small()
                            .icon(IconName::Delete)
                            .label("Delete")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                delete_app.update(cx, |this, cx| {
                                    this.confirm_delete_environment(
                                        delete_id.clone(),
                                        delete_name.clone(),
                                        window,
                                        cx,
                                    )
                                });
                            }),
                        )
                        .child(
                            Button::new(SharedString::from(format!(
                                "view-environment-{}",
                                environment.id
                            )))
                            .outline()
                            .small()
                            .icon(IconName::ChevronRight)
                            .label("View")
                            .on_click(move |_, window, cx| {
                                cx.stop_propagation();
                                view_app.update(cx, |this, cx| {
                                    this.open_environment_details(view_id.clone(), window, cx)
                                });
                            }),
                        )
                        .on_click(move |_, window, cx| {
                            open_app.update(cx, |this, cx| {
                                this.open_environment_details(open_id.clone(), window, cx)
                            });
                        }),
                );
            }
        }

        div().size_full().min_h_0().p_4().child(
            div()
                .size_full()
                .min_h_0()
                .overflow_hidden()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().sidebar)
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(68.))
                        .px_4()
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    Label::new("Environments")
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_lg(),
                                )
                                .child(
                                    Label::new("Reusable request groups and variables")
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        )
                        .child(
                            div().ml_auto().w(px(280.)).child(
                                Input::new(&self.environment_search)
                                    .prefix(IconName::Search)
                                    .w_full(),
                            ),
                        )
                        .child(
                            Tag::secondary()
                                .small()
                                .rounded_full()
                                .child(format!("{} total", self.environments.environments.len())),
                        )
                        .child(
                            Button::new("new-environment")
                                .primary()
                                .small()
                                .icon(IconName::Plus)
                                .label("New environment")
                                .on_click(cx.listener(Self::open_environment_dialog)),
                        ),
                )
                .child(content),
        )
    }

    fn render_environments(&self, cx: &mut Context<Self>) -> Div {
        self.selected_environment_id
            .as_deref()
            .and_then(|environment_id| {
                self.environments
                    .environments
                    .iter()
                    .find(|environment| environment.id == environment_id)
            })
            .map(|environment| self.render_environment_detail(environment, cx))
            .unwrap_or_else(|| self.render_environment_index(cx))
    }

    fn history_entry_row(&self, index: usize, app: Entity<Self>, cx: &mut App) -> Option<Div> {
        let entry = self.history.entries.get(index)?;
        let open_history_id = entry.id.clone();
        let delete_history_app = app.clone();
        let delete_history_id = entry.id.clone();
        let delete_history_name = entry.request.display_name();
        let (outcome, outcome_color) = if let Some(status) = entry.status {
            let color = if status < 300 {
                cx.theme().success
            } else if status < 400 {
                cx.theme().info
            } else if status < 500 {
                cx.theme().warning
            } else {
                cx.theme().danger
            };
            (
                format!(
                    "{} · {} ms · {}",
                    status,
                    entry.elapsed_ms.unwrap_or_default(),
                    format_size(entry.size_bytes.unwrap_or_default())
                ),
                color,
            )
        } else {
            (
                entry
                    .error
                    .clone()
                    .unwrap_or_else(|| "Request failed".into()),
                cx.theme().danger,
            )
        };
        Some(
            div().h(px(72.)).pb_2().child(
                div()
                    .w_full()
                    .h_full()
                    .px_4()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .hover(|this| {
                        this.border_color(cx.theme().muted_foreground.opacity(0.32))
                            .bg(cx.theme().muted)
                    })
                    .child(method_badge(entry.request.method, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                Label::new(entry.request.display_name())
                                    .truncate()
                                    .font_weight(FontWeight::SEMIBOLD),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .px_2()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(outcome_color.opacity(0.1))
                                            .text_size(px(10.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(outcome_color)
                                            .child(outcome),
                                    )
                                    .child(
                                        Label::new(relative_history_time(entry.sent_at_unix_ms))
                                            .truncate()
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "open-history-request-{}",
                            entry.id
                        )))
                        .outline()
                        .small()
                        .label("Open as tab")
                        .on_click(move |_, window, cx| {
                            app.update(cx, |this, cx| {
                                this.open_history_request(&open_history_id, window, cx)
                            });
                        }),
                    )
                    .child(
                        Button::new(SharedString::from(format!(
                            "delete-history-entry-{}",
                            entry.id
                        )))
                        .danger()
                        .small()
                        .icon(IconName::Delete)
                        .label("Delete")
                        .on_click(move |_, window, cx| {
                            delete_history_app.update(cx, |this, cx| {
                                this.confirm_delete_history_entry(
                                    delete_history_id.clone(),
                                    delete_history_name.clone(),
                                    window,
                                    cx,
                                )
                            });
                        }),
                    ),
            ),
        )
    }

    fn render_history(&self, cx: &mut Context<Self>) -> Div {
        let content = if self.history.entries.is_empty() {
            div().flex_1().min_h_0().child(empty_state(
                "No request history yet",
                "Each completed or failed send is recorded independently of tabs",
                cx,
            ))
        } else {
            let app = cx.entity();
            div()
                .flex_1()
                .min_h_0()
                .p_3()
                .bg(cx.theme().background)
                .child(
                    uniform_list(
                        "history-entries",
                        self.history.entries.len(),
                        cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                            range
                                .filter_map(|index| this.history_entry_row(index, app.clone(), cx))
                                .collect::<Vec<_>>()
                        }),
                    )
                    .size_full(),
                )
        };
        div()
            .size_full()
            .min_h_0()
            .p_4()
            .child(
                div()
                    .size_full()
                    .min_h_0()
                    .overflow_hidden()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().sidebar)
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .h(px(58.))
                            .px_4()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        Label::new("History")
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_lg(),
                                    )
                                    .child(
                                        Label::new("Persistent request executions; response bodies are not retained")
                                            .text_xs()
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                            )
                            .child(
                                Tag::secondary()
                                    .small()
                                    .rounded_full()
                                    .child(format!("{} entries", self.history.entries.len())),
                            ),
                    )
                    .child(content),
            )
    }

    fn render_request_builder(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let tab = &self.tabs[self.active_tab];
        let method = tab.method.clone();
        let url = tab.url.clone();
        let url_state = tab.url_template_state.clone();
        let has_url_error = matches!(url_state, TemplateVisualState::Error(_));
        let mut url_input = Input::new(&url)
            .large()
            .appearance(false)
            .focus_bordered(false)
            .w_full();
        url_input = match &url_state {
            TemplateVisualState::Plain => url_input,
            TemplateVisualState::Valid => url_input.text_color(cx.theme().primary),
            TemplateVisualState::Error(message) => url_input.text_color(cx.theme().danger).suffix(
                Button::new("url-variable-error")
                    .ghost()
                    .small()
                    .compact()
                    .icon(IconName::TriangleAlert)
                    .tooltip(message.clone()),
            ),
        };
        let composer_focused = method
            .read(cx)
            .focus_handle(cx)
            .contains_focused(window, cx)
            || url.read(cx).focus_handle(cx).contains_focused(window, cx);
        let method_value = tab.draft.method;
        let sending = tab.sending;
        let websocket_request = tab.websocket_hint(cx);
        let can_stop = sending && tab.websocket.is_some();
        let send_label = if can_stop {
            "Stop"
        } else if sending {
            "Sending"
        } else if websocket_request && tab.websocket.is_some() {
            "Reconnect"
        } else if websocket_request {
            "Connect"
        } else {
            "Send"
        };
        let send_hovered = self.send_hovered && (!sending || can_stop);
        let send_hover_app = cx.entity();
        let send_hover_start = if send_hovered { px(0.) } else { px(-1.) };
        let send_hover_target = if send_hovered { px(-1.) } else { px(0.) };
        let arrow_hover_start = if send_hovered { px(0.) } else { px(3.) };
        let arrow_hover_target = if send_hovered { px(3.) } else { px(0.) };
        let send_animation_id = tab.draft.id.clone();
        let send_arrow = if can_stop {
            div()
                .relative()
                .flex()
                .items_center()
                .child(Icon::new(IconName::Close).size_3p5())
                .into_any_element()
        } else if sending {
            div()
                .relative()
                .flex()
                .items_center()
                .child(Icon::new(IconName::ArrowRight).size_3p5())
                .with_animation(
                    SharedString::from(format!("send-arrow-{send_animation_id}")),
                    Animation::new(Duration::from_secs_f64(0.62))
                        .repeat()
                        .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                    |this, delta| {
                        let opacity = (1.0 - (delta * 2.0 - 1.0).abs()).max(0.25);
                        this.left(px(-5.) + px(13.) * delta).opacity(opacity)
                    },
                )
                .into_any_element()
        } else {
            div()
                .relative()
                .flex()
                .items_center()
                .child(Icon::new(IconName::ArrowRight).size_3p5())
                .with_animation(
                    SharedString::from(format!(
                        "send-hover-arrow-{send_animation_id}-{send_hovered}"
                    )),
                    Animation::new(Duration::from_secs_f64(0.12))
                        .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                    move |this, delta| {
                        this.left(
                            arrow_hover_start + (arrow_hover_target - arrow_hover_start) * delta,
                        )
                    },
                )
                .into_any_element()
        };
        div()
            .w_full()
            .flex_shrink_0()
            .px_4()
            .pt_4()
            .pb_3()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .relative()
                    .w_full()
                    .h(px(43.))
                    .flex()
                    .items_center()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(if composer_focused {
                        cx.theme().primary.opacity(0.7)
                    } else if has_url_error {
                        cx.theme().danger.opacity(0.72)
                    } else {
                        cx.theme().border
                    })
                    .bg(cx.theme().muted)
                    .when(composer_focused, |this| {
                        this.child(
                            div()
                                .absolute()
                                .top(px(-4.))
                                .left(px(-4.))
                                .right(px(-4.))
                                .bottom(px(-4.))
                                .rounded(cx.theme().radius + px(4.))
                                .border(px(3.))
                                .border_color(cx.theme().primary.opacity(0.12)),
                        )
                    })
                    .child(
                        div()
                            .w(px(104.))
                            .h_full()
                            .flex_shrink_0()
                            .border_r_1()
                            .border_color(cx.theme().border)
                            .child(
                                Select::new(&method)
                                    .large()
                                    .appearance(false)
                                    .px(px(13.))
                                    .text_size(px(11.))
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .font_weight(FontWeight::BOLD)
                                    .text_color(method_color(method_value, cx))
                                    .menu_width(px(144.)),
                            ),
                    )
                    .child(div().flex_1().min_w_0().h_full().child(url_input))
                    .child(
                        div().w(px(94.)).h_full().p_1().flex_shrink_0().child(
                            div()
                                .id("send")
                                .relative()
                                .size_full()
                                .overflow_hidden()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().primary)
                                .text_color(cx.theme().primary_foreground)
                                .text_size(px(12.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .flex()
                                .items_center()
                                .justify_center()
                                .gap_2()
                                .when(can_stop, |this| {
                                    this.cursor_pointer()
                                        .hover(|this| this.bg(cx.theme().primary.opacity(0.88)))
                                        .on_click(cx.listener(Self::stop_websocket))
                                })
                                .when(!sending, |this| {
                                    this.cursor_pointer()
                                        .hover(|this| this.bg(cx.theme().primary.opacity(0.88)))
                                        .on_click(cx.listener(Self::send_request))
                                })
                                .child(send_label)
                                .child(send_arrow)
                                .when(sending && !can_stop, |this| {
                                    this.child(
                                        div()
                                            .absolute()
                                            .left_0()
                                            .bottom_0()
                                            .h(px(2.))
                                            .bg(cx.theme().primary_foreground.opacity(0.82))
                                            .with_animation(
                                                SharedString::from(format!(
                                                    "send-progress-{send_animation_id}"
                                                )),
                                                Animation::new(Duration::from_secs_f64(0.76))
                                                    .repeat()
                                                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                                |this, delta| this.w(relative(delta)),
                                            ),
                                    )
                                })
                                .on_hover(move |hovered, _, cx| {
                                    send_hover_app.update(cx, |this, cx| {
                                        if this.send_hovered != *hovered {
                                            this.send_hovered = *hovered;
                                            cx.notify();
                                        }
                                    });
                                })
                                .with_animation(
                                    SharedString::from(format!(
                                        "send-hover-{send_animation_id}-{send_hovered}"
                                    )),
                                    Animation::new(Duration::from_secs_f64(0.12))
                                        .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                    move |this, delta| {
                                        this.top(
                                            send_hover_start
                                                + (send_hover_target - send_hover_start) * delta,
                                        )
                                    },
                                ),
                        ),
                    ),
            )
            .child(self.render_editor(cx))
    }

    fn render_editor(&self, cx: &mut Context<Self>) -> Div {
        let tab = &self.tabs[self.active_tab];
        let current = tab.section;
        let app = cx.entity();
        let mut tabs = TabBar::new("request-editor-sections")
            .underline()
            .selected_index(current as usize)
            .on_click(move |index, _, cx| {
                let Some(section) = EditorSection::ALL.get(*index).copied() else {
                    return;
                };
                app.update(cx, |this, cx| this.select_section(section, cx));
            });
        for section in EditorSection::ALL {
            let count = match section {
                EditorSection::Parameters => {
                    tab.parameters.iter().filter(|pair| pair.enabled).count()
                }
                EditorSection::Headers => tab.headers.iter().filter(|pair| pair.enabled).count(),
                // Avoid materializing the entire rope merely to render a badge.
                // Large request bodies otherwise turn every app render into an O(n) copy.
                EditorSection::Body => 0,
            };
            tabs = tabs.child(Tab::new().label(section.label()).when(count > 0, |this| {
                this.suffix(
                    Tag::secondary()
                        .small()
                        .rounded_full()
                        .child(count.to_string()),
                )
            }));
        }

        div()
            .h(px(240.))
            .flex_shrink_0()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(tabs.h(px(43.)).px_3())
            .child(match current {
                EditorSection::Body => self.render_body(cx),
                EditorSection::Parameters | EditorSection::Headers => self.render_pairs(cx),
            })
    }

    fn render_body(&self, cx: &mut Context<Self>) -> Div {
        let tab = &self.tabs[self.active_tab];
        let body = tab.body.clone();
        let state = &tab.body_template_state;
        div().flex_1().min_h_0().overflow_hidden().p_3().child(
            div()
                .relative()
                .size_full()
                .when(matches!(state, TemplateVisualState::Error(_)), |this| {
                    this.border_1()
                        .border_color(cx.theme().danger.opacity(0.7))
                        .rounded(cx.theme().radius)
                })
                .child(Input::new(&body).size_full())
                .when(matches!(state, TemplateVisualState::Valid), |this| {
                    this.child(
                        div()
                            .absolute()
                            .top_2()
                            .right_2()
                            .px_2()
                            .py_0p5()
                            .rounded_full()
                            .bg(cx.theme().primary.opacity(0.14))
                            .text_color(cx.theme().primary)
                            .text_xs()
                            .child("Variables"),
                    )
                })
                .when_some(
                    match &state {
                        TemplateVisualState::Error(message) => Some(message.clone()),
                        _ => None,
                    },
                    |this, message| {
                        this.child(
                            Button::new("body-variable-error")
                                .absolute()
                                .top_2()
                                .right_2()
                                .ghost()
                                .small()
                                .compact()
                                .icon(IconName::TriangleAlert)
                                .tooltip(message),
                        )
                    },
                ),
        )
    }

    fn render_pairs(&self, cx: &mut Context<Self>) -> Div {
        let tab = &self.tabs[self.active_tab];
        let app = cx.entity();
        let pairs: &[PairInputs] = match tab.section {
            EditorSection::Parameters => &tab.parameters,
            EditorSection::Headers => &tab.headers,
            EditorSection::Body => &[],
        };
        let mut list = div().w_full().p_2().flex().flex_col().gap(px(2.));
        for (index, pair) in pairs.iter().enumerate() {
            let key = pair.key.clone();
            let value = pair.value.clone();
            let enabled = pair.enabled;
            let checkbox_app = app.clone();
            let key_state = if enabled {
                pair.key_template_state.borrow().clone()
            } else {
                TemplateVisualState::Plain
            };
            let value_state = if enabled {
                pair.value_template_state.borrow().clone()
            } else {
                TemplateVisualState::Plain
            };
            let has_error = matches!(key_state, TemplateVisualState::Error(_))
                || matches!(value_state, TemplateVisualState::Error(_));
            let mut key_input = Input::new(&key)
                .small()
                .appearance(false)
                .focus_bordered(false)
                .w_full()
                .disabled(!enabled);
            key_input = match &key_state {
                TemplateVisualState::Plain => key_input,
                TemplateVisualState::Valid => key_input.text_color(cx.theme().primary),
                TemplateVisualState::Error(message) => {
                    key_input.text_color(cx.theme().danger).suffix(
                        Button::new(("pair-key-variable-error", index))
                            .ghost()
                            .small()
                            .compact()
                            .icon(IconName::TriangleAlert)
                            .tooltip(message.clone()),
                    )
                }
            };
            let mut value_input = Input::new(&value)
                .small()
                .appearance(false)
                .focus_bordered(false)
                .w_full()
                .disabled(!enabled);
            value_input = match &value_state {
                TemplateVisualState::Plain => value_input,
                TemplateVisualState::Valid => value_input.text_color(cx.theme().primary),
                TemplateVisualState::Error(message) => {
                    value_input.text_color(cx.theme().danger).suffix(
                        Button::new(("pair-value-variable-error", index))
                            .ghost()
                            .small()
                            .compact()
                            .icon(IconName::TriangleAlert)
                            .tooltip(message.clone()),
                    )
                }
            };
            list = list.child(
                div()
                    .w_full()
                    .h(px(35.))
                    .px_1()
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded(cx.theme().radius)
                    .when(has_error, |this| {
                        this.border_1().border_color(cx.theme().danger.opacity(0.5))
                    })
                    .hover(|this| this.bg(cx.theme().muted))
                    .child(
                        div()
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Checkbox::new(("enable-pair", index))
                                    .small()
                                    .checked(enabled)
                                    .on_click(move |_, _, cx| {
                                        checkbox_app
                                            .update(cx, |this, cx| this.toggle_pair(index, cx));
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(32.))
                            .flex()
                            .items_center()
                            .child(key_input),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(32.))
                            .flex()
                            .items_center()
                            .child(value_input),
                    )
                    .child(
                        div()
                            .size(px(32.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(
                                Button::new(("remove-pair", index))
                                    .ghost()
                                    .small()
                                    .compact()
                                    .icon(IconName::Close)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_pair(index, cx)
                                    })),
                            ),
                    ),
            );
        }
        let list = list.child(
            div().mt_1().flex().child(
                Button::new("add-pair")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .label("Add row")
                    .on_click(cx.listener(|this, _, window, cx| this.add_pair(window, cx))),
            ),
        );
        div()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .child(list.h_full().overflow_y_scrollbar())
    }

    fn build_formatted_response_views(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Option<AnyElement>, Vec<AnyElement>) {
        let show_active = self.tabs[self.active_tab].response.is_some()
            && self.tabs[self.active_tab].response_view == ResponseViewMode::Formatted;
        let editor_background = cx
            .theme()
            .highlight_theme
            .style
            .editor_background
            .unwrap_or(cx.theme().background);
        let mut code_block = div().p_0().rounded_none().bg(editor_background);
        let mut text_view_style = TextViewStyle::default()
            .paragraph_gap(rems(0.))
            .code_block(code_block.style().clone());
        text_view_style.highlight_theme = cx.theme().highlight_theme.clone();
        let mut active_view = None;
        let mut keepalive_views = Vec::new();

        for (index, tab) in self.tabs.iter_mut().enumerate() {
            let Some(editors) = tab.response_editors.as_mut() else {
                continue;
            };
            if editors.formatted_markdown.is_empty() {
                editors.formatted_ready = false;
                continue;
            }
            editors.formatted_published = true;
            let view_id = SharedString::from(format!(
                "response-formatted-{}-{}",
                tab.draft.id, tab.response_revision
            ));
            let view = TextView::markdown(view_id, editors.formatted_markdown.clone(), window, cx)
                .style(text_view_style.clone())
                .selectable(true)
                .scrollable(true)
                .select_all_text(editors.formatted_text.clone())
                .size_full();
            editors.formatted_ready = editors.formatted_published && view.is_current(cx);
            let container = div().size_full().min_h_0().child(view).into_any_element();

            // Once initialized, TextView retains its last parsed (highlighted)
            // document while newer chunks or a theme are parsed off-thread.
            if index == self.active_tab && show_active {
                active_view = Some(container);
            } else {
                keepalive_views.push(div().hidden().child(container).into_any_element());
            }
        }
        (active_view, keepalive_views)
    }

    fn websocket_message_row(
        &self,
        request_id: &str,
        index: usize,
        app: Entity<Self>,
        cx: &mut App,
    ) -> Option<Div> {
        let session = self
            .tabs
            .iter()
            .find(|tab| tab.draft.id == request_id)?
            .websocket
            .as_ref()?;
        let message = session.messages.get(index)?;
        let sequence = message.sequence;
        let selected = session.selected_sequence == Some(sequence);
        let direction_color = match message.direction {
            WebSocketDirection::Sent => cx.theme().primary,
            WebSocketDirection::Received => cx.theme().success,
        };
        Some(
            div().h(px(57.)).pb(px(2.)).child(
                div()
                    .id(SharedString::from(format!("websocket-message-{sequence}")))
                    .size_full()
                    .px_2()
                    .py_2()
                    .rounded(cx.theme().radius)
                    .border_1()
                    .border_color(if selected {
                        cx.theme().primary.opacity(0.45)
                    } else {
                        cx.theme().transparent
                    })
                    .bg(if selected {
                        cx.theme().secondary.lighten(0.06)
                    } else {
                        cx.theme().transparent
                    })
                    .hover(|this| this.bg(cx.theme().secondary.opacity(0.72)))
                    .cursor_pointer()
                    .on_click(move |_, window, cx| {
                        app.update(cx, |this, cx| {
                            this.select_websocket_message(sequence, window, cx)
                        });
                    })
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .text_size(px(9.))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(direction_color)
                                    .child(match message.direction {
                                        WebSocketDirection::Sent => "→",
                                        WebSocketDirection::Received => "←",
                                    })
                                    .child(message.kind.label()),
                            )
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{} ms", message.elapsed_ms)),
                            ),
                    )
                    .child(
                        div()
                            .mt_1()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(10.))
                            .text_color(cx.theme().foreground.opacity(0.82))
                            .overflow_hidden()
                            .text_ellipsis()
                            .child(websocket_message_preview(&message.body)),
                    ),
            ),
        )
    }

    fn build_websocket_messages_view(&self, cx: &mut Context<Self>) -> Option<Div> {
        let tab = &self.tabs[self.active_tab];
        let session = tab.websocket.as_ref()?;
        let request_id = tab.draft.id.clone();
        let message_count = session.messages.len();
        let app = cx.entity();
        let mut message_list = div().flex_1().min_h_0().p_2().flex().flex_col();
        if session.dropped_messages > 0 {
            message_list = message_list.child(
                div()
                    .px_2()
                    .py_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "{} older messages released from memory",
                        session.dropped_messages
                    )),
            );
        }
        message_list = message_list.child(
            uniform_list(
                SharedString::from(format!("websocket-messages-{request_id}")),
                message_count,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .filter_map(|index| {
                            this.websocket_message_row(&request_id, index, app.clone(), cx)
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_h_0(),
        );

        let selected = session.selected();
        let inspector = session.inspector.clone();
        let detail = if let Some(message) = selected {
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(36.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .child(
                            div()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(match message.direction {
                                    WebSocketDirection::Sent => cx.theme().primary,
                                    WebSocketDirection::Received => cx.theme().success,
                                })
                                .child(format!(
                                    "{} {}",
                                    message.direction.label(),
                                    message.kind.label()
                                )),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(format_size(message.size_bytes)),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{} ms", message.elapsed_ms)),
                        )
                        .when(message.truncated, |this| {
                            this.child(
                                div()
                                    .px_2()
                                    .py_0p5()
                                    .rounded_full()
                                    .bg(cx.theme().warning.opacity(0.13))
                                    .text_color(cx.theme().warning)
                                    .child("Preview truncated"),
                            )
                        }),
                )
                .child(
                    div().flex_1().min_h_0().p_3().child(
                        Input::new(&inspector)
                            .appearance(false)
                            .focus_bordered(false)
                            .disabled(true)
                            .size_full(),
                    ),
                )
        } else {
            empty_state(
                "Waiting for messages…",
                "Each incoming and initial outgoing message will appear here",
                cx,
            )
        };

        Some(
            div()
                .size_full()
                .min_h_0()
                .flex()
                .child(
                    div()
                        .w(px(238.))
                        .h_full()
                        .flex_shrink_0()
                        .border_r_1()
                        .border_color(cx.theme().border)
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .h(px(36.))
                                .px_3()
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Messages")
                                .child(
                                    div()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(session.messages.len().to_string()),
                                ),
                        )
                        .child(message_list),
                )
                .child(detail),
        )
    }

    fn render_response(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let (formatted_view, keepalive_views) = self.build_formatted_response_views(window, cx);
        let websocket_messages_view = self.build_websocket_messages_view(cx);
        let tab = &self.tabs[self.active_tab];
        let websocket = tab.websocket.is_some();
        let connection_state = tab.websocket.as_ref().map(|session| session.state);
        let response_indicator = match connection_state {
            Some(WebSocketConnectionState::Connecting | WebSocketConnectionState::Stopping) => {
                cx.theme().primary
            }
            Some(WebSocketConnectionState::Failed) => cx.theme().danger,
            Some(WebSocketConnectionState::Closed) => cx.theme().muted_foreground,
            Some(WebSocketConnectionState::Open) | None => cx.theme().success,
        };
        let header = div()
            .h(px(43.))
            .px_3()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .text_size(px(11.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(
                        div()
                            .size(px(12.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(response_indicator.opacity(0.14))
                            .child(div().size(px(6.)).rounded_full().bg(response_indicator)),
                    )
                    .child(if websocket { "WebSocket" } else { "Response" }),
            )
            .when(tab.response.is_some(), |this| {
                let formatted = tab.response_view == ResponseViewMode::Formatted;
                let raw = tab.response_view == ResponseViewMode::Raw;
                let headers = tab.response_view == ResponseViewMode::Headers;
                let messages = tab.response_view == ResponseViewMode::Messages;
                let formatted_app = cx.entity();
                let raw_app = formatted_app.clone();
                let headers_app = formatted_app.clone();
                let messages_app = formatted_app.clone();
                let connection_app = formatted_app.clone();
                let mode_button_style = ButtonCustomVariant::new(cx)
                    .color(cx.theme().transparent)
                    .foreground(cx.theme().muted_foreground)
                    .border(cx.theme().transparent)
                    .hover(cx.theme().secondary.lighten(0.1))
                    .active(cx.theme().muted);
                let copy_button_style = ButtonCustomVariant::new(cx)
                    .color(cx.theme().muted)
                    .foreground(cx.theme().muted_foreground)
                    .border(cx.theme().border)
                    .hover(cx.theme().secondary.lighten(0.1))
                    .active(cx.theme().secondary.lighten(0.04));
                let copy_label = if self.copy_feedback_active {
                    "Copied"
                } else {
                    "Copy"
                };
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(7.))
                        .child(
                            div()
                                .h(px(27.))
                                .p(px(2.))
                                .flex()
                                .items_center()
                                .border_1()
                                .border_color(cx.theme().border)
                                .rounded(px(7.))
                                .bg(cx.theme().background)
                                .when(websocket, |this| {
                                    this.child(
                                        Button::new("response-messages")
                                            .custom(mode_button_style)
                                            .with_size(px(23.))
                                            .rounded(px(5.))
                                            .w(px(62.))
                                            .px_2()
                                            .selected(messages)
                                            .when(messages, |this| {
                                                this.bg(cx.theme().secondary.lighten(0.1))
                                                    .text_color(cx.theme().foreground)
                                                    .shadow_xs()
                                            })
                                            .child(
                                                div()
                                                    .text_size(px(9.))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(if messages {
                                                        cx.theme().foreground
                                                    } else {
                                                        cx.theme().muted_foreground.opacity(0.72)
                                                    })
                                                    .child("Messages"),
                                            )
                                            .on_click(move |_, _, cx| {
                                                messages_app.update(cx, |this, cx| {
                                                    this.set_response_view(
                                                        ResponseViewMode::Messages,
                                                        cx,
                                                    )
                                                });
                                            }),
                                    )
                                })
                                .when(!websocket, |this| {
                                    this.child(
                                        Button::new("response-formatted")
                                            .custom(mode_button_style)
                                            .with_size(px(23.))
                                            .rounded(px(5.))
                                            .w(px(64.))
                                            .px_2()
                                            .selected(formatted)
                                            .when(formatted, |this| {
                                                this.bg(cx.theme().secondary.lighten(0.1))
                                                    .text_color(cx.theme().foreground)
                                                    .shadow_xs()
                                            })
                                            .child(
                                                div()
                                                    .text_size(px(9.))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(if formatted {
                                                        cx.theme().foreground
                                                    } else {
                                                        cx.theme().muted_foreground.opacity(0.72)
                                                    })
                                                    .child("Formatted"),
                                            )
                                            .on_click(move |_, _, cx| {
                                                formatted_app.update(cx, |this, cx| {
                                                    this.set_response_view(
                                                        ResponseViewMode::Formatted,
                                                        cx,
                                                    )
                                                });
                                            }),
                                    )
                                })
                                .when(!websocket, |this| {
                                    this.child(
                                        Button::new("response-raw")
                                            .custom(mode_button_style)
                                            .with_size(px(23.))
                                            .rounded(px(5.))
                                            .w(px(48.))
                                            .px_2()
                                            .selected(raw)
                                            .when(raw, |this| {
                                                this.bg(cx.theme().secondary.lighten(0.1))
                                                    .text_color(cx.theme().foreground)
                                                    .shadow_xs()
                                            })
                                            .child(
                                                div()
                                                    .text_size(px(9.))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(if raw {
                                                        cx.theme().foreground
                                                    } else {
                                                        cx.theme().muted_foreground.opacity(0.72)
                                                    })
                                                    .child("Raw"),
                                            )
                                            .on_click(move |_, _, cx| {
                                                raw_app.update(cx, |this, cx| {
                                                    this.set_response_view(
                                                        ResponseViewMode::Raw,
                                                        cx,
                                                    )
                                                });
                                            }),
                                    )
                                })
                                .child(
                                    Button::new("response-headers")
                                        .custom(mode_button_style)
                                        .with_size(px(23.))
                                        .rounded(px(5.))
                                        .w(px(58.))
                                        .px_2()
                                        .selected(headers)
                                        .when(headers, |this| {
                                            this.bg(cx.theme().secondary.lighten(0.1))
                                                .text_color(cx.theme().foreground)
                                                .shadow_xs()
                                        })
                                        .child(
                                            div()
                                                .text_size(px(9.))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(if headers {
                                                    cx.theme().foreground
                                                } else {
                                                    cx.theme().muted_foreground.opacity(0.72)
                                                })
                                                .child("Headers"),
                                        )
                                        .on_click(move |_, _, cx| {
                                            headers_app.update(cx, |this, cx| {
                                                this.set_response_view(
                                                    ResponseViewMode::Headers,
                                                    cx,
                                                )
                                            });
                                        }),
                                ),
                        )
                        .when(websocket, |this| {
                            let active = tab.sending;
                            this.child(
                                Button::new("websocket-connection-control")
                                    .custom(copy_button_style)
                                    .h(px(27.))
                                    .w(px(78.))
                                    .rounded(px(6.))
                                    .px_2()
                                    .cursor_pointer()
                                    .child(
                                        div()
                                            .text_size(px(10.))
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(if active { "Stop" } else { "Reconnect" }),
                                    )
                                    .on_click(move |event, window, cx| {
                                        connection_app.update(cx, |this, cx| {
                                            if active {
                                                this.stop_websocket(event, window, cx);
                                            } else {
                                                this.send_request(event, window, cx);
                                            }
                                        });
                                    }),
                            )
                        })
                        .child(
                            Button::new("copy-response")
                                .custom(copy_button_style)
                                .h(px(27.))
                                .w(px(74.))
                                .rounded(px(6.))
                                .px_2()
                                .cursor_pointer()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(6.))
                                        .text_size(px(10.))
                                        .child(Icon::new(IconName::Copy).size_3())
                                        .child(copy_label),
                                )
                                .on_click(cx.listener(Self::copy_response)),
                        ),
                )
            });

        let content = if tab.sending && tab.response.is_none() {
            if websocket {
                empty_state("Connecting…", "Waiting for the WebSocket handshake", cx)
            } else {
                empty_state("Sending request…", "Waiting for response headers", cx)
            }
        } else if let Some(error) = &tab.error {
            div()
                .flex_1()
                .min_h_0()
                .p_4()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_color(cx.theme().danger)
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Request failed"),
                )
                .child(
                    Label::new(error.clone())
                        .text_sm()
                        .text_color(cx.theme().muted_foreground),
                )
        } else if let Some(response) = &tab.response {
            let websocket_messages_view = (tab.response_view == ResponseViewMode::Messages)
                .then_some(websocket_messages_view)
                .flatten();
            let formatted_published = tab
                .response_editors
                .as_ref()
                .is_some_and(|editors| editors.formatted_published);
            let raw_editor = (tab.response_view == ResponseViewMode::Raw
                || (tab.response_view == ResponseViewMode::Formatted && !formatted_published))
                .then(|| {
                    tab.response_editors
                        .as_ref()
                        .map(|editors| editors.raw.clone())
                })
                .flatten();
            let status_color = if response.status < 400 {
                cx.theme().success
            } else {
                cx.theme().danger
            };
            let headers_view = (tab.response_view == ResponseViewMode::Headers).then(|| {
                let mut rows = div().size_full().overflow_y_scrollbar().flex().flex_col();
                if response.headers.is_empty() {
                    rows = rows.child(
                        Label::new("This response did not include any headers")
                            .text_sm()
                            .text_color(cx.theme().muted_foreground),
                    );
                } else {
                    for (index, (name, value)) in response.headers.iter().enumerate() {
                        rows = rows.child(
                            div()
                                .w_full()
                                .min_h(px(34.))
                                .py_2()
                                .flex()
                                .items_start()
                                .gap_4()
                                .when(index + 1 < response.headers.len(), |this| {
                                    this.border_b_1().border_color(cx.theme().border)
                                })
                                .child(
                                    div()
                                        .w(px(190.))
                                        .flex_shrink_0()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(cx.theme().primary)
                                        .child(name.clone()),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_xs()
                                        .text_color(cx.theme().foreground)
                                        .child(value.clone()),
                                ),
                        );
                    }
                }
                rows
            });
            let payload_mode = match tab.response_view {
                ResponseViewMode::Formatted => "formatted",
                ResponseViewMode::Raw => "raw",
                ResponseViewMode::Headers => "headers",
                ResponseViewMode::Messages => "messages",
            };
            let payload = div()
                .relative()
                .size_full()
                .min_h_0()
                .when_some(formatted_view, |this, viewer| this.child(viewer))
                .when_some(raw_editor, |this, editor| {
                    this.child(
                        Input::new(&editor)
                            .appearance(false)
                            .focus_bordered(false)
                            .disabled(true)
                            .h_full()
                            .size_full(),
                    )
                })
                .when_some(headers_view, |this, headers| this.child(headers))
                .when_some(websocket_messages_view, |this, messages| {
                    this.child(messages)
                })
                .with_animation(
                    SharedString::from(format!(
                        "response-payload-{}-{}-{payload_mode}",
                        tab.draft.id, tab.response_revision
                    )),
                    Animation::new(Duration::from_secs_f64(0.28))
                        .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                    |this, delta| this.opacity(delta).top(px(4.) * (1.0 - delta)),
                );
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(35.))
                        .px_3()
                        .flex()
                        .items_center()
                        .gap_4()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .text_sm()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .text_color(status_color)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(div().size(px(5.)).rounded_full().bg(status_color))
                                .child(format!("{} {}", response.status, response.status_text))
                                .with_animation(
                                    SharedString::from(format!(
                                        "response-status-{}-{}",
                                        tab.draft.id, tab.response_revision
                                    )),
                                    Animation::new(Duration::from_secs_f64(0.28))
                                        .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                    |this, delta| this.opacity(delta),
                                ),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("{} ms", response.elapsed_ms)),
                        )
                        .child(
                            div()
                                .text_color(cx.theme().muted_foreground)
                                .child(format_size(response.size_bytes)),
                        )
                        .when_some(connection_state, |this, state| {
                            this.child(
                                div()
                                    .text_color(match state {
                                        WebSocketConnectionState::Open
                                        | WebSocketConnectionState::Connecting
                                        | WebSocketConnectionState::Stopping => cx.theme().primary,
                                        WebSocketConnectionState::Closed => {
                                            cx.theme().muted_foreground
                                        }
                                        WebSocketConnectionState::Failed => cx.theme().danger,
                                    })
                                    .child(state.label()),
                            )
                        })
                        .when(!websocket && tab.sending, |this| {
                            this.child(div().text_color(cx.theme().primary).child("Streaming…"))
                        })
                        .when_some(
                            tab.websocket
                                .as_ref()
                                .and_then(|session| session.detail.clone()),
                            |this, detail| {
                                this.child(
                                    div()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(detail),
                                )
                            },
                        ),
                )
                .child(
                    div()
                        .flex_1()
                        .min_h_0()
                        .p_3()
                        .bg(cx
                            .theme()
                            .highlight_theme
                            .style
                            .editor_background
                            .unwrap_or(cx.theme().background))
                        .rounded(cx.theme().radius)
                        .child(payload),
                )
        } else {
            empty_state(
                "No response yet",
                "Send or connect to inspect status, timing, headers, and payloads",
                cx,
            )
        };

        div()
            .flex_1()
            .min_h_0()
            .mx_4()
            .mb_4()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().sidebar)
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(header)
            .child(content)
            .children(keepalive_views)
    }
}

impl Render for AlulaApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // gpui-component exposes these Root-managed layers separately. Keeping
        // them at the end preserves correct occlusion and native focus handling.
        let sheet_layer = Root::render_sheet_layer(window, cx);
        let dialog_layer = Root::render_dialog_layer(window, cx);
        let notification_layer = Root::render_notification_layer(window, cx);
        let (request_workspace, secondary_workspace) = match self.workspace_section {
            WorkspaceSection::Requests => (self.render_requests_page(window, cx), div().hidden()),
            WorkspaceSection::Environments => (
                self.render_request_keepalive(window, cx),
                self.render_environments(cx),
            ),
            WorkspaceSection::History => (
                self.render_request_keepalive(window, cx),
                self.render_history(cx),
            ),
        };
        let main_content = div()
            .size_full()
            .min_h_0()
            .child(request_workspace)
            .child(secondary_workspace);
        div()
            .size_full()
            .track_focus(&self.focus_handle)
            .font_family(cx.theme().font_family.clone())
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .flex()
            .flex_col()
            .on_action(cx.listener(Self::shortcut_create_new))
            .on_action(cx.listener(Self::shortcut_close_tab))
            .on_action(cx.listener(Self::shortcut_next_tab))
            .on_action(cx.listener(Self::shortcut_previous_tab))
            .on_action(cx.listener(Self::shortcut_send_request))
            .on_action(cx.listener(Self::shortcut_show_parameters))
            .on_action(cx.listener(Self::shortcut_show_headers))
            .on_action(cx.listener(Self::shortcut_show_body))
            .on_action(cx.listener(Self::shortcut_copy_response))
            .on_action(cx.listener(Self::shortcut_add_parameter))
            .on_action(cx.listener(Self::shortcut_add_header))
            .on_action(cx.listener(Self::shortcut_show_requests))
            .on_action(cx.listener(Self::shortcut_show_environments))
            .on_action(cx.listener(Self::shortcut_show_history))
            .on_action(cx.listener(Self::shortcut_open_settings))
            .on_action(cx.listener(Self::shortcut_open_command_palette))
            .on_action(cx.listener(Self::shortcut_focus_url))
            .on_action(cx.listener(Self::shortcut_show_formatted_response))
            .on_action(cx.listener(Self::shortcut_show_raw_response))
            .child(self.render_top_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_sidebar(window, cx))
                    .child(div().flex_1().min_w_0().min_h_0().child(main_content)),
            )
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
            .with_animation(
                "alula-app-enter",
                Animation::new(Duration::from_secs_f64(0.28))
                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                |this, delta| this.opacity(delta),
            )
    }
}

fn empty_state(title: &'static str, subtitle: &'static str, cx: &App) -> Div {
    div()
        .flex_1()
        .min_h_0()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_2()
        .child(
            div()
                .mb_1()
                .size(px(38.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(cx.theme().radius_lg)
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().muted)
                .child(
                    div()
                        .size(px(8.))
                        .rounded_full()
                        .bg(cx.theme().primary.opacity(0.72)),
                ),
        )
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            Label::new(subtitle)
                .text_xs()
                .text_color(cx.theme().muted_foreground),
        )
}

fn method_badge(method: HttpMethod, cx: &App) -> Div {
    let color = method_color(method, cx);
    div()
        .w(px(58.))
        .h(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(color.opacity(0.1))
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(9.))
        .font_weight(FontWeight::BOLD)
        .text_color(color)
        .child(if method == HttpMethod::Delete {
            "DEL"
        } else {
            method.as_str()
        })
}

fn method_color(method: HttpMethod, cx: &App) -> Hsla {
    match method {
        HttpMethod::Get => cx.theme().success,
        HttpMethod::Post => cx.theme().info,
        HttpMethod::Put => cx.theme().warning,
        HttpMethod::Patch => cx.theme().primary,
        HttpMethod::Delete => cx.theme().danger,
        HttpMethod::Head | HttpMethod::Options => cx.theme().muted_foreground,
    }
}

fn register_response_languages() {
    let registry = LanguageRegistry::singleton();
    registry.register(
        "css",
        &LanguageConfig::new(
            "css",
            tree_sitter_css::LANGUAGE.into(),
            vec![],
            tree_sitter_css::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
    );
    registry.register(
        "javascript",
        &LanguageConfig::new(
            "javascript",
            tree_sitter_javascript::LANGUAGE.into(),
            vec!["json".into(), "css".into(), "html".into()],
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
    );
    registry.register(
        "html",
        &LanguageConfig::new(
            "html",
            tree_sitter_html::LANGUAGE.into(),
            vec!["javascript".into(), "css".into()],
            tree_sitter_html::HIGHLIGHTS_QUERY,
            tree_sitter_html::INJECTIONS_QUERY,
            "",
        ),
    );
    registry.register(
        "xml",
        &LanguageConfig::new(
            "xml",
            tree_sitter_html::LANGUAGE.into(),
            vec![],
            tree_sitter_html::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
    );
}

fn parse_http_method(value: &str) -> Option<HttpMethod> {
    HttpMethod::ALL
        .into_iter()
        .find(|method| method.as_str().eq_ignore_ascii_case(value.trim()))
}

fn parse_key_value_fields(value: &Value) -> Result<Vec<KeyValueField>, String> {
    match value {
        Value::Object(entries) => entries
            .iter()
            .map(|(key, value)| {
                value
                    .as_str()
                    .map(|value| KeyValueField::new(key, value))
                    .ok_or_else(|| format!("value for {key:?} must be a string"))
            })
            .collect(),
        Value::Array(entries) => entries
            .iter()
            .map(|entry| {
                let key = entry
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or_else(|| "each field must contain a string key".to_owned())?;
                let value = entry.get("value").and_then(Value::as_str).unwrap_or("");
                let mut field = KeyValueField::new(key, value);
                field.enabled = entry
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true);
                Ok(field)
            })
            .collect(),
        _ => Err("fields must be an object or an array of key/value rows".into()),
    }
}

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    }
}

fn websocket_message_preview(body: &str) -> String {
    const PREVIEW_CHARACTERS: usize = 72;
    let mut preview = String::with_capacity(PREVIEW_CHARACTERS);
    let mut characters = body.chars();
    for character in characters.by_ref().take(PREVIEW_CHARACTERS) {
        preview.push(if matches!(character, '\n' | '\r' | '\t') {
            ' '
        } else {
            character
        });
    }
    if characters.next().is_some() {
        preview.push('…');
    }
    if preview.is_empty() {
        "Empty message".into()
    } else {
        preview
    }
}

fn relative_history_time(sent_at_unix_ms: u64) -> String {
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64;
    let elapsed_seconds = now.saturating_sub(sent_at_unix_ms) / 1_000;
    match elapsed_seconds {
        0..=59 => "just now".into(),
        60..=3_599 => format!("{} min ago", elapsed_seconds / 60),
        3_600..=86_399 => format!("{} h ago", elapsed_seconds / 3_600),
        86_400..=2_591_999 => format!("{} d ago", elapsed_seconds / 86_400),
        2_592_000..=31_535_999 => format!("{} mo ago", elapsed_seconds / 2_592_000),
        _ => format!("{} y ago", elapsed_seconds / 31_536_000),
    }
}

fn main() -> Result<()> {
    install_tls_crypto_provider();
    let app = Application::new().with_assets(Assets);
    app.run(|cx| {
        gpui_component::init(cx);
        if let Err(error) = cx
            .text_system()
            .add_fonts(vec![Cow::Borrowed(include_bytes!(
                "../assets/fonts/InterVariable.ttf"
            ))])
        {
            eprintln!("could not load bundled Inter font: {error:#}");
        }
        let component_bindings = cx.key_bindings().borrow().bindings().cloned().collect();
        cx.set_global(ComponentKeyBindings(component_bindings));
        register_response_languages();
        let theme_path = config_path();
        let theme_config = AppConfig::load_or_create(&theme_path).unwrap_or_else(|error| {
            eprintln!("could not load theme configuration: {error:#}");
            AppConfig::default()
        });
        if let Err(error) = apply_theme(&theme_config, cx) {
            eprintln!("could not apply theme configuration: {error:#}");
        }
        install_key_bindings(&theme_config, cx);
        let state_paths = StatePaths::beside(&theme_path);
        let persisted = PersistedState::load(&state_paths).unwrap_or_else(|error| {
            eprintln!("could not load persistent workspace state: {error:#}");
            PersistedState {
                workspace: Workspace::default(),
                history: HistoryStore::default(),
                environments: EnvironmentStore::default(),
            }
        });
        let bounds = Bounds::centered(None, size(px(1320.), px(860.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                let view =
                    cx.new(|cx| AlulaApp::new(theme_config, theme_path, persisted, window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("failed to open Alula window");
        cx.activate(true);
    });
    Ok(())
}
