#![recursion_limit = "256"]
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use alula::{
    AddHeader, AddParameter, AgentReply, AppConfig, CloseTab, CopyResponseBody, CreateNew,
    EnvironmentAgentCommand, EnvironmentFolder, EnvironmentStore, EnvironmentVariable, FocusUrl,
    HistoryAgentCommand, HistoryEntry, HistoryStore, HttpMethod, HttpSession, HttpStreamEvent,
    KeyValueField, McpHttpServer, McpToolHandler, NextTab, OpenCommandPalette, OpenSettings,
    PersistedState, PreviousTab, QuitApplication, RequestDraft, ResponseBodyCache,
    ResponseSnapshot, SendRequest, SettingsView, ShowBody, ShowEnvironments, ShowFormattedResponse,
    ShowHeaders, ShowHistory, ShowParameters, ShowRawResponse, ShowRequests, StatePaths,
    ThemeAgentCommand, WebSocketDirection, WebSocketExecutor, WebSocketMessageSnapshot,
    WebSocketStreamEvent, Workspace, application_key_bindings, apply_environment_agent_command,
    apply_history_agent_command, apply_theme, apply_theme_agent_command,
    chunked_fenced_code_blocks, config_path, delete_secret, inspect_template,
    install_tls_crypto_provider, is_websocket_request, load_secret, reply_to_tool, resolve_request,
    store_secret, syntax_language, trim_response_formatting_start, valid_variable_name,
};
use anyhow::Result;
use gpui::{prelude::*, *};
use gpui_component::{
    ActiveTheme as _, Colorize as _, Icon, IconName, IndexPath, Root, Selectable as _,
    Sizable as _, WindowExt as _,
    animation::{StableAnimationExt as _, cubic_bezier},
    button::{Button, ButtonCustomVariant, ButtonVariant, ButtonVariants as _},
    checkbox::Checkbox,
    dialog::DialogButtonProps,
    highlighter::{LanguageConfig, LanguageRegistry},
    input::{CompletionProvider, Input, InputEvent, InputState, RopeExt as _},
    label::Label,
    menu::{ContextMenuExt as _, DropdownMenu as _, PopupMenuItem},
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
    collections::{HashSet, VecDeque},
    fs,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc, Once,
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
            "icons/alula-mark.svg" => Some(include_bytes!("../assets/icons/alula-mark.svg")),
            "icons/arrow-left.svg" => Some(include_bytes!("../assets/icons/arrow-left.svg")),
            "icons/arrow-right.svg" => Some(include_bytes!("../assets/icons/arrow-right.svg")),
            "icons/bot.svg" => Some(include_bytes!("../assets/icons/bot.svg")),
            "icons/check.svg" => Some(include_bytes!("../assets/icons/check.svg")),
            "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
            "icons/circle-check.svg" => Some(include_bytes!("../assets/icons/circle-check.svg")),
            "icons/circle-x.svg" => Some(include_bytes!("../assets/icons/circle-x.svg")),
            "icons/close.svg" => Some(include_bytes!("../assets/icons/close.svg")),
            "icons/copy.svg" => Some(include_bytes!("../assets/icons/copy.svg")),
            "icons/delete.svg" => Some(include_bytes!("../assets/icons/delete.svg")),
            "icons/ellipsis.svg" => Some(include_bytes!("../assets/icons/ellipsis.svg")),
            "icons/folder.svg" => Some(include_bytes!("../assets/icons/folder.svg")),
            "icons/folder-open.svg" => Some(include_bytes!("../assets/icons/folder-open.svg")),
            "icons/globe.svg" => Some(include_bytes!("../assets/icons/globe.svg")),
            "icons/info.svg" => Some(include_bytes!("../assets/icons/info.svg")),
            "icons/loader-circle.svg" => Some(include_bytes!("../assets/icons/loader-circle.svg")),
            "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
            "icons/palette.svg" => Some(include_bytes!("../assets/icons/palette.svg")),
            "icons/redo-2.svg" => Some(include_bytes!("../assets/icons/redo-2.svg")),
            "icons/replace.svg" => Some(include_bytes!("../assets/icons/replace.svg")),
            "icons/search.svg" => Some(include_bytes!("../assets/icons/search.svg")),
            "icons/settings.svg" => Some(include_bytes!("../assets/icons/settings.svg")),
            "icons/settings-2.svg" => Some(include_bytes!("../assets/icons/settings-2.svg")),
            "icons/square-terminal.svg" => {
                Some(include_bytes!("../assets/icons/square-terminal.svg"))
            }
            "icons/triangle-alert.svg" => {
                Some(include_bytes!("../assets/icons/triangle-alert.svg"))
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
            "alula-mark.svg",
            "arrow-left.svg",
            "arrow-right.svg",
            "bot.svg",
            "check.svg",
            "chevron-down.svg",
            "circle-check.svg",
            "circle-x.svg",
            "close.svg",
            "copy.svg",
            "delete.svg",
            "ellipsis.svg",
            "folder.svg",
            "folder-open.svg",
            "globe.svg",
            "info.svg",
            "loader-circle.svg",
            "plus.svg",
            "palette.svg",
            "redo-2.svg",
            "replace.svg",
            "search.svg",
            "settings.svg",
            "settings-2.svg",
            "square-terminal.svg",
            "triangle-alert.svg",
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

struct ResponseSwitchSegment {
    id: &'static str,
    label: &'static str,
    width: Pixels,
    mode: ResponseViewMode,
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

fn active_tab_index_after_close(active: usize, closing: usize, tab_count: usize) -> usize {
    debug_assert!(tab_count > 1);
    debug_assert!(active < tab_count);
    debug_assert!(closing < tab_count);

    if closing < active {
        active - 1
    } else if closing == active {
        active.min(tab_count - 2)
    } else {
        active
    }
}

#[cfg(test)]
mod command_palette_tests {
    use super::{
        Assets, HttpStreamEvent, PaletteCommand, RequestDestination, RequestStreamEvent, Rope,
        active_tab_index_after_close, ascii_contains_ignore_case, formatting_stream_chunk,
        next_palette_index, open_variable_at_cursor, open_variable_at_rope_cursor,
        previous_palette_index, push_stream_event_batch, redact_secret_values, split_history_error,
        template_visual_state, variable_completion_context_at_cursor,
        variable_completion_insert_text,
    };

    #[cfg(target_os = "macos")]
    use super::{QuitApplication, macos_app_menu};

    #[test]
    fn semantic_notification_icons_are_bundled() {
        for path in [
            "icons/circle-check.svg",
            "icons/circle-x.svg",
            "icons/info.svg",
            "icons/triangle-alert.svg",
        ] {
            assert!(
                gpui::AssetSource::load(&Assets, path).unwrap().is_some(),
                "missing {path}"
            );
        }
    }

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
    fn large_list_filtering_matches_ascii_without_allocating_lowercase_rows() {
        assert!(ascii_contains_ignore_case("Production_API_URL", "api"));
        assert!(ascii_contains_ignore_case("POST", "post"));
        assert!(ascii_contains_ignore_case("anything", ""));
        assert!(!ascii_contains_ignore_case("staging", "prod"));
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
    fn destination_filter_matches_environment_nested_path_and_unfiled_alias() {
        let nested = RequestDestination {
            environment_id: "env-1".into(),
            environment_name: "Production API".into(),
            folder_id: Some("folder-1".into()),
            folder_path: Some("Backend / Authentication".into()),
        };
        assert!(nested.matches("production auth"));
        assert!(nested.matches("backend"));
        assert!(!nested.matches("staging"));

        let root = RequestDestination {
            environment_id: "env-2".into(),
            environment_name: "Staging".into(),
            folder_id: None,
            folder_path: None,
        };
        assert!(root.matches("staging unfiled"));
        assert!(root.matches("root"));
    }

    #[test]
    fn closing_tabs_preserves_the_active_request_when_possible() {
        assert_eq!(active_tab_index_after_close(2, 0, 4), 1);
        assert_eq!(active_tab_index_after_close(2, 3, 4), 2);
        assert_eq!(active_tab_index_after_close(2, 2, 4), 2);
        assert_eq!(active_tab_index_after_close(3, 3, 4), 2);
        assert_eq!(active_tab_index_after_close(0, 0, 4), 0);
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
    fn variable_completion_preserves_an_existing_closing_delimiter() {
        let context = variable_completion_context_at_cursor("{{ba}}", 4).unwrap();
        assert_eq!(context.start, 0);
        assert_eq!(context.partial, "ba");
        assert!(context.has_closing_delimiter);

        let context = variable_completion_context_at_cursor("{{ba", 4).unwrap();
        assert!(!context.has_closing_delimiter);

        assert_eq!(variable_completion_insert_text("base", true), "{{base");
        assert_eq!(variable_completion_insert_text("base", false), "{{base}}");
    }

    #[test]
    fn template_visual_state_tracks_only_variable_spans() {
        let mut environment = alula::Environment::new("test");
        environment
            .variables
            .push(alula::EnvironmentVariable::public("base", "example.com"));
        let source = "https://{{base}}/users";
        let state = template_visual_state(source, Some(&environment));
        assert_eq!(state.references, vec![8..16]);
        assert!(state.error.is_none());
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

    #[test]
    fn history_http_errors_are_split_into_status_and_detail() {
        assert_eq!(
            split_history_error("WebSocket handshake failed: HTTP error: 401 Unauthorized",),
            (
                "401 Unauthorized".into(),
                Some("WebSocket handshake failed".into()),
            )
        );
        assert_eq!(
            split_history_error("connection refused"),
            ("connection refused".into(), None),
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_application_menu_exposes_the_quit_action() {
        let menus = macos_app_menu();
        assert_eq!(menus.len(), 1);
        assert_eq!(menus[0].name.as_ref(), "Alula");

        let gpui::MenuItem::Action { name, action, .. } = menus[0].items.last().unwrap() else {
            panic!("expected Quit Alula to be the final application menu item");
        };
        assert_eq!(name.as_ref(), "Quit Alula");
        assert!(action.as_any().is::<QuitApplication>());
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
    cx.bind_keys(application_key_bindings(config));
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
                .default_value(key_value)
                .text_highlights(key_template_state.borrow().text_highlights(cx));
            state.lsp.completion_provider =
                Some(Rc::new(VariableCompletionProvider::new(key_names)));
            state
        });
        let value_names = variable_names.clone();
        let value = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder("Value")
                .default_value(value_value)
                .text_highlights(value_template_state.borrow().text_highlights(cx));
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
                let next_visual_state = template_visual_state(value.as_ref(), environment);
                let highlights = next_visual_state.text_highlights(cx);
                input.update(cx, |input, cx| {
                    input.set_text_highlights(highlights, cx);
                });
                *visual_state.borrow_mut() = next_visual_state;
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

    fn refresh_template_state(&self, environment: Option<&alula::Environment>, cx: &mut App) {
        let key_state = template_visual_state(self.key.read(cx).value().as_ref(), environment);
        let key_highlights = key_state.text_highlights(cx);
        self.key.update(cx, |input, cx| {
            input.set_text_highlights(key_highlights, cx);
        });
        *self.key_template_state.borrow_mut() = key_state;

        let value_state = template_visual_state(self.value.read(cx).value().as_ref(), environment);
        let value_highlights = value_state.text_highlights(cx);
        self.value.update(cx, |input, cx| {
            input.set_text_highlights(value_highlights, cx);
        });
        *self.value_template_state.borrow_mut() = value_state;
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
    active: Cell<bool>,
}

impl VariableCompletionProvider {
    fn new(variable_names: Rc<RefCell<Vec<(String, bool)>>>) -> Self {
        Self {
            variable_names,
            active: Cell::new(false),
        }
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
        const MAX_VISIBLE_COMPLETIONS: usize = 100;

        let Some(context) = variable_completion_context_at_rope_cursor(text, offset) else {
            self.active.set(false);
            return Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        };
        self.active.set(true);
        let range = LspRange {
            start: text.offset_to_position(context.start),
            end: text.offset_to_position(offset),
        };
        let variable_names = self.variable_names.borrow();
        let first_match =
            variable_names.partition_point(|(name, _)| name.as_str() < context.partial.as_str());
        let items = variable_names[first_match..]
            .iter()
            .take_while(|(name, _)| name.starts_with(&context.partial))
            .take(MAX_VISIBLE_COMPLETIONS)
            .map(|(name, secret)| {
                let syntax = format!("{{{{{name}}}}}");
                let new_text = variable_completion_insert_text(name, context.has_closing_delimiter);
                CompletionItem {
                    label: syntax.clone(),
                    detail: Some(if *secret {
                        "Secret environment variable".into()
                    } else {
                        "Environment variable".into()
                    }),
                    kind: Some(CompletionItemKind::VARIABLE),
                    text_edit: Some(CompletionTextEdit::Edit(TextEdit { range, new_text })),
                    ..Default::default()
                }
            })
            .collect();
        Task::ready(Ok(CompletionResponse::Array(items)))
    }

    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut Context<InputState>) -> bool {
        new_text.contains('{')
            || (self.active.get()
                && (new_text.is_empty()
                    || new_text.chars().all(|character| {
                        character == '{'
                            || character == '}'
                            || character == '_'
                            || character == '-'
                            || character == '.'
                            || character.is_ascii_alphanumeric()
                    })))
    }
}

fn variable_completion_insert_text(name: &str, has_closing_delimiter: bool) -> String {
    if has_closing_delimiter {
        format!("{{{{{name}")
    } else {
        format!("{{{{{name}}}}}")
    }
}

#[derive(Debug, PartialEq, Eq)]
struct VariableCompletionContext {
    start: usize,
    partial: String,
    has_closing_delimiter: bool,
}

fn variable_completion_context_at_cursor(
    source: &str,
    offset: usize,
) -> Option<VariableCompletionContext> {
    let before = source.get(..offset)?;
    let start = before.rfind("{{")?;
    if before[start + 2..].contains("}}") {
        return None;
    }
    let partial = &before[start + 2..];
    let valid_partial = partial.is_empty()
        || partial.chars().enumerate().all(|(index, character)| {
            if index == 0 {
                character == '_' || character.is_ascii_alphabetic()
            } else {
                character == '_'
                    || character == '-'
                    || character == '.'
                    || character.is_ascii_alphanumeric()
            }
        });
    valid_partial.then(|| VariableCompletionContext {
        start,
        partial: partial.to_owned(),
        has_closing_delimiter: source[offset..].starts_with("}}"),
    })
}

#[cfg(test)]
fn open_variable_at_cursor(source: &str, offset: usize) -> Option<usize> {
    variable_completion_context_at_cursor(source, offset).map(|context| context.start)
}

fn variable_completion_context_at_rope_cursor(
    text: &Rope,
    offset: usize,
) -> Option<VariableCompletionContext> {
    const MAX_COMPLETION_PREFIX_CHARS: usize = 128;

    let reversed = text
        .chars_at(offset)
        .reversed()
        .take(MAX_COMPLETION_PREFIX_CHARS)
        .collect::<String>();
    let suffix = reversed.chars().rev().collect::<String>();
    let mut context = variable_completion_context_at_cursor(&suffix, suffix.len())?;
    context.start = offset - (suffix.len() - context.start);
    context.has_closing_delimiter =
        text.char_at(offset) == Some('}') && text.char_at(offset + 1) == Some('}');
    Some(context)
}

#[cfg(test)]
fn open_variable_at_rope_cursor(text: &Rope, offset: usize) -> Option<usize> {
    variable_completion_context_at_rope_cursor(text, offset).map(|context| context.start)
}

#[derive(Clone, Default)]
struct TemplateVisualState {
    references: Vec<std::ops::Range<usize>>,
    error: Option<String>,
}

impl TemplateVisualState {
    fn is_valid(&self) -> bool {
        !self.references.is_empty() && self.error.is_none()
    }

    fn text_highlights(&self, cx: &App) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
        let style = HighlightStyle {
            color: Some(cx.theme().primary),
            ..Default::default()
        };
        self.references
            .iter()
            .cloned()
            .map(|range| (range, style))
            .collect()
    }
}

fn template_visual_state(
    source: &str,
    environment: Option<&alula::Environment>,
) -> TemplateVisualState {
    let inspection = inspect_template(source, environment);
    TemplateVisualState {
        references: inspection.references,
        error: inspection.errors.first().map(ToString::to_string),
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
        register_response_languages();
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
        register_response_languages();
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
                .default_value(url_value)
                .text_highlights(url_template_state.text_highlights(cx));
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
            state = state
                .soft_wrap(false)
                .default_value(body_value)
                .text_highlights(body_template_state.text_highlights(cx));
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
            let highlights = state.text_highlights(cx);
            input.update(cx, |input, cx| {
                input.set_text_highlights(highlights, cx);
            });
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
            let highlights = state.text_highlights(cx);
            input.update(cx, |input, cx| {
                input.set_text_highlights(highlights, cx);
            });
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
        let mut names = variables
            .iter()
            .map(|variable| (variable.name.clone(), variable.secret))
            .collect::<Vec<_>>();
        names.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        *self.variable_names.borrow_mut() = names;
    }

    fn refresh_environment(&mut self, environment: Option<&alula::Environment>, cx: &mut App) {
        self.set_environment_variables(
            environment
                .map(|environment| environment.variables.as_slice())
                .unwrap_or_default(),
        );
        self.url_template_state =
            template_visual_state(self.url.read(cx).value().as_ref(), environment);
        let url_highlights = self.url_template_state.text_highlights(cx);
        self.url.update(cx, |input, cx| {
            input.set_text_highlights(url_highlights, cx);
        });
        self.body_template_state =
            template_visual_state(self.body.read(cx).value().as_ref(), environment);
        let body_highlights = self.body_template_state.text_highlights(cx);
        self.body.update(cx, |input, cx| {
            input.set_text_highlights(body_highlights, cx);
        });
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

type SidebarNavClick = Box<dyn Fn(&mut Window, &mut App)>;

struct AlulaApp {
    focus_handle: FocusHandle,
    new_request_focus: FocusHandle,
    sidebar_collapsed: bool,
    sidebar_hovered: Option<WorkspaceSection>,
    sidebar_pressed: Option<WorkspaceSection>,
    new_request_hovered: bool,
    new_request_pressed: bool,
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
    history_loaded: bool,
    state_paths: StatePaths,
    persistence_dirty: Arc<AtomicBool>,
    persistence_pending_while_history_loads: bool,
    environment_name: Entity<InputState>,
    environment_folder_name: Entity<InputState>,
    environment_search: Entity<InputState>,
    history_search: Entity<InputState>,
    environment_request_search: Entity<InputState>,
    environment_variable_search: Entity<InputState>,
    selected_environment_id: Option<String>,
    environment_detail_tab: EnvironmentDetailTab,
    expanded_environment_folder_ids: HashSet<String>,
    hovered_environment_id: Option<String>,
    hovered_environment_request_id: Option<String>,
    hovered_environment_variable_id: Option<String>,
    hovered_history_id: Option<String>,
    revealed_secret_variable_id: Option<String>,
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

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestDestination {
    environment_id: String,
    environment_name: String,
    folder_id: Option<String>,
    folder_path: Option<String>,
}

impl RequestDestination {
    fn matches(&self, query: &str) -> bool {
        query.split_whitespace().all(|term| {
            ascii_contains_ignore_case(&self.environment_name, term)
                || self
                    .folder_path
                    .as_deref()
                    .is_some_and(|path| ascii_contains_ignore_case(path, term))
                || (self.folder_path.is_none()
                    && ascii_contains_ignore_case("unfiled environment root", term))
        })
    }

    fn path_label(&self) -> &str {
        self.folder_path.as_deref().unwrap_or("Unfiled")
    }
}

struct RequestDestinationPickerView {
    app: Entity<AlulaApp>,
    request_id: String,
    destinations: Arc<Vec<RequestDestination>>,
    filtered: Vec<usize>,
    input: Entity<InputState>,
    selected: usize,
    current_environment_id: Option<String>,
    current_folder_id: Option<String>,
    scroll: UniformListScrollHandle,
}

struct McpUiCall {
    name: String,
    arguments: Value,
    reply: mpsc::SyncSender<Value>,
}

#[derive(Clone)]
enum McpStatus {
    Ready { port: u16 },
    Stopped,
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
                |this, delta| this.opacity(delta),
            )
    }
}

impl RequestDestinationPickerView {
    fn new(
        app: Entity<AlulaApp>,
        request_id: String,
        destinations: Arc<Vec<RequestDestination>>,
        current_environment_id: Option<String>,
        current_folder_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx).placeholder("Search environments and folder paths…")
        });
        cx.subscribe_in(
            &input,
            window,
            |this, input, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let query = input.read(cx).value();
                    this.filtered = this
                        .destinations
                        .iter()
                        .enumerate()
                        .filter_map(|(index, destination)| {
                            destination.matches(query.as_ref()).then_some(index)
                        })
                        .collect();
                    this.selected = 0;
                    this.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.confirm(window, cx),
                _ => {}
            },
        )
        .detach();
        let filtered = (0..destinations.len()).collect();
        Self {
            app,
            request_id,
            destinations,
            filtered,
            input,
            selected: 0,
            current_environment_id,
            current_folder_id,
            scroll: UniformListScrollHandle::new(),
        }
    }

    fn is_current(&self, destination: &RequestDestination) -> bool {
        self.current_environment_id.as_deref() == Some(&destination.environment_id)
            && self.current_folder_id.as_deref() == destination.folder_id.as_deref()
    }

    fn select_previous(&mut self, cx: &mut Context<Self>) {
        if let Some(selected) = previous_palette_index(self.selected, self.filtered.len()) {
            self.selected = selected;
            self.scroll.scroll_to_item(selected, ScrollStrategy::Center);
            cx.notify();
        }
    }

    fn select_next(&mut self, cx: &mut Context<Self>) {
        if let Some(selected) = next_palette_index(self.selected, self.filtered.len()) {
            self.selected = selected;
            self.scroll.scroll_to_item(selected, ScrollStrategy::Center);
            cx.notify();
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(&destination_index) = self.filtered.get(self.selected) else {
            return;
        };
        self.move_to(destination_index, window, cx);
    }

    fn move_to(&mut self, destination_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(destination) = self.destinations.get(destination_index).cloned() else {
            return;
        };
        self.app.update(cx, |this, cx| {
            this.assign_request_to_environment_folder(
                &self.request_id,
                &destination.environment_id,
                destination.folder_id.as_deref(),
                cx,
            )
        });
        window.close_dialog(cx);
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

    fn destination_row(
        &self,
        filtered_index: usize,
        picker: Entity<Self>,
        cx: &mut App,
    ) -> Option<AnyElement> {
        let destination_index = *self.filtered.get(filtered_index)?;
        let destination = self.destinations.get(destination_index)?;
        let selected = filtered_index == self.selected;
        let current = self.is_current(destination);
        let hover_picker = picker.clone();
        let click_picker = picker.clone();
        Some(
            div()
                .id(("request-destination", destination_index))
                .h(px(48.))
                .w_full()
                .px_3()
                .flex()
                .items_center()
                .gap_3()
                .rounded(px(7.))
                .cursor_pointer()
                .text_color(if selected {
                    cx.theme().foreground
                } else {
                    cx.theme().muted_foreground
                })
                .when(selected, |this| this.bg(cx.theme().accent))
                .when(!selected, |this| {
                    this.hover(|this| this.bg(cx.theme().secondary.lighten(0.1)))
                })
                .child(
                    div()
                        .w(px(20.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(if destination.folder_id.is_some() {
                                IconName::FolderOpen
                            } else {
                                IconName::Globe
                            })
                            .size(px(14.))
                            .text_color(if selected {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground.opacity(0.72)
                            }),
                        ),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(2.))
                        .child(
                            div()
                                .truncate()
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(cx.theme().foreground)
                                .child(destination.environment_name.clone()),
                        )
                        .child(
                            div()
                                .truncate()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(10.))
                                .text_color(cx.theme().muted_foreground.opacity(0.78))
                                .child(destination.path_label().to_owned()),
                        ),
                )
                .when(current, |this| {
                    this.child(
                        div()
                            .flex_shrink_0()
                            .px_2()
                            .py(px(2.))
                            .rounded_full()
                            .bg(cx.theme().primary.opacity(0.12))
                            .text_size(px(9.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(cx.theme().primary)
                            .child("Current"),
                    )
                })
                .on_hover(move |hovered, _, cx| {
                    if *hovered {
                        hover_picker.update(cx, |this, cx| {
                            if this.selected != filtered_index {
                                this.selected = filtered_index;
                                cx.notify();
                            }
                        });
                    }
                })
                .on_click(move |_, window, cx| {
                    click_picker.update(cx, |this, cx| this.move_to(destination_index, window, cx));
                })
                .into_any_element(),
        )
    }
}

impl Render for RequestDestinationPickerView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.selected = self.selected.min(self.filtered.len().saturating_sub(1));
        let picker = cx.entity();
        let result_count = self.filtered.len();
        let results = if result_count == 0 {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .items_center()
                .justify_center()
                .text_size(px(11.))
                .text_color(cx.theme().muted_foreground)
                .child("No matching environments or folders")
                .into_any_element()
        } else {
            uniform_list(
                "request-destination-results",
                result_count,
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .filter_map(|index| this.destination_row(index, picker.clone(), cx))
                        .collect::<Vec<_>>()
                }),
            )
            .size_full()
            .track_scroll(self.scroll.clone())
            .into_any_element()
        };

        div()
            .id("request-destination-picker")
            .h(px(460.))
            .min_h_0()
            .flex()
            .flex_col()
            .gap_2()
            .on_key_down(cx.listener(Self::handle_key_down))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(cx.theme().muted_foreground)
                    .child("Search by environment name or any part of a nested folder path."),
            )
            .child(
                div().h(px(32.)).flex_shrink_0().child(
                    Input::new(&self.input)
                        .prefix(IconName::Search)
                        .text_size(px(11.))
                        .rounded(px(6.))
                        .h_full()
                        .w_full(),
                ),
            )
            .child(div().flex_1().min_h_0().flex().flex_col().child(results))
            .child(
                div()
                    .pt_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .flex()
                    .items_center()
                    .gap_3()
                    .text_size(px(9.))
                    .text_color(cx.theme().muted_foreground.opacity(0.72))
                    .child("↑↓ Navigate")
                    .child("↵ Move")
                    .child("Esc Close"),
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
        let PersistedState {
            workspace,
            history,
            environments,
        } = persisted;
        let workspace = if theme_config.application.restore_open_requests {
            workspace
        } else {
            Workspace::default()
        };
        let active_tab = workspace
            .requests
            .iter()
            .position(|request| request.id == workspace.active_request_id)
            .unwrap_or(0);
        let mut tabs: Vec<_> = workspace
            .requests
            .into_iter()
            .map(|request| RequestTab::new(request, window, cx))
            .collect();
        for tab in &mut tabs {
            let environment = environments.environment_for_request(&tab.draft.id);
            tab.refresh_environment(environment, cx);
        }
        let focus_handle = cx.focus_handle();
        let new_request_focus = cx.focus_handle();
        focus_handle.focus(window);
        let (mcp_ui_tx, mcp_ui_rx) = smol::channel::unbounded();
        let (mcp_http, mcp_status) = if theme_config.agent.start_with_app {
            Self::launch_mcp_http(theme_config.agent.port, &theme_path, mcp_ui_tx.clone())
        } else {
            (None, McpStatus::Stopped)
        };
        let environment_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search environments…"));
        let history_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter history…"));
        let environment_request_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search requests…"));
        let environment_variable_search =
            cx.new(|cx| InputState::new(window, cx).placeholder("Search variables…"));
        for input in [
            &environment_search,
            &history_search,
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
            sidebar_pressed: None,
            new_request_hovered: false,
            new_request_pressed: false,
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
            environments,
            history,
            history_loaded: false,
            persistence_dirty: Arc::new(AtomicBool::new(false)),
            persistence_pending_while_history_loads: false,
            environment_name: cx
                .new(|cx| InputState::new(window, cx).placeholder("Production, Staging, Local…")),
            environment_folder_name: cx
                .new(|cx| InputState::new(window, cx).placeholder("Authentication, Billing…")),
            environment_search,
            history_search,
            environment_request_search,
            environment_variable_search,
            selected_environment_id: None,
            environment_detail_tab: EnvironmentDetailTab::Requests,
            expanded_environment_folder_ids: HashSet::new(),
            hovered_environment_id: None,
            hovered_environment_request_id: None,
            hovered_environment_variable_id: None,
            hovered_history_id: None,
            revealed_secret_variable_id: None,
            mcp_http,
            mcp_status,
            mcp_ui_tx,
            http_session: HttpSession::new(),
        };
        Self::watch_theme_file(app.theme_path.clone(), app.theme_modified, cx);
        Self::watch_persistence(app.persistence_dirty.clone(), cx);
        Self::watch_mcp_calls(mcp_ui_rx, window, cx);
        app.hydrate_secrets_in_background(cx);
        Self::finish_startup_after_first_frame(window, cx);
        app
    }

    fn finish_startup_after_first_frame(window: &mut Window, cx: &mut Context<Self>) {
        // History and tree-sitter's global language registry are not needed to
        // paint the request editor. Start both only after the first frame has
        // reached the screen, keeping cold files and language setup off the
        // startup-critical path.
        cx.on_next_frame(window, |this, _, cx| {
            this.load_history_in_background(cx);
            cx.background_executor()
                .spawn(async { register_response_languages() })
                .detach();
        });
    }

    fn load_history_in_background(&self, cx: &mut Context<Self>) {
        let paths = self.state_paths.clone();
        let load = cx
            .background_executor()
            .spawn(async move { PersistedState::load_history(&paths) });
        cx.spawn(async move |this, cx| {
            let history = load.await;
            let _ = this.update(cx, |this, cx| {
                match history {
                    Ok(history) => this.history.merge_older(history),
                    Err(error) => eprintln!("could not load request history: {error:#}"),
                }
                this.history_loaded = true;
                if this.persistence_pending_while_history_loads {
                    this.persistence_pending_while_history_loads = false;
                    this.persistence_dirty.store(true, Ordering::Release);
                }
                if this.workspace_section == WorkspaceSection::History {
                    cx.notify();
                }
            });
        })
        .detach();
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
        if !self.theme_config.agent.start_with_app {
            self.mcp_status = McpStatus::Stopped;
            return;
        }
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
                            .request_ids()
                            .map(str::to_owned)
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
            "create_environment_folder" => {
                let Some(environment_id) = arguments.get("environment_id").and_then(Value::as_str)
                else {
                    return reply_to_tool(AgentReply::error("environment_id must be a string"));
                };
                let Some(name) = arguments.get("name").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("name must be a string"));
                };
                if arguments
                    .get("parent_folder_id")
                    .is_some_and(|value| !value.is_string() && !value.is_null())
                {
                    return reply_to_tool(AgentReply::error("parent_folder_id must be a string"));
                }
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::CreateFolder {
                        environment_id: environment_id.to_owned(),
                        parent_folder_id: arguments
                            .get("parent_folder_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        name: name.to_owned(),
                    },
                );
                if reply.ok {
                    self.persistence_dirty.store(true, Ordering::Release);
                    cx.notify();
                }
                reply
            }
            "rename_environment_folder" => {
                let Some(environment_id) = arguments.get("environment_id").and_then(Value::as_str)
                else {
                    return reply_to_tool(AgentReply::error("environment_id must be a string"));
                };
                let Some(folder_id) = arguments.get("folder_id").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("folder_id must be a string"));
                };
                let Some(name) = arguments.get("name").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("name must be a string"));
                };
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::RenameFolder {
                        environment_id: environment_id.to_owned(),
                        folder_id: folder_id.to_owned(),
                        name: name.to_owned(),
                    },
                );
                if reply.ok {
                    self.persistence_dirty.store(true, Ordering::Release);
                    cx.notify();
                }
                reply
            }
            "delete_environment_folder" => {
                let Some(environment_id) = arguments.get("environment_id").and_then(Value::as_str)
                else {
                    return reply_to_tool(AgentReply::error("environment_id must be a string"));
                };
                let Some(folder_id) = arguments.get("folder_id").and_then(Value::as_str) else {
                    return reply_to_tool(AgentReply::error("folder_id must be a string"));
                };
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::DeleteFolder {
                        environment_id: environment_id.to_owned(),
                        folder_id: folder_id.to_owned(),
                    },
                );
                if reply.ok {
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
                                .request_ids()
                                .map(str::to_owned)
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
                if arguments
                    .get("folder_id")
                    .is_some_and(|value| !value.is_string() && !value.is_null())
                {
                    return reply_to_tool(AgentReply::error("folder_id must be a string"));
                }
                let workspace = self.workspace_snapshot(cx);
                let reply = apply_environment_agent_command(
                    &workspace,
                    &mut self.environments,
                    EnvironmentAgentCommand::AssignRequest {
                        environment_id: environment_id.to_owned(),
                        folder_id: arguments
                            .get("folder_id")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
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
                    if !this.history_loaded {
                        this.persistence_pending_while_history_loads = true;
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
        window.open_dialog(cx, move |dialog, _, _cx| {
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
                .close_button(true)
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Save settings")
                        .cancel_text("Cancel")
                        .cancel_variant(ButtonVariant::Secondary),
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

    fn add_environment_request(
        &mut self,
        environment_id: &str,
        folder_id: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.add_request(&ClickEvent::default(), window, cx);
        let request_id = self.tabs[self.active_tab].draft.id.clone();
        self.assign_request_to_environment_folder(&request_id, environment_id, folder_id, cx);
    }

    fn close_request(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.len() == 1 || index >= self.tabs.len() {
            return;
        }
        let next_active = active_tab_index_after_close(self.active_tab, index, self.tabs.len());
        if let Some(cancellation) = self.tabs[index].cancellation.take() {
            cancellation.store(true, Ordering::Release);
        }
        self.tabs.remove(index);
        self.active_tab = next_active;
        self.request_tabs_scroll.scroll_to_item(self.active_tab);
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
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title(Label::new("New environment").font_weight(FontWeight::SEMIBOLD))
                .w(px(440.))
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
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
                        .cancel_text("Cancel")
                        .cancel_variant(ButtonVariant::Secondary),
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

    fn open_import_environment_dialog(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let share_string = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .soft_wrap(true)
                .placeholder("alula-env-v1:…")
        });
        let save_share_string = share_string.clone();
        let dialog_share_string = share_string.clone();
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title(Label::new("Import environment").font_weight(FontWeight::SEMIBOLD))
                .w(px(560.))
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            Label::new(
                                "Paste an Alula environment share string. Requests, folders, and public variables will be imported; secret values must be entered again.",
                            )
                            .text_sm()
                            .text_color(cx.theme().muted_foreground),
                        )
                        .child(
                            div()
                                .h(px(150.))
                                .child(Input::new(&dialog_share_string).size_full()),
                        ),
                )
                .confirm()
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("Import environment")
                        .cancel_text("Cancel")
                        .cancel_variant(ButtonVariant::Secondary),
                )
                .on_ok({
                    let app = app.clone();
                    let save_share_string = save_share_string.clone();
                    move |_, window, cx| {
                        let source = save_share_string.read(cx).value().to_string();
                        match alula::Environment::from_share_string(&source) {
                            Ok(environment) => {
                                let name = environment.name.clone();
                                let environment_id = environment.id.clone();
                                app.update(cx, |this, cx| {
                                    this.environments.environments.push(environment);
                                    this.selected_environment_id = Some(environment_id);
                                    this.persistence_dirty.store(true, Ordering::Release);
                                    cx.notify();
                                });
                                window.push_notification(
                                    Notification::success(format!(
                                        "Imported environment “{name}”"
                                    )),
                                    cx,
                                );
                                true
                            }
                            Err(error) => {
                                window.push_notification(
                                    Notification::error(format!(
                                        "Could not import environment: {error:#}"
                                    )),
                                    cx,
                                );
                                false
                            }
                        }
                    }
                })
        });
        share_string.update(cx, |input, cx| input.focus(window, cx));
    }

    fn export_environment(
        &mut self,
        environment_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace_snapshot(cx);
        self.environments.sync_open_requests(&workspace.requests);
        let Some(environment) = self
            .environments
            .environments
            .iter()
            .find(|environment| environment.id == environment_id)
        else {
            window.push_notification(Notification::error("Environment not found"), cx);
            return;
        };
        match environment.to_share_string() {
            Ok(share_string) => {
                cx.write_to_clipboard(ClipboardItem::new_string(share_string));
                window.push_notification(
                    Notification::success(format!(
                        "Copied “{}” share string to the clipboard",
                        environment.name
                    )),
                    cx,
                );
            }
            Err(error) => window.push_notification(
                Notification::error(format!("Could not export environment: {error:#}")),
                cx,
            ),
        }
    }

    fn open_environment_folder_dialog(
        &mut self,
        environment_id: String,
        parent_folder_id: Option<String>,
        folder_id: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing_name = folder_id
            .as_deref()
            .and_then(|folder_id| {
                self.environments
                    .environments
                    .iter()
                    .find(|environment| environment.id == environment_id)
                    .and_then(|environment| environment.find_folder(folder_id))
            })
            .map(|folder| folder.name.clone())
            .unwrap_or_default();
        self.environment_folder_name.update(cx, |input, cx| {
            input.set_value(existing_name, window, cx);
        });
        let editing = folder_id.is_some();
        let input = self.environment_folder_name.clone();
        let save_input = input.clone();
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title(
                    Label::new(if editing {
                        "Rename folder"
                    } else {
                        "New folder"
                    })
                    .font_weight(FontWeight::SEMIBOLD),
                )
                .w(px(440.))
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
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
                        .ok_text(if editing {
                            "Rename folder"
                        } else {
                            "Create folder"
                        })
                        .cancel_text("Cancel")
                        .cancel_variant(ButtonVariant::Secondary),
                )
                .on_ok({
                    let app = app.clone();
                    let environment_id = environment_id.clone();
                    let parent_folder_id = parent_folder_id.clone();
                    let folder_id = folder_id.clone();
                    let save_input = save_input.clone();
                    move |_, window, cx| {
                        let name = save_input.read(cx).value().trim().to_owned();
                        if name.is_empty() {
                            return false;
                        }
                        let reply = app.update(cx, |this, cx| {
                            let workspace = this.workspace_snapshot(cx);
                            let command = if let Some(folder_id) = folder_id.clone() {
                                EnvironmentAgentCommand::RenameFolder {
                                    environment_id: environment_id.clone(),
                                    folder_id,
                                    name,
                                }
                            } else {
                                EnvironmentAgentCommand::CreateFolder {
                                    environment_id: environment_id.clone(),
                                    parent_folder_id: parent_folder_id.clone(),
                                    name,
                                }
                            };
                            let reply = apply_environment_agent_command(
                                &workspace,
                                &mut this.environments,
                                command,
                            );
                            if reply.ok {
                                if let Some(parent_folder_id) = &parent_folder_id {
                                    this.expanded_environment_folder_ids
                                        .insert(parent_folder_id.clone());
                                }
                                this.persistence_dirty.store(true, Ordering::Release);
                                cx.notify();
                            }
                            reply
                        });
                        if !reply.ok {
                            window.push_notification(Notification::error(reply.message), cx);
                        }
                        reply.ok
                    }
                })
        });
    }

    fn delete_environment_folder(
        &mut self,
        environment_id: &str,
        folder_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let workspace = self.workspace_snapshot(cx);
        let reply = apply_environment_agent_command(
            &workspace,
            &mut self.environments,
            EnvironmentAgentCommand::DeleteFolder {
                environment_id: environment_id.to_owned(),
                folder_id: folder_id.to_owned(),
            },
        );
        if reply.ok {
            self.expanded_environment_folder_ids.remove(folder_id);
            self.persistence_dirty.store(true, Ordering::Release);
            cx.notify();
        } else {
            window.push_notification(Notification::error(reply.message), cx);
        }
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
                    let request_count = environment.request_count();
                    choices = choices.child(
                        Button::new(SharedString::from(format!(
                            "palette-delete-environment-{environment_id}"
                        )))
                        .ghost()
                        .w_full()
                        .h(px(42.))
                        .px_3()
                        .rounded(cx.theme().radius)
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(div().size(px(7.)).rounded_full().bg(cx.theme().primary))
                                .child(
                                    Label::new(environment.name.clone())
                                        .font_weight(FontWeight::MEDIUM),
                                )
                                .child(
                                    div()
                                        .ml_auto()
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_size(px(10.))
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!(
                                            "{} request{}",
                                            request_count,
                                            if request_count == 1 { "" } else { "s" }
                                        )),
                                ),
                        )
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
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
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
        if !self.theme_config.application.confirm_destructive_actions {
            self.delete_environment_now(&environment_id, &environment_name, window, cx);
            return;
        }
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let delete_app = app.clone();
            let delete_id = environment_id.clone();
            let deleted_name = environment_name.clone();
            dialog
                .title(Label::new("Delete environment?").font_weight(FontWeight::SEMIBOLD))
                .w(px(480.))
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
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
                        .cancel_text("Cancel")
                        .cancel_variant(ButtonVariant::Secondary),
                )
                .on_ok(move |_, window, cx| {
                    delete_app.update(cx, |this, cx| {
                        this.delete_environment_now(&delete_id, &deleted_name, window, cx)
                    })
                })
        });
    }

    fn delete_environment_now(
        &mut self,
        environment_id: &str,
        environment_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
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
        if !reply.ok {
            window.push_notification(Notification::error(reply.message), cx);
            return false;
        }
        if self.selected_environment_id.as_deref() == Some(environment_id) {
            self.selected_environment_id = None;
        }
        for request_id in affected_request_ids {
            self.refresh_request_variable_names(&request_id, cx);
        }
        self.persistence_dirty.store(true, Ordering::Release);
        window.push_notification(
            Notification::success(format!("Deleted environment “{environment_name}”")),
            cx,
        );
        cx.notify();
        true
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
                        .h(px(42.))
                        .px_3()
                        .rounded(cx.theme().radius)
                        .child(
                            div()
                                .w_full()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(method_badge(entry.request.method, cx))
                                .child(
                                    Label::new(display_name.clone())
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_weight(FontWeight::MEDIUM),
                                )
                                .child(
                                    Label::new(relative_history_time(entry.sent_at_unix_ms))
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground),
                                ),
                        )
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
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
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
        if !self.theme_config.application.confirm_destructive_actions {
            self.delete_history_entry_now(&history_id, window, cx);
            return;
        }
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, cx| {
            let delete_app = app.clone();
            let delete_id = history_id.clone();
            dialog
                .title(Label::new("Delete history entry?").font_weight(FontWeight::SEMIBOLD))
                .w(px(460.))
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
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
                        .cancel_text("Cancel")
                        .cancel_variant(ButtonVariant::Secondary),
                )
                .on_ok(move |_, window, cx| {
                    delete_app.update(cx, |this, cx| {
                        this.delete_history_entry_now(&delete_id, window, cx)
                    })
                })
        });
    }

    fn delete_history_entry_now(
        &mut self,
        history_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let reply = apply_history_agent_command(
            &mut self.history,
            HistoryAgentCommand::DeleteHistoryEntry {
                history_id: history_id.to_owned(),
            },
        );
        if !reply.ok {
            window.push_notification(Notification::error(reply.message), cx);
            return false;
        }
        self.persistence_dirty.store(true, Ordering::Release);
        window.push_notification(Notification::success("History entry deleted"), cx);
        cx.notify();
        true
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
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
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
                        .cancel_text("Cancel")
                        .cancel_variant(ButtonVariant::Secondary),
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
                                .request_ids()
                                .map(str::to_owned)
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
            .request_ids()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        for request_id in scoped_request_ids {
            self.refresh_request_variable_names(&request_id, cx);
        }
        self.persistence_dirty.store(true, Ordering::Release);
        cx.notify();
    }

    fn open_request_destination_picker(
        &mut self,
        request_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.environments.environments.is_empty() {
            window.push_notification(
                Notification::info("Create an environment before organizing requests"),
                cx,
            );
            return;
        }

        let current_environment_id = self
            .environments
            .environment_for_request(&request_id)
            .map(|environment| environment.id.clone());
        let current_folder_id = self
            .environments
            .folder_for_request(&request_id)
            .map(|folder| folder.id.clone());
        let mut environments = self.environments.environments.iter().collect::<Vec<_>>();
        environments.sort_by_key(|environment| environment.name.to_ascii_lowercase());

        let mut destinations = Vec::new();
        for environment in environments {
            destinations.push(RequestDestination {
                environment_id: environment.id.clone(),
                environment_name: environment.name.clone(),
                folder_id: None,
                folder_path: None,
            });
            let mut folders = environment.folder_paths();
            folders.sort_by_key(|(_, path)| path.to_ascii_lowercase());
            destinations.extend(folders.into_iter().map(|(folder_id, folder_path)| {
                RequestDestination {
                    environment_id: environment.id.clone(),
                    environment_name: environment.name.clone(),
                    folder_id: Some(folder_id),
                    folder_path: Some(folder_path),
                }
            }));
        }

        let app = cx.entity();
        let picker = cx.new(|cx| {
            RequestDestinationPickerView::new(
                app,
                request_id,
                Arc::new(destinations),
                current_environment_id,
                current_folder_id,
                window,
                cx,
            )
        });
        let focused_input = picker.read(cx).input.clone();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title(Label::new("Move request").font_weight(FontWeight::SEMIBOLD))
                .w(px(560.))
                .close_button(true)
                .bg(cx.theme().muted)
                .border_color(cx.theme().muted_foreground.opacity(0.32))
                .child(picker.clone())
        });
        focused_input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn assign_request_to_environment_folder(
        &mut self,
        request_id: &str,
        environment_id: &str,
        folder_id: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.iter().find(|tab| tab.draft.id == request_id) else {
            return;
        };
        let request = tab.snapshot(cx);
        if self
            .environments
            .assign_to_folder(environment_id, folder_id, request)
            .is_ok()
        {
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

    fn refresh_request_variable_names(&mut self, request_id: &str, cx: &mut App) {
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
            .and_then(|environment| environment.request(request_id))
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

    fn format_active_request_body(
        &mut self,
        _: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab = &self.tabs[self.active_tab];
        let body_input = tab.body.clone();
        let source = body_input.read(cx).value().to_string();
        let content_type = tab.headers.iter().find_map(|header| {
            (header.enabled
                && header
                    .key
                    .read(cx)
                    .value()
                    .trim()
                    .eq_ignore_ascii_case("content-type"))
            .then(|| header.value.read(cx).value().trim().to_owned())
        });

        match alula::format_request_body(&source, content_type.as_deref()) {
            Ok(formatted) if formatted.text == source => {
                window.push_notification(
                    Notification::info(format!(
                        "The {} request body is already formatted",
                        formatted.language.to_ascii_uppercase()
                    )),
                    cx,
                );
            }
            Ok(formatted) => {
                let language = formatted.language.to_ascii_uppercase();
                body_input.update(cx, |input, cx| {
                    input.set_value(formatted.text, window, cx);
                });
                self.persistence_dirty.store(true, Ordering::Release);
                window.push_notification(
                    Notification::success(format!("Formatted {language} request body")),
                    cx,
                );
                cx.notify();
            }
            Err(error) => window.push_notification(
                Notification::error(format!("Could not format request body: {error}")),
                cx,
            ),
        }
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
                            .child(
                                svg()
                                    .path("icons/alula-mark.svg")
                                    .size(px(19.))
                                    .text_color(cx.theme().primary_foreground),
                            ),
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
            .with_stable_animation(
                "brand-collapse",
                collapsed as usize,
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
            McpStatus::Stopped => div()
                .h(px(27.))
                .px_2()
                .flex()
                .items_center()
                .gap_2()
                .rounded_full()
                .border_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().secondary)
                .text_color(cx.theme().muted_foreground.opacity(0.68))
                .font_family(cx.theme().mono_font_family.clone())
                .text_size(px(10.))
                .child(
                    div()
                        .size(px(6.))
                        .rounded_full()
                        .bg(cx.theme().muted_foreground.opacity(0.5)),
                )
                .child("MCP off"),
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
                        .font_weight(FontWeight::BOLD),
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
                            .with_stable_animation(
                                "command-hover",
                                command_hovered as usize,
                                Animation::new(Duration::from_secs_f64(0.12))
                                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                move |this, delta| {
                                    this.opacity(if command_hovered {
                                        0.96 + 0.04 * delta
                                    } else {
                                        1.0
                                    })
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

    fn sidebar_request_row(&self, index: usize, app: Entity<Self>, cx: &mut App) -> Option<Div> {
        let tab = self.tabs.get(index)?;
        let method = tab.draft.method;
        let label = tab.title.clone();
        Some(
            div().h(px(31.)).pb(px(2.)).child(
                div()
                    .id(SharedString::from(format!(
                        "sidebar-request-{}",
                        tab.draft.id
                    )))
                    .w_full()
                    .h_full()
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
                        app.update(cx, |this, cx| {
                            this.active_tab = index.min(this.tabs.len().saturating_sub(1));
                            this.show_selected_request(cx);
                        });
                    }),
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
        let new_request_press_app = app.clone();
        let new_request_release_app = app.clone();
        let new_request_cancel_app = app.clone();

        let nav_item = |section: WorkspaceSection,
                        label: &'static str,
                        icon: IconName,
                        active: bool,
                        count: usize,
                        on_click: SidebarNavClick| {
            let hovered = self.sidebar_hovered == Some(section);
            let hover_app = app.clone();
            let press_app = app.clone();
            let release_app = app.clone();
            let cancel_app = app.clone();
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
                    this.hover(|this| this.bg(cx.theme().muted).text_color(cx.theme().foreground))
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
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    press_app.update(cx, |this, _| {
                        this.sidebar_pressed = Some(section);
                    });
                    window.prevent_default();
                })
                .on_mouse_up(MouseButton::Left, move |_, window, cx| {
                    let activate = release_app.update(cx, |this, _| {
                        let activate = this.sidebar_pressed == Some(section);
                        this.sidebar_pressed = None;
                        activate
                    });
                    if activate {
                        on_click(window, cx);
                    }
                })
                .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                    cancel_app.update(cx, |this, _| {
                        if this.sidebar_pressed == Some(section) {
                            this.sidebar_pressed = None;
                        }
                    });
                })
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
                .with_stable_animation(
                    SharedString::from(format!("sidebar-hover-{label}")),
                    hovered as usize,
                    Animation::new(Duration::from_secs_f64(0.12))
                        .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                    move |this, delta| {
                        this.opacity(if hovered { 0.96 + 0.04 * delta } else { 1.0 })
                    },
                )
        };

        let sidebar_requests_app = app.clone();
        let open_requests = uniform_list(
            "sidebar-open-requests",
            self.tabs.len(),
            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                range
                    .filter_map(|index| {
                        this.sidebar_request_row(index, sidebar_requests_app.clone(), cx)
                    })
                    .collect::<Vec<_>>()
            }),
        )
        .size_full();

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
                            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                                new_request_press_app.update(cx, |this, _| {
                                    this.new_request_pressed = true;
                                });
                                window.prevent_default();
                            })
                            .on_mouse_up(MouseButton::Left, move |_, window, cx| {
                                let activate = new_request_release_app.update(cx, |this, _| {
                                    std::mem::take(&mut this.new_request_pressed)
                                });
                                if activate {
                                    new_request_release_app.update(cx, |this, cx| {
                                        this.add_request(&ClickEvent::default(), window, cx)
                                    });
                                }
                            })
                            .on_mouse_up_out(MouseButton::Left, move |_, _, cx| {
                                new_request_cancel_app.update(cx, |this, _| {
                                    this.new_request_pressed = false;
                                });
                            })
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
                            .with_stable_animation(
                                "new-request-hover",
                                new_request_hovered as usize,
                                Animation::new(Duration::from_secs_f64(0.12))
                                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                move |this, delta| {
                                    this.opacity(if new_request_hovered {
                                        0.96 + 0.04 * delta
                                    } else {
                                        1.0
                                    })
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
                                Box::new(move |_, cx| {
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
                                Box::new(move |_, cx| {
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
                                Box::new(move |_, cx| {
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
                            .child(div().flex_1().min_h_0().child(open_requests)),
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

        sidebar.with_stable_animation(
            "sidebar-collapse",
            collapsed as usize,
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
        let has_environments = !self.environments.environments.is_empty();
        let assigned_request_ids = (self.tabs.len() > 4).then(|| {
            self.environments
                .assigned_request_ids(self.tabs.iter().map(|tab| tab.draft.id.as_str()))
        });
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
            let assigned_environment = assigned_request_ids.as_ref().map_or_else(
                || {
                    self.environments
                        .environment_for_request(&request_id)
                        .is_some()
                },
                |assigned_request_ids| assigned_request_ids.contains(request_id.as_str()),
            );
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
                    menu = menu.label("Request organization");
                    let picker_app = menu_app.clone();
                    let picker_request_id = request_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Move to environment or folder…")
                            .disabled(!has_environments)
                            .on_click(move |_, window, cx| {
                                picker_app.update(cx, |this, cx| {
                                    this.open_request_destination_picker(
                                        picker_request_id.clone(),
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    );
                    if assigned_environment {
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

    fn environment_variable_row(
        &self,
        environment_id: &str,
        variable_index: usize,
        app: Entity<Self>,
        cx: &mut App,
    ) -> Option<Div> {
        let environment = self
            .environments
            .environments
            .iter()
            .find(|environment| environment.id == environment_id)?;
        let variable = environment.variables.get(variable_index)?;
        let hovered = self.hovered_environment_variable_id.as_deref() == Some(variable.id.as_str());
        let hover_app = app.clone();
        let hover_id = variable.id.clone();
        let edit_app = app.clone();
        let edit_environment_id = environment.id.clone();
        let edit_variable_id = variable.id.clone();
        let row_edit_app = app.clone();
        let row_edit_environment_id = environment.id.clone();
        let row_edit_variable_id = variable.id.clone();
        let remove_app = app.clone();
        let remove_environment_id = environment.id.clone();
        let remove_variable_id = variable.id.clone();
        let revealed = variable.secret
            && self.revealed_secret_variable_id.as_deref() == Some(variable.id.as_str());
        let display_value = if variable.secret && !revealed {
            "••••••••".to_owned()
        } else {
            variable.value.clone().unwrap_or_default()
        };
        let action = if variable.secret {
            let reveal_app = app.clone();
            let reveal_id = variable.id.clone();
            design_button(
                SharedString::from(format!("reveal-environment-variable-{}", variable.id)),
                if revealed { "Hide" } else { "Reveal" },
            )
            .secondary()
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                reveal_app.update(cx, |this, cx| {
                    this.revealed_secret_variable_id = if this
                        .revealed_secret_variable_id
                        .as_deref()
                        == Some(reveal_id.as_str())
                    {
                        None
                    } else {
                        Some(reveal_id.clone())
                    };
                    cx.notify();
                });
            })
        } else {
            design_button(
                SharedString::from(format!("edit-environment-variable-{}", variable.id)),
                "Edit",
            )
            .secondary()
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                edit_app.update(cx, |this, cx| {
                    this.open_environment_variable_dialog(
                        edit_environment_id.clone(),
                        Some(edit_variable_id.clone()),
                        window,
                        cx,
                    )
                });
            })
        };
        Some(
            div().h(px(62.)).pt(px(1.)).pb(px(7.)).child(
                div()
                    .id(SharedString::from(format!(
                        "environment-variable-row-{}",
                        variable.id
                    )))
                    .relative()
                    .w_full()
                    .h_full()
                    .px(px(11.))
                    .flex()
                    .items_center()
                    .gap(px(11.))
                    .cursor_pointer()
                    .rounded(px(9.))
                    .border_1()
                    .border_color(if hovered {
                        cx.theme().muted_foreground.opacity(0.32)
                    } else {
                        cx.theme().border
                    })
                    .bg(if hovered {
                        cx.theme().secondary
                    } else {
                        cx.theme().background
                    })
                    .child(
                        div()
                            .w(px(210.))
                            .flex_shrink_0()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(10.))
                            .text_color(cx.theme().primary.lighten(0.14))
                            .child(variable.name.clone()),
                    )
                    .child(
                        Label::new(display_value)
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family(cx.theme().mono_font_family.clone())
                            .text_size(px(10.))
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(
                                design_button(
                                    SharedString::from(format!(
                                        "remove-environment-variable-{}",
                                        variable.id
                                    )),
                                    "Delete",
                                )
                                .danger()
                                .on_click(move |_, window, cx| {
                                    cx.stop_propagation();
                                    remove_app.update(cx, |this, cx| {
                                        this.remove_environment_variable(
                                            &remove_environment_id,
                                            &remove_variable_id,
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                            )
                            .child(action),
                    )
                    .on_click(move |_, window, cx| {
                        row_edit_app.update(cx, |this, cx| {
                            this.open_environment_variable_dialog(
                                row_edit_environment_id.clone(),
                                Some(row_edit_variable_id.clone()),
                                window,
                                cx,
                            )
                        });
                    })
                    .on_hover(move |is_hovered, _, cx| {
                        hover_app.update(cx, |this, cx| {
                            let next = if *is_hovered {
                                Some(hover_id.clone())
                            } else if this.hovered_environment_variable_id.as_deref()
                                == Some(hover_id.as_str())
                            {
                                None
                            } else {
                                this.hovered_environment_variable_id.clone()
                            };
                            if this.hovered_environment_variable_id != next {
                                this.hovered_environment_variable_id = next;
                                cx.notify();
                            }
                        });
                    })
                    .with_stable_animation(
                        SharedString::from(format!("environment-variable-hover-{}", variable.id)),
                        hovered as usize,
                        Animation::new(Duration::from_secs_f64(0.12))
                            .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                        move |this, delta| {
                            this.opacity(if hovered { 0.98 + 0.02 * delta } else { 1.0 })
                        },
                    ),
            ),
        )
    }

    fn render_environment_variable_rows(
        &self,
        environment: &alula::Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let query = self
            .environment_variable_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let filtered_indices = environment
            .variables
            .iter()
            .enumerate()
            .filter_map(|(index, variable)| {
                (query.is_empty()
                    || ascii_contains_ignore_case(&variable.name, &query)
                    || (!variable.secret
                        && ascii_contains_ignore_case(
                            variable.value.as_deref().unwrap_or_default(),
                            &query,
                        )))
                .then_some(index)
            })
            .collect::<Vec<_>>();
        if filtered_indices.is_empty() {
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

        let app = cx.entity();
        let environment_id = environment.id.clone();
        let list_id = SharedString::from(format!("environment-variables-{environment_id}"));
        div().size_full().min_h_0().child(
            uniform_list(
                list_id,
                filtered_indices.len(),
                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                    range
                        .filter_map(|index| {
                            this.environment_variable_row(
                                &environment_id,
                                filtered_indices[index],
                                app.clone(),
                                cx,
                            )
                        })
                        .collect::<Vec<_>>()
                }),
            )
            .size_full(),
        )
    }

    fn environment_request_row(
        &self,
        environment_id: &str,
        request: &RequestDraft,
        app: Entity<Self>,
        cx: &mut App,
    ) -> Div {
        let hovered = self.hovered_environment_request_id.as_deref() == Some(request.id.as_str());
        let hover_app = app.clone();
        let hover_id = request.id.clone();
        let open_app = app.clone();
        let open_environment_id = environment_id.to_owned();
        let open_request_id = request.id.clone();
        div().h(px(62.)).pt(px(1.)).pb(px(7.)).child(
            div()
                .id(SharedString::from(format!(
                    "environment-request-row-{}",
                    request.id
                )))
                .relative()
                .w_full()
                .h_full()
                .px(px(11.))
                .flex()
                .items_center()
                .gap(px(11.))
                .rounded(px(9.))
                .border_1()
                .border_color(if hovered {
                    cx.theme().muted_foreground.opacity(0.32)
                } else {
                    cx.theme().border
                })
                .bg(if hovered {
                    cx.theme().secondary
                } else {
                    cx.theme().background
                })
                .child(method_badge(request.method, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .child(
                            Label::new(request.display_name())
                                .truncate()
                                .text_size(px(12.))
                                .font_weight(FontWeight(580.)),
                        )
                        .child(
                            Label::new(request.url.clone())
                                .mt(px(4.))
                                .truncate()
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_size(px(9.))
                                .text_color(cx.theme().muted_foreground.opacity(0.68)),
                        ),
                )
                .child(
                    design_button(
                        SharedString::from(format!("open-environment-request-{}", request.id)),
                        "Open",
                    )
                    .secondary()
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
                )
                .on_hover(move |is_hovered, _, cx| {
                    hover_app.update(cx, |this, cx| {
                        let next = if *is_hovered {
                            Some(hover_id.clone())
                        } else if this.hovered_environment_request_id.as_deref()
                            == Some(hover_id.as_str())
                        {
                            None
                        } else {
                            this.hovered_environment_request_id.clone()
                        };
                        if this.hovered_environment_request_id != next {
                            this.hovered_environment_request_id = next;
                            cx.notify();
                        }
                    });
                })
                .with_stable_animation(
                    SharedString::from(format!("environment-request-hover-{}", request.id)),
                    hovered as usize,
                    Animation::new(Duration::from_secs_f64(0.12))
                        .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                    move |this, delta| {
                        this.opacity(if hovered { 0.98 + 0.02 * delta } else { 1.0 })
                    },
                ),
        )
    }

    fn render_environment_folder_section(
        &self,
        environment_id: &str,
        folder: &EnvironmentFolder,
        query: &str,
        depth: usize,
        app: Entity<Self>,
        cx: &mut App,
    ) -> Option<Div> {
        let matching_count = if query.is_empty() {
            folder.request_count()
        } else {
            folder_matching_request_count(folder, query)
        };
        if !query.is_empty() && matching_count == 0 {
            return None;
        }
        let expanded =
            !query.is_empty() || self.expanded_environment_folder_ids.contains(&folder.id);
        let folder_requests = expanded.then(|| {
            folder
                .requests
                .iter()
                .filter(|request| request_matches_query(request, query))
                .collect::<Vec<_>>()
        });
        let add_request_app = app.clone();
        let add_request_environment_id = environment_id.to_owned();
        let add_request_folder_id = folder.id.clone();
        let add_folder_app = app.clone();
        let add_folder_environment_id = environment_id.to_owned();
        let add_folder_parent_id = folder.id.clone();
        let rename_app = app.clone();
        let rename_environment_id = environment_id.to_owned();
        let rename_folder_id = folder.id.clone();
        let delete_app = app.clone();
        let delete_environment_id = environment_id.to_owned();
        let delete_folder_id = folder.id.clone();
        let toggle_app = app.clone();
        let toggle_folder_id = folder.id.clone();
        let count = if query.is_empty() {
            folder.request_count()
        } else {
            matching_count
        };
        let folder_toggle = div()
            .id(SharedString::from(format!(
                "environment-folder-toggle-{}",
                folder.id
            )))
            .h_full()
            .flex_1()
            .min_w_0()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .text_size(px(11.))
            .font_weight(FontWeight(600.))
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(px(13.)),
            )
            .child(Icon::new(IconName::FolderOpen).size(px(14.)))
            .child(Label::new(folder.name.clone()).truncate())
            .child(quiet_badge(count.to_string(), cx))
            .on_click(move |_, _, cx| {
                toggle_app.update(cx, |this, cx| {
                    if !this
                        .expanded_environment_folder_ids
                        .remove(&toggle_folder_id)
                    {
                        this.expanded_environment_folder_ids
                            .insert(toggle_folder_id.clone());
                    }
                    cx.notify();
                });
            });
        let folder_actions =
            Button::new(SharedString::from(format!("folder-actions-{}", folder.id)))
                .ghost()
                .xsmall()
                .compact()
                .icon(IconName::Ellipsis)
                .tooltip("Folder actions")
                .dropdown_menu(move |mut menu, _, _| {
                    let action_app = add_folder_app.clone();
                    let action_environment_id = add_folder_environment_id.clone();
                    let action_parent_id = add_folder_parent_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("New folder")
                            .icon(IconName::Folder)
                            .on_click(move |_, window, cx| {
                                action_app.update(cx, |this, cx| {
                                    this.open_environment_folder_dialog(
                                        action_environment_id.clone(),
                                        Some(action_parent_id.clone()),
                                        None,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    );

                    let action_app = rename_app.clone();
                    let action_environment_id = rename_environment_id.clone();
                    let action_folder_id = rename_folder_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Rename")
                            .icon(IconName::Replace)
                            .on_click(move |_, window, cx| {
                                action_app.update(cx, |this, cx| {
                                    this.open_environment_folder_dialog(
                                        action_environment_id.clone(),
                                        None,
                                        Some(action_folder_id.clone()),
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    );

                    let action_app = delete_app.clone();
                    let action_environment_id = delete_environment_id.clone();
                    let action_folder_id = delete_folder_id.clone();
                    menu = menu.item(
                        PopupMenuItem::new("Delete")
                            .icon(IconName::Delete)
                            .on_click(move |_, window, cx| {
                                action_app.update(cx, |this, cx| {
                                    this.delete_environment_folder(
                                        &action_environment_id,
                                        &action_folder_id,
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    );

                    let action_app = add_request_app.clone();
                    let action_environment_id = add_request_environment_id.clone();
                    let action_folder_id = add_request_folder_id.clone();
                    menu.item(
                        PopupMenuItem::new("New request")
                            .icon(IconName::Plus)
                            .on_click(move |_, window, cx| {
                                action_app.update(cx, |this, cx| {
                                    this.add_environment_request(
                                        &action_environment_id,
                                        Some(&action_folder_id),
                                        window,
                                        cx,
                                    )
                                });
                            }),
                    )
                })
                .anchor(Corner::TopRight);
        let mut section = div()
            .ml(px(depth as f32 * 14.))
            .mb_2()
            .flex()
            .flex_col()
            .rounded(px(8.))
            .border_1()
            .border_color(cx.theme().border)
            .px_1()
            .child(
                div()
                    .h(px(38.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(folder_toggle)
                    .child(folder_actions),
            );
        if expanded {
            let mut child_folders = folder.folders.iter().collect::<Vec<_>>();
            child_folders.sort_by_key(|folder| folder.name.to_ascii_lowercase());
            for child in child_folders {
                if let Some(child_section) = self.render_environment_folder_section(
                    environment_id,
                    child,
                    query,
                    depth + 1,
                    app.clone(),
                    cx,
                ) {
                    section = section.child(child_section);
                }
            }
            for request in folder_requests.into_iter().flatten() {
                section = section.child(self.environment_request_row(
                    environment_id,
                    request,
                    app.clone(),
                    cx,
                ));
            }
        }
        Some(section)
    }

    fn render_environment_request_rows(
        &self,
        environment: &alula::Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let query = self
            .environment_request_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        if environment.folders.is_empty() {
            let filtered_indices = environment
                .requests
                .iter()
                .enumerate()
                .filter_map(|(index, request)| {
                    request_matches_query(request, &query).then_some(index)
                })
                .collect::<Vec<_>>();
            if filtered_indices.is_empty() {
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

            let app = cx.entity();
            let environment_id = environment.id.clone();
            let list_id = SharedString::from(format!("environment-requests-{environment_id}"));
            return div().size_full().min_h_0().child(
                uniform_list(
                    list_id,
                    filtered_indices.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        let Some(environment) = this
                            .environments
                            .environments
                            .iter()
                            .find(|environment| environment.id == environment_id)
                        else {
                            return Vec::new();
                        };
                        range
                            .filter_map(|position| {
                                let request =
                                    environment.requests.get(filtered_indices[position])?;
                                Some(this.environment_request_row(
                                    &environment_id,
                                    request,
                                    app.clone(),
                                    cx,
                                ))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full(),
            );
        }

        let matching_count = environment
            .requests
            .iter()
            .filter(|request| request_matches_query(request, &query))
            .count()
            + environment
                .folders
                .iter()
                .map(|folder| folder_matching_request_count(folder, &query))
                .sum::<usize>();
        if matching_count == 0 && (!query.is_empty() || environment.folders.is_empty()) {
            return empty_state(
                if query.is_empty() && environment.folders.is_empty() {
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

        let app = cx.entity();
        let environment_id = environment.id.clone();
        let root_requests = environment
            .requests
            .iter()
            .filter(|request| request_matches_query(request, &query))
            .collect::<Vec<_>>();
        let mut list = div()
            .size_full()
            .min_h_0()
            .overflow_y_scrollbar()
            .flex()
            .flex_col()
            .gap_3()
            .pb_2();

        let mut folders = environment.folders.iter().collect::<Vec<_>>();
        folders.sort_by_key(|folder| folder.name.to_ascii_lowercase());
        for folder in folders {
            if let Some(section) = self.render_environment_folder_section(
                &environment_id,
                folder,
                &query,
                0,
                app.clone(),
                cx,
            ) {
                list = list.child(section);
            }
        }
        if !root_requests.is_empty() {
            let mut root_section = div().flex().flex_col().child(
                div()
                    .h(px(30.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_size(px(11.))
                    .font_weight(FontWeight(600.))
                    .child("Requests")
                    .child(quiet_badge(root_requests.len().to_string(), cx)),
            );
            for request in root_requests {
                root_section = root_section.child(self.environment_request_row(
                    &environment_id,
                    request,
                    app.clone(),
                    cx,
                ));
            }
            list = list.child(root_section);
        }
        div().size_full().min_h_0().child(list)
    }

    fn render_environment_detail(
        &self,
        environment: &alula::Environment,
        cx: &mut Context<Self>,
    ) -> Div {
        let app = cx.entity();
        let back_app = app.clone();
        let export_app = app.clone();
        let export_id = environment.id.clone();
        let delete_app = app.clone();
        let delete_id = environment.id.clone();
        let delete_name = environment.name.clone();
        let requests_selected = self.environment_detail_tab == EnvironmentDetailTab::Requests;
        let variables_selected = self.environment_detail_tab == EnvironmentDetailTab::Variables;
        let request_tab_app = app.clone();
        let variable_tab_app = app.clone();
        let new_folder_app = app.clone();
        let new_folder_environment_id = environment.id.clone();
        let (search, rows, add_action) = if requests_selected {
            let add_app = app.clone();
            let add_environment_id = environment.id.clone();
            (
                Input::new(&self.environment_request_search)
                    .prefix(IconName::Search)
                    .h(px(32.))
                    .text_size(px(11.))
                    .rounded(px(6.))
                    .w_full(),
                self.render_environment_request_rows(environment, cx),
                Some(
                    design_icon_button(
                        "environment-detail-add-request",
                        IconName::Plus,
                        "New request",
                    )
                    .primary()
                    .on_click(move |_, window, cx| {
                        add_app.update(cx, |this, cx| {
                            this.add_environment_request(&add_environment_id, None, window, cx)
                        });
                    }),
                ),
            )
        } else {
            let add_app = app.clone();
            let add_environment_id = environment.id.clone();
            (
                Input::new(&self.environment_variable_search)
                    .prefix(IconName::Search)
                    .h(px(32.))
                    .text_size(px(11.))
                    .rounded(px(6.))
                    .w_full(),
                self.render_environment_variable_rows(environment, cx),
                Some(
                    design_icon_button(
                        "environment-detail-add-variable",
                        IconName::Plus,
                        "New variable",
                    )
                    .primary()
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
                            div().size(px(30.)).flex_none().child(
                                Button::new("back-to-environments")
                                    .secondary()
                                    .size_full()
                                    .px_0()
                                    .rounded(px(6.))
                                    .child(Icon::new(IconName::ArrowLeft).size(px(15.)))
                                    .tooltip("Back to environments")
                                    .on_click(move |_, _, cx| {
                                        back_app.update(cx, |this, cx| {
                                            this.close_environment_details(cx)
                                        });
                                    }),
                            ),
                        )
                        .child(
                            div()
                                // GPUI measures the custom button's intrinsic icon width here;
                                // keep the reference's 12 px optical gap from the painted border.
                                .ml(px(16.))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    Label::new(environment.name.clone())
                                        .text_size(px(14.))
                                        .font_weight(FontWeight(630.)),
                                )
                                .child(
                                    Label::new(format!(
                                        "{} requests · {} variables · Active environment",
                                        environment.request_count(),
                                        environment.variables.len()
                                    ))
                                    .text_size(px(10.))
                                    .text_color(cx.theme().muted_foreground.opacity(0.68)),
                                ),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    design_button("export-environment-detail", "Export")
                                        .secondary()
                                        .on_click(move |_, window, cx| {
                                            export_app.update(cx, |this, cx| {
                                                this.export_environment(&export_id, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    design_button(
                                        "delete-environment-detail",
                                        "Delete environment",
                                    )
                                    .danger()
                                    .on_click(
                                        move |_, window, cx| {
                                            delete_app.update(cx, |this, cx| {
                                                this.confirm_delete_environment(
                                                    delete_id.clone(),
                                                    delete_name.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        },
                                    ),
                                ),
                        ),
                )
                .child(
                    div()
                        .h(px(42.))
                        .px_4()
                        .flex_shrink_0()
                        .flex()
                        .gap(px(18.))
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            Button::new("environment-requests-tab")
                                .custom(ButtonCustomVariant::new(cx).foreground(
                                    if requests_selected {
                                        cx.theme().foreground
                                    } else {
                                        cx.theme().muted_foreground
                                    },
                                ))
                                .with_size(px(12.57))
                                .relative()
                                .h_full()
                                .px_0()
                                .rounded_none()
                                .font_weight(FontWeight::NORMAL)
                                .child(
                                    div().flex().items_center().gap_2().child("Requests").child(
                                        div()
                                            .px_1p5()
                                            .py_0p5()
                                            .rounded_full()
                                            .bg(cx.theme().muted)
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(px(9.))
                                            .text_color(cx.theme().muted_foreground)
                                            .child(environment.request_count().to_string()),
                                    ),
                                )
                                .when(requests_selected, |this| {
                                    this.child(
                                        div()
                                            .absolute()
                                            .left_0()
                                            .right_0()
                                            .bottom(px(-1.))
                                            .h(px(2.))
                                            .rounded_t(px(2.))
                                            .bg(cx.theme().primary),
                                    )
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
                                .custom(ButtonCustomVariant::new(cx).foreground(
                                    if variables_selected {
                                        cx.theme().foreground
                                    } else {
                                        cx.theme().muted_foreground
                                    },
                                ))
                                .with_size(px(12.57))
                                .relative()
                                .h_full()
                                .px_0()
                                .rounded_none()
                                .font_weight(FontWeight::NORMAL)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .child("Variables")
                                        .child(
                                            div()
                                                .px_1p5()
                                                .py_0p5()
                                                .rounded_full()
                                                .bg(cx.theme().muted)
                                                .font_family(cx.theme().mono_font_family.clone())
                                                .text_size(px(9.))
                                                .text_color(cx.theme().muted_foreground)
                                                .child(environment.variables.len().to_string()),
                                        ),
                                )
                                .when(variables_selected, |this| {
                                    this.child(
                                        div()
                                            .absolute()
                                            .left_0()
                                            .right_0()
                                            .bottom(px(-1.))
                                            .h(px(2.))
                                            .rounded_t(px(2.))
                                            .bg(cx.theme().primary),
                                    )
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
                        .child(
                            div()
                                .w_full()
                                .flex_shrink_0()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(div().w(px(280.)).child(search))
                                .child(
                                    div()
                                        .ml_auto()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .when(requests_selected, |this| {
                                            this.child(
                                                design_button(
                                                    "environment-detail-add-folder",
                                                    "New folder",
                                                )
                                                .secondary()
                                                .on_click(move |_, window, cx| {
                                                    new_folder_app.update(cx, |this, cx| {
                                                        this.open_environment_folder_dialog(
                                                            new_folder_environment_id.clone(),
                                                            None,
                                                            None,
                                                            window,
                                                            cx,
                                                        )
                                                    });
                                                }),
                                            )
                                        })
                                        .children(add_action),
                                ),
                        )
                        .child(div().flex_1().min_h_0().child(rows)),
                ),
        )
    }

    fn environment_card(&self, index: usize, app: Entity<Self>, cx: &mut App) -> Option<Div> {
        let environment = self.environments.environments.get(index)?;
        let hovered = self.hovered_environment_id.as_deref() == Some(environment.id.as_str());
        let hover_app = app.clone();
        let hover_id = environment.id.clone();
        let open_app = app.clone();
        let open_id = environment.id.clone();
        let view_app = app.clone();
        let view_id = environment.id.clone();
        let delete_app = app.clone();
        let delete_id = environment.id.clone();
        let delete_name = environment.name.clone();
        Some(
            div()
                .w_full()
                .h(px(80.))
                .flex_shrink_0()
                .pt(px(1.))
                .pb(px(7.))
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "environment-card-{}",
                            environment.id
                        )))
                        .w_full()
                        .h_full()
                        .px(px(13.))
                        .flex()
                        .items_center()
                        .cursor_pointer()
                        .rounded(px(9.))
                        .border_1()
                        .border_color(if hovered {
                            cx.theme().muted_foreground.opacity(0.32)
                        } else {
                            cx.theme().border
                        })
                        .bg(if hovered {
                            cx.theme().secondary
                        } else {
                            cx.theme().background
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .items_center()
                                .gap(px(7.))
                                .child(
                                    div()
                                        .size(px(15.))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_full()
                                        .bg(cx.theme().primary.opacity(0.12))
                                        .child(
                                            div()
                                                .size(px(7.))
                                                .rounded_full()
                                                .bg(cx.theme().primary),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .flex_col()
                                        .child(
                                            Label::new(environment.name.clone())
                                                .truncate()
                                                .text_size(px(13.))
                                                .font_weight(FontWeight(610.)),
                                        )
                                        .child(
                                            Label::new(format!(
                                                "{} variables · {} requests",
                                                environment.variables.len(),
                                                environment.request_count()
                                            ))
                                            .mt(px(5.))
                                            .text_size(px(10.))
                                            .text_color(cx.theme().muted_foreground.opacity(0.68)),
                                        ),
                                ),
                        )
                        .child(
                            div()
                                .ml(px(16.))
                                .flex()
                                .items_center()
                                .gap(px(5.))
                                .child(
                                    design_button(
                                        SharedString::from(format!(
                                            "delete-environment-{}",
                                            environment.id
                                        )),
                                        "Delete",
                                    )
                                    .text()
                                    .px(px(7.))
                                    .on_click(
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            delete_app.update(cx, |this, cx| {
                                                this.confirm_delete_environment(
                                                    delete_id.clone(),
                                                    delete_name.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        },
                                    ),
                                )
                                .child(
                                    design_button(
                                        SharedString::from(format!(
                                            "view-environment-{}",
                                            environment.id
                                        )),
                                        "View",
                                    )
                                    .secondary()
                                    .on_click(
                                        move |_, window, cx| {
                                            cx.stop_propagation();
                                            view_app.update(cx, |this, cx| {
                                                this.open_environment_details(
                                                    view_id.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        },
                                    ),
                                ),
                        )
                        .on_click(move |_, window, cx| {
                            open_app.update(cx, |this, cx| {
                                this.open_environment_details(open_id.clone(), window, cx)
                            });
                        })
                        .on_hover(move |is_hovered, _, cx| {
                            hover_app.update(cx, |this, cx| {
                                let next = if *is_hovered {
                                    Some(hover_id.clone())
                                } else if this.hovered_environment_id.as_deref()
                                    == Some(hover_id.as_str())
                                {
                                    None
                                } else {
                                    this.hovered_environment_id.clone()
                                };
                                if this.hovered_environment_id != next {
                                    this.hovered_environment_id = next;
                                    cx.notify();
                                }
                            });
                        })
                        .with_stable_animation(
                            SharedString::from(format!(
                                "environment-card-hover-{}",
                                environment.id
                            )),
                            hovered as usize,
                            Animation::new(Duration::from_secs_f64(0.12))
                                .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                            move |this, delta| {
                                this.opacity(if hovered { 0.98 + 0.02 * delta } else { 1.0 })
                            },
                        ),
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
        let filtered_indices = self
            .environments
            .environments
            .iter()
            .enumerate()
            .filter_map(|(index, environment)| {
                (query.is_empty() || ascii_contains_ignore_case(&environment.name, &query))
                    .then_some(index)
            })
            .collect::<Vec<_>>();
        let content = if filtered_indices.is_empty() {
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(empty_state(
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
                ))
        } else {
            div().flex_1().min_h_0().p_3().child(
                uniform_list(
                    "environment-index",
                    filtered_indices.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .filter_map(|index| {
                                this.environment_card(filtered_indices[index], app.clone(), cx)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full(),
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
                        .h(px(66.))
                        .px(px(15.))
                        .flex_shrink_0()
                        .flex()
                        .items_center()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    Label::new("Environments")
                                        .text_size(px(16.))
                                        .font_weight(FontWeight(650.)),
                                )
                                .child(
                                    Label::new(
                                        "Reusable request groups, endpoints, and scoped variables",
                                    )
                                    .text_size(px(10.))
                                    .text_color(cx.theme().muted_foreground.opacity(0.68)),
                                ),
                        )
                        .child(
                            div()
                                .ml_auto()
                                .flex()
                                .items_center()
                                .gap(px(7.))
                                .child(
                                    div().w(px(280.)).child(
                                        Input::new(&self.environment_search)
                                            .prefix(IconName::Search)
                                            .h(px(32.))
                                            .text_size(px(11.))
                                            .rounded(px(6.))
                                            .w_full(),
                                    ),
                                )
                                .child(quiet_badge(
                                    format!("{} total", self.environments.environments.len()),
                                    cx,
                                ))
                                .child(
                                    design_button("import-environment", "Import")
                                        .secondary()
                                        .on_click(
                                            cx.listener(Self::open_import_environment_dialog),
                                        ),
                                )
                                .child(
                                    design_icon_button(
                                        "new-environment",
                                        IconName::Plus,
                                        "New environment",
                                    )
                                    .primary()
                                    .on_click(cx.listener(Self::open_environment_dialog)),
                                ),
                        ),
                )
                .child(content),
        )
    }

    fn render_environments(&self, cx: &mut Context<Self>) -> Div {
        let transition_key = self
            .selected_environment_id
            .as_deref()
            .map(|id| format!("environment-detail-enter-{id}"))
            .unwrap_or_else(|| "environment-index-enter".to_owned());
        let page = self
            .selected_environment_id
            .as_deref()
            .and_then(|environment_id| {
                self.environments
                    .environments
                    .iter()
                    .find(|environment| environment.id == environment_id)
            })
            .map(|environment| self.render_environment_detail(environment, cx))
            .unwrap_or_else(|| self.render_environment_index(cx));
        div().size_full().min_h_0().child(
            page.with_animation(
                SharedString::from(transition_key),
                Animation::new(Duration::from_secs_f64(0.18))
                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                |this, delta| this.opacity(delta),
            ),
        )
    }

    fn history_entry_row(&self, index: usize, app: Entity<Self>, cx: &mut App) -> Option<Div> {
        let entry = self.history.entries.get(index)?;
        let open_history_id = entry.id.clone();
        let delete_history_app = app.clone();
        let delete_history_id = entry.id.clone();
        let delete_history_name = entry.request.display_name();
        let hovered = self.hovered_history_id.as_deref() == Some(entry.id.as_str());
        let hover_app = app.clone();
        let hover_id = entry.id.clone();
        let (outcome, outcome_color, outcome_detail) = if let Some(status) = entry.status {
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
                None,
            )
        } else {
            let error = entry
                .error
                .clone()
                .unwrap_or_else(|| "Request failed".into());
            let (summary, detail) = split_history_error(&error);
            (summary, cx.theme().danger, detail)
        };
        let has_outcome_detail = outcome_detail.is_some();
        let outcome_is_truncatable_error = entry.status.is_none() && !has_outcome_detail;
        Some(
            div().h(px(70.)).pt(px(1.)).pb(px(7.)).child(
                div()
                    .id(SharedString::from(format!("history-row-{}", entry.id)))
                    .relative()
                    .w_full()
                    .h_full()
                    .px(px(11.))
                    .py_2()
                    .flex()
                    .items_center()
                    .gap(px(11.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(if hovered {
                        cx.theme().muted_foreground.opacity(0.32)
                    } else {
                        cx.theme().border
                    })
                    .bg(if hovered {
                        cx.theme().secondary
                    } else {
                        cx.theme().background
                    })
                    .child(method_badge(entry.request.method, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .child(
                                Label::new(entry.request.display_name())
                                    .truncate()
                                    .text_size(px(12.))
                                    .font_weight(FontWeight(580.)),
                            )
                            .child(
                                div()
                                    .mt(px(5.))
                                    .min_w_0()
                                    .overflow_hidden()
                                    .flex()
                                    .items_center()
                                    .gap(px(9.))
                                    .child(
                                        div()
                                            .min_w_0()
                                            .when(outcome_is_truncatable_error, |this| {
                                                this.flex_1()
                                            })
                                            .when(!outcome_is_truncatable_error, |this| {
                                                this.flex_shrink_0()
                                            })
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .px(px(6.))
                                            .py(px(3.))
                                            .rounded_full()
                                            .bg(outcome_color.opacity(0.1))
                                            .font_family(cx.theme().mono_font_family.clone())
                                            .text_size(px(8.))
                                            .text_color(outcome_color)
                                            .child(outcome),
                                    )
                                    .children(outcome_detail.map(|detail| {
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(9.))
                                            .text_color(cx.theme().muted_foreground.opacity(0.68))
                                            .child(detail)
                                    }))
                                    .child(
                                        Label::new(relative_history_time(entry.sent_at_unix_ms))
                                            .flex_shrink_0()
                                            .truncate()
                                            .text_size(px(9.))
                                            .text_color(cx.theme().muted_foreground.opacity(0.68)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .child(
                                design_button(
                                    SharedString::from(format!(
                                        "open-history-request-{}",
                                        entry.id
                                    )),
                                    "Open as tab",
                                )
                                .secondary()
                                .on_click(move |_, window, cx| {
                                    app.update(cx, |this, cx| {
                                        this.open_history_request(&open_history_id, window, cx)
                                    });
                                }),
                            )
                            .child(
                                design_button(
                                    SharedString::from(format!(
                                        "delete-history-entry-{}",
                                        entry.id
                                    )),
                                    "Delete",
                                )
                                .danger()
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
                    )
                    .on_hover(move |is_hovered, _, cx| {
                        hover_app.update(cx, |this, cx| {
                            let next = if *is_hovered {
                                Some(hover_id.clone())
                            } else if this.hovered_history_id.as_deref() == Some(hover_id.as_str())
                            {
                                None
                            } else {
                                this.hovered_history_id.clone()
                            };
                            if this.hovered_history_id != next {
                                this.hovered_history_id = next;
                                cx.notify();
                            }
                        });
                    })
                    .with_stable_animation(
                        SharedString::from(format!("history-hover-{}", entry.id)),
                        hovered as usize,
                        Animation::new(Duration::from_secs_f64(0.12))
                            .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                        move |this, delta| {
                            this.opacity(if hovered { 0.98 + 0.02 * delta } else { 1.0 })
                        },
                    ),
            ),
        )
    }

    fn render_history(&self, cx: &mut Context<Self>) -> Div {
        let query = self
            .history_search
            .read(cx)
            .value()
            .trim()
            .to_ascii_lowercase();
        let filtered_indices = if query.is_empty() {
            (0..self.history.entries.len()).collect::<Vec<_>>()
        } else {
            self.history
                .entries
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    history_entry_matches_query(entry, &query).then_some(index)
                })
                .collect::<Vec<_>>()
        };
        let visible_count = filtered_indices.len();
        let count_label = if query.is_empty() {
            format!("{} entries", self.history.entries.len())
        } else {
            format!("{} of {}", visible_count, self.history.entries.len())
        };
        let content = if self.history.entries.is_empty() {
            div().flex_1().min_h_0().child(empty_state(
                "No request history yet",
                "Each completed or failed send is recorded independently of tabs",
                cx,
            ))
        } else if filtered_indices.is_empty() {
            div().flex_1().min_h_0().child(empty_state(
                "No matching history",
                "Try a different method, request name, URL, status, or error",
                cx,
            ))
        } else {
            let app = cx.entity();
            div().flex_1().min_h_0().p_3().child(
                uniform_list(
                    "history-entries",
                    filtered_indices.len(),
                    cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                        range
                            .filter_map(|index| {
                                this.history_entry_row(filtered_indices[index], app.clone(), cx)
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .size_full(),
            )
        };
        let page = div()
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
                            .h(px(66.))
                            .px(px(15.))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .border_b_1()
                            .border_color(cx.theme().border)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        Label::new("History")
                                            .text_size(px(16.))
                                            .font_weight(FontWeight(650.)),
                                    )
                                    .child(
                                        Label::new("Persistent request executions; response bodies are not retained")
                                            .text_size(px(10.))
                                            .text_color(cx.theme().muted_foreground.opacity(0.68)),
                                    ),
                            )
                            .child(
                                div()
                                    .ml_auto()
                                    .flex()
                                    .items_center()
                                    .gap(px(7.))
                                    .child(
                                        div().w(px(280.)).child(
                                            Input::new(&self.history_search)
                                                .prefix(IconName::Search)
                                                .h(px(32.))
                                                .text_size(px(11.))
                                                .rounded(px(6.))
                                                .w_full(),
                                        ),
                                    )
                                    .child(quiet_badge(count_label, cx)),
                            ),
                    )
                    .child(content),
            );
        div().size_full().min_h_0().child(
            page.with_animation(
                "history-workspace-enter",
                Animation::new(Duration::from_secs_f64(0.28))
                    .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                |this, delta| this.opacity(delta),
            ),
        )
    }

    fn render_request_builder(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let tab = &self.tabs[self.active_tab];
        let method = tab.method.clone();
        let url = tab.url.clone();
        let url_state = tab.url_template_state.clone();
        let has_url_error = url_state.error.is_some();
        let mut url_input = Input::new(&url)
            .large()
            .appearance(false)
            .focus_bordered(false)
            .w_full();
        if let Some(message) = &url_state.error {
            url_input = url_input.text_color(cx.theme().danger).suffix(
                Button::new("url-variable-error")
                    .ghost()
                    .small()
                    .compact()
                    .icon(IconName::TriangleAlert)
                    .tooltip(message.clone()),
            );
        }
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
                .with_stable_animation(
                    SharedString::from(format!("send-hover-arrow-{send_animation_id}")),
                    send_hovered as usize,
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
                                .with_stable_animation(
                                    SharedString::from(format!("send-hover-{send_animation_id}")),
                                    send_hovered as usize,
                                    Animation::new(Duration::from_secs_f64(0.12))
                                        .with_easing(cubic_bezier(0.2, 0.8, 0.2, 1.0)),
                                    move |this, delta| {
                                        this.opacity(if send_hovered {
                                            0.96 + 0.04 * delta
                                        } else {
                                            1.0
                                        })
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
        div()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .p_3()
            .pt_2()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .h(px(26.))
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .child(
                        Label::new("Auto-detects JSON, HTML, XML, CSS, or JavaScript")
                            .text_size(px(9.))
                            .text_color(cx.theme().muted_foreground.opacity(0.68)),
                    )
                    .child(
                        design_button("format-request-body", "Format body")
                            .ml_auto()
                            .small()
                            .secondary()
                            .on_click(cx.listener(Self::format_active_request_body)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .when(state.error.is_some(), |this| {
                        this.border_1()
                            .border_color(cx.theme().danger.opacity(0.7))
                            .rounded(cx.theme().radius)
                    })
                    .child(Input::new(&body).size_full())
                    .when(state.is_valid(), |this| {
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
                    .when_some(state.error.clone(), |this, message| {
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
                    }),
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
                TemplateVisualState::default()
            };
            let value_state = if enabled {
                pair.value_template_state.borrow().clone()
            } else {
                TemplateVisualState::default()
            };
            let has_error = key_state.error.is_some() || value_state.error.is_some();
            let mut key_input = Input::new(&key)
                .small()
                .appearance(false)
                .focus_bordered(false)
                .w_full()
                .disabled(!enabled);
            if let Some(message) = &key_state.error {
                key_input = key_input.text_color(cx.theme().danger).suffix(
                    Button::new(("pair-key-variable-error", index))
                        .ghost()
                        .small()
                        .compact()
                        .icon(IconName::TriangleAlert)
                        .tooltip(message.clone()),
                );
            }
            let mut value_input = Input::new(&value)
                .small()
                .appearance(false)
                .focus_bordered(false)
                .w_full()
                .disabled(!enabled);
            if let Some(message) = &value_state.error {
                value_input = value_input.text_color(cx.theme().danger).suffix(
                    Button::new(("pair-value-variable-error", index))
                        .ghost()
                        .small()
                        .compact()
                        .icon(IconName::TriangleAlert)
                        .tooltip(message.clone()),
                );
            }
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

    fn response_switch_button(
        segment: ResponseSwitchSegment,
        selected: bool,
        style: ButtonCustomVariant,
        app: Entity<Self>,
        cx: &App,
    ) -> Button {
        Button::new(segment.id)
            .custom(style)
            // The 27 px switch has a 1 px border and 2 px padding on each side,
            // leaving an exact 21 px fill area for every segment.
            .h(px(21.))
            .w(segment.width)
            .px_2()
            .rounded(px(5.))
            .selected(selected)
            .when(selected, |this| {
                this.bg(cx.theme().secondary.lighten(0.1))
                    .text_color(cx.theme().foreground)
                    .shadow_xs()
            })
            .child(
                div()
                    .text_size(px(9.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if selected {
                        cx.theme().foreground
                    } else {
                        cx.theme().muted_foreground.opacity(0.72)
                    })
                    .child(segment.label),
            )
            .on_click(move |_, _, cx| {
                app.update(cx, |this, cx| this.set_response_view(segment.mode, cx));
            })
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
                                    this.child(Self::response_switch_button(
                                        ResponseSwitchSegment {
                                            id: "response-messages",
                                            label: "Messages",
                                            width: px(62.),
                                            mode: ResponseViewMode::Messages,
                                        },
                                        messages,
                                        mode_button_style,
                                        messages_app,
                                        cx,
                                    ))
                                })
                                .when(!websocket, |this| {
                                    this.child(Self::response_switch_button(
                                        ResponseSwitchSegment {
                                            id: "response-formatted",
                                            label: "Formatted",
                                            width: px(64.),
                                            mode: ResponseViewMode::Formatted,
                                        },
                                        formatted,
                                        mode_button_style,
                                        formatted_app,
                                        cx,
                                    ))
                                })
                                .when(!websocket, |this| {
                                    this.child(Self::response_switch_button(
                                        ResponseSwitchSegment {
                                            id: "response-raw",
                                            label: "Raw",
                                            width: px(48.),
                                            mode: ResponseViewMode::Raw,
                                        },
                                        raw,
                                        mode_button_style,
                                        raw_app,
                                        cx,
                                    ))
                                })
                                .child(Self::response_switch_button(
                                    ResponseSwitchSegment {
                                        id: "response-headers",
                                        label: "Headers",
                                        width: px(58.),
                                        mode: ResponseViewMode::Headers,
                                    },
                                    headers,
                                    mode_button_style,
                                    headers_app,
                                    cx,
                                )),
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
                    |this, delta| this.opacity(delta),
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
        .text_center()
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
        .w(px(52.))
        .h(px(24.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(color.opacity(0.1))
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(8.))
        .font_weight(FontWeight::BOLD)
        .text_color(color)
        .child(if method == HttpMethod::Delete {
            "DEL"
        } else {
            method.as_str()
        })
}

fn design_button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Button {
    Button::new(id).label(label)
}

fn design_icon_button(id: impl Into<ElementId>, icon: IconName, label: &'static str) -> Button {
    Button::new(id).child(
        div()
            .flex()
            .items_center()
            .gap(px(6.))
            .child(Icon::new(icon).size(px(12.)))
            .child(label),
    )
}

fn quiet_badge(label: impl Into<SharedString>, cx: &App) -> Div {
    let label: SharedString = label.into();
    div()
        .min_w(px(25.))
        .h(px(22.))
        .px(px(7.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .border_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .font_family(cx.theme().mono_font_family.clone())
        .text_size(px(9.))
        .text_color(cx.theme().muted_foreground)
        .child(label)
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

fn ascii_contains_ignore_case(haystack: &str, lowercase_needle: &str) -> bool {
    if lowercase_needle.is_empty() {
        return true;
    }
    let needle = lowercase_needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

fn request_matches_query(request: &RequestDraft, query: &str) -> bool {
    query.is_empty()
        || ascii_contains_ignore_case(&request.name, query)
        || ascii_contains_ignore_case(&request.url, query)
        || ascii_contains_ignore_case(request.method.as_str(), query)
}

fn history_entry_matches_query(entry: &HistoryEntry, query: &str) -> bool {
    format!(
        "{} {} {} {} {}",
        entry.request.method.as_str(),
        entry.request.display_name(),
        entry.request.url,
        entry
            .status
            .map(|status| status.to_string())
            .unwrap_or_default(),
        entry.error.as_deref().unwrap_or_default(),
    )
    .to_ascii_lowercase()
    .contains(query)
}

fn folder_matching_request_count(folder: &EnvironmentFolder, query: &str) -> usize {
    folder
        .requests
        .iter()
        .filter(|request| request_matches_query(request, query))
        .count()
        + folder
            .folders
            .iter()
            .map(|folder| folder_matching_request_count(folder, query))
            .sum::<usize>()
}

fn split_history_error(error: &str) -> (String, Option<String>) {
    if let Some((detail, status)) = error.rsplit_once("HTTP error: ") {
        let detail = detail.trim().trim_end_matches(':').trim();
        let status = status.trim();
        if !detail.is_empty() && !status.is_empty() {
            return (status.to_owned(), Some(detail.to_owned()));
        }
    }
    (error.to_owned(), None)
}

fn register_response_languages() {
    static RESPONSE_LANGUAGES: Once = Once::new();
    RESPONSE_LANGUAGES.call_once(|| {
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
    });
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

fn quit_application(_: &QuitApplication, cx: &mut App) {
    cx.quit();
}

#[cfg(target_os = "macos")]
fn macos_app_menu() -> Vec<Menu> {
    vec![Menu {
        name: "Alula".into(),
        items: vec![
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Alula", QuitApplication),
        ],
    }]
}

#[cfg(target_os = "macos")]
fn install_app_menu(cx: &mut App) {
    cx.set_menus(macos_app_menu());
}

#[cfg(not(target_os = "macos"))]
fn install_app_menu(_: &mut App) {}

#[cfg(target_os = "macos")]
fn install_app_icon() {
    use objc2::{AnyThread, MainThreadMarker, rc::Retained};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let icon_bytes = include_bytes!("../assets/app-icon.png");
    let icon_data =
        unsafe { NSData::dataWithBytes_length(icon_bytes.as_ptr().cast(), icon_bytes.len()) };
    let Some(icon): Option<Retained<NSImage>> = NSImage::initWithData(NSImage::alloc(), &icon_data)
    else {
        eprintln!("could not decode bundled Alula app icon");
        return;
    };
    let Some(main_thread) = MainThreadMarker::new() else {
        eprintln!("could not install Alula app icon outside the main thread");
        return;
    };
    unsafe {
        NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&icon));
    }
}

#[cfg(not(target_os = "macos"))]
fn install_app_icon() {}

fn main() -> Result<()> {
    install_tls_crypto_provider();
    let app = Application::new().with_assets(Assets);
    app.run(|cx| {
        gpui_component::init(cx);
        cx.on_action(quit_application);
        install_app_icon();
        // GPUI 0.2 matches font weights between registered faces but does not
        // set a variable font's `wght` axis. Register concrete instances so
        // NORMAL, MEDIUM, SEMIBOLD, and BOLD produce distinct glyphs.
        if let Err(error) = cx.text_system().add_fonts(vec![
            Cow::Borrowed(include_bytes!("../assets/fonts/InterRegular.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/InterMedium.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/InterSemiBold.ttf")),
            Cow::Borrowed(include_bytes!("../assets/fonts/InterBold.ttf")),
        ]) {
            eprintln!("could not load bundled Inter font faces: {error:#}");
        }
        let component_bindings = cx.key_bindings().borrow().bindings().cloned().collect();
        cx.set_global(ComponentKeyBindings(component_bindings));
        let theme_path = config_path();
        let theme_config = AppConfig::load_or_create(&theme_path).unwrap_or_else(|error| {
            eprintln!("could not load theme configuration: {error:#}");
            AppConfig::default()
        });
        if let Err(error) = apply_theme(&theme_config, cx) {
            eprintln!("could not apply theme configuration: {error:#}");
        }
        install_key_bindings(&theme_config, cx);
        install_app_menu(cx);
        let state_paths = StatePaths::beside(&theme_path);
        let persisted = PersistedState::load_startup(&state_paths).unwrap_or_else(|error| {
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
