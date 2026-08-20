#![recursion_limit = "256"]

use alula::{
    AppConfig, EnvironmentStore, HistoryEntry, HistoryStore, HttpExecutor, HttpMethod,
    HttpStreamEvent, KeyValueField, PersistedState, RequestDraft, ResponseBodyCache,
    ResponseSnapshot, SettingsView, StatePaths, Workspace, apply_theme, chunked_fenced_code_blocks,
    config_path, syntax_language,
};
use anyhow::Result;
use gpui::prelude::*;
use gpui::*;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::checkbox::Checkbox;
use gpui_component::dialog::DialogButtonProps;
use gpui_component::group_box::{GroupBox, GroupBoxVariants as _};
use gpui_component::highlighter::{LanguageConfig, LanguageRegistry};
use gpui_component::input::{Input, InputEvent, InputState};
use gpui_component::label::Label;
use gpui_component::menu::{ContextMenuExt as _, PopupMenuItem};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::select::{Select, SelectEvent, SelectItem, SelectState};
use gpui_component::sidebar::{Sidebar, SidebarMenu, SidebarMenuItem};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tag::Tag;
use gpui_component::text::{TextView, TextViewStyle};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, IndexPath, Root, Sizable as _, WindowExt as _,
};
use std::{
    borrow::Cow,
    fs,
    path::PathBuf,
    time::{Duration, SystemTime},
};

/// gpui-components' editable code editor recomputes tree-sitter styles during
/// paint. Large editable request bodies use its multiline mode; response
/// highlighting uses the asynchronous, virtualized TextView path below.
const MAX_INTERACTIVE_SYNTAX_BYTES: usize = 32 * 1024;

/// Live highlighting stays bounded while the complete raw response continues
/// to stream. The completed response replaces this preview asynchronously.
const STREAM_HIGHLIGHT_PREVIEW_BYTES: usize = 256 * 1024;

struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        let bytes: Option<&'static [u8]> = match path {
            "icons/arrow-right.svg" => Some(include_bytes!("../assets/icons/arrow-right.svg")),
            "icons/check.svg" => Some(include_bytes!("../assets/icons/check.svg")),
            "icons/chevron-down.svg" => Some(include_bytes!("../assets/icons/chevron-down.svg")),
            "icons/close.svg" => Some(include_bytes!("../assets/icons/close.svg")),
            "icons/copy.svg" => Some(include_bytes!("../assets/icons/copy.svg")),
            "icons/globe.svg" => Some(include_bytes!("../assets/icons/globe.svg")),
            "icons/loader-circle.svg" => Some(include_bytes!("../assets/icons/loader-circle.svg")),
            "icons/plus.svg" => Some(include_bytes!("../assets/icons/plus.svg")),
            "icons/redo-2.svg" => Some(include_bytes!("../assets/icons/redo-2.svg")),
            "icons/settings.svg" => Some(include_bytes!("../assets/icons/settings.svg")),
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
            "check.svg",
            "chevron-down.svg",
            "close.svg",
            "copy.svg",
            "globe.svg",
            "loader-circle.svg",
            "plus.svg",
            "redo-2.svg",
            "settings.svg",
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkspaceSection {
    Requests,
    Environments,
    History,
}

#[derive(Clone)]
struct PairInputs {
    id: String,
    enabled: bool,
    key: Entity<InputState>,
    value: Entity<InputState>,
}

impl PairInputs {
    fn from_field(field: &KeyValueField, window: &mut Window, cx: &mut Context<AlulaApp>) -> Self {
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Key")
                .default_value(field.key.clone())
        });
        let value = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Value")
                .default_value(field.value.clone())
        });
        for input in [&key, &value] {
            cx.subscribe(input, |this, _, event: &InputEvent, _| {
                if matches!(event, InputEvent::Change) {
                    this.persistence_dirty = true;
                }
            })
            .detach();
        }
        Self {
            id: field.id.clone(),
            enabled: field.enabled,
            key,
            value,
        }
    }

    fn empty(window: &mut Window, cx: &mut Context<AlulaApp>) -> Self {
        Self::from_field(&KeyValueField::empty(), window, cx)
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
}

struct ResponseEditors {
    formatted_text: SharedString,
    formatted_markdown: SharedString,
    formatted_published: bool,
    formatted_ready: bool,
    stream_content_type: Option<String>,
    stream_language: Option<&'static str>,
    highlighted_stream_bytes: usize,
    raw: Entity<InputState>,
    raw_text: SharedString,
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
            formatted_published: false,
            formatted_ready: false,
            stream_content_type: None,
            stream_language: None,
            highlighted_stream_bytes: 0,
            raw: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .soft_wrap(false)
                    .default_value(raw_text.clone())
            }),
            raw_text,
            complete: true,
        }
    }

    fn streaming(
        content_type: Option<String>,
        window: &mut Window,
        cx: &mut Context<AlulaApp>,
    ) -> Self {
        let raw_text = SharedString::default();
        Self {
            formatted_text: SharedString::default(),
            formatted_markdown: SharedString::default(),
            formatted_published: false,
            formatted_ready: false,
            stream_content_type: content_type,
            stream_language: None,
            highlighted_stream_bytes: 0,
            raw: cx.new(|cx| {
                InputState::new(window, cx)
                    .multi_line(true)
                    .soft_wrap(false)
            }),
            raw_text,
            complete: false,
        }
    }

    fn append_stream_chunk(&mut self, text: &str, complete_body: &str) {
        if self.highlighted_stream_bytes >= STREAM_HIGHLIGHT_PREVIEW_BYTES || text.is_empty() {
            return;
        }

        let language = *self.stream_language.get_or_insert_with(|| {
            let detected = syntax_language(self.stream_content_type.as_deref(), complete_body);
            if detected == "text" {
                match complete_body.trim_start().as_bytes().first() {
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
        let mut markdown = self.formatted_markdown.to_string();
        if !markdown.is_empty() && !fragment.is_empty() {
            markdown.push_str("\n\n");
        }
        markdown.push_str(&fragment);
        self.formatted_markdown = SharedString::from(markdown);
        self.highlighted_stream_bytes += end;
        self.formatted_ready = false;
    }

    fn finish(&mut self, cache: ResponseBodyCache) {
        self.formatted_text = SharedString::from(cache.formatted.display.text);
        self.formatted_markdown = SharedString::from(cache.formatted.markdown);
        self.raw_text = SharedString::from(cache.raw.text);
        self.formatted_ready = false;
        self.complete = true;
    }
}

impl RequestTab {
    fn new(mut draft: RequestDraft, window: &mut Window, cx: &mut Context<AlulaApp>) -> Self {
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
                    this.persistence_dirty = true;
                    cx.notify();
                }
            },
        )
        .detach();
        let url = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://api.example.com/v1/resource")
                .default_value(draft.url.clone())
        });
        let body = cx.new(|cx| {
            let state = InputState::new(window, cx);
            let state = if draft.body.len() <= MAX_INTERACTIVE_SYNTAX_BYTES {
                state.code_editor("json").line_number(false)
            } else {
                state.multi_line(true)
            };
            state.soft_wrap(false).default_value(draft.body.clone())
        });
        for input in [&url, &body] {
            cx.subscribe(input, |this, _, event: &InputEvent, _| {
                if matches!(event, InputEvent::Change) {
                    this.persistence_dirty = true;
                }
            })
            .detach();
        }
        let parameters = draft
            .parameters
            .iter()
            .map(|field| PairInputs::from_field(field, window, cx))
            .collect();
        let headers = draft
            .headers
            .iter()
            .map(|field| PairInputs::from_field(field, window, cx))
            .collect();
        // Input entities are the canonical editable state. Retaining the imported
        // payload and field vectors in the lightweight tab metadata doubles memory.
        draft.url.clear();
        draft.parameters.clear();
        draft.headers.clear();
        draft.body.clear();
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
}

struct AlulaApp {
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
    persistence_dirty: bool,
    environment_name: Entity<InputState>,
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
        let tabs = persisted
            .workspace
            .requests
            .into_iter()
            .map(|request| RequestTab::new(request, window, cx))
            .collect();
        let app = Self {
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
            persistence_dirty: false,
            environment_name: cx
                .new(|cx| InputState::new(window, cx).placeholder("Production, Staging, Local…")),
        };
        Self::watch_theme_file(cx);
        Self::watch_persistence(cx);
        app
    }

    fn workspace_snapshot(&self, cx: &App) -> Workspace {
        let requests = self.tabs.iter().map(|tab| tab.snapshot(cx)).collect();
        Workspace {
            requests,
            active_request_id: self.tabs[self.active_tab].draft.id.clone(),
        }
    }

    fn watch_persistence(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(400)).await;
                let state = match this.update(cx, |this, cx| {
                    if !this.persistence_dirty {
                        return None;
                    }
                    this.persistence_dirty = false;
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

    fn watch_theme_file(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_secs(1)).await;
                if this
                    .update(cx, |this, cx| {
                        let modified = fs::metadata(&this.theme_path)
                            .and_then(|value| value.modified())
                            .ok();
                        if modified.is_none() || modified == this.theme_modified {
                            return;
                        }
                        this.theme_modified = modified;
                        if let Ok(config) = AppConfig::load(&this.theme_path) {
                            this.theme_config = config.clone();
                            let _ = apply_theme(&config, cx);
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
        let save = settings.clone();
        let restore = settings.clone();
        let app = cx.entity();
        window.open_dialog(cx, move |dialog, _, _| {
            dialog
                .title(Label::new("Settings").font_weight(FontWeight::SEMIBOLD))
                .w(px(860.))
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
        }
        cx.notify();
    }

    fn add_request(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.tabs
            .push(RequestTab::new(RequestDraft::default(), window, cx));
        self.active_tab = self.tabs.len() - 1;
        self.request_tabs_scroll.scroll_to_item(self.active_tab);
        self.workspace_section = WorkspaceSection::Requests;
        self.persistence_dirty = true;
        cx.notify();
    }

    fn close_request(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.tabs.len() == 1 {
            return;
        }
        self.tabs.remove(index);
        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
        self.persistence_dirty = true;
        cx.notify();
    }

    fn select_workspace_section(&mut self, section: WorkspaceSection, cx: &mut Context<Self>) {
        self.workspace_section = section;
        cx.notify();
    }

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
                            this.persistence_dirty = true;
                            cx.notify();
                        });
                        true
                    }
                })
        });
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
            self.persistence_dirty = true;
            cx.notify();
        }
    }

    fn remove_request_from_environment(&mut self, request_id: &str, cx: &mut Context<Self>) {
        if self.environments.remove_request(request_id) {
            self.persistence_dirty = true;
            cx.notify();
        }
    }

    fn open_saved_request(
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
        self.workspace_section = WorkspaceSection::Requests;
        self.request_tabs_scroll.scroll_to_item(self.active_tab);
        self.persistence_dirty = true;
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
                _ => editors.raw_text.to_string(),
            })
            .unwrap_or_else(|| response.body.clone());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    fn select_section(&mut self, section: EditorSection, cx: &mut Context<Self>) {
        self.tabs[self.active_tab].section = section;
        cx.notify();
    }

    fn add_pair(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pair = PairInputs::empty(window, cx);
        let tab = &mut self.tabs[self.active_tab];
        match tab.section {
            EditorSection::Parameters => tab.parameters.push(pair),
            EditorSection::Headers => tab.headers.push(pair),
            EditorSection::Body => {}
        }
        self.persistence_dirty = true;
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
        self.persistence_dirty = true;
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
        self.persistence_dirty = true;
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
                response.body.push_str(&text);
                response.size_bytes = total_bytes;
                if let Some(editors) = tab.response_editors.as_mut() {
                    editors.append_stream_chunk(&text, &response.body);
                    let raw_text = SharedString::from(response.body.clone());
                    editors.raw_text = raw_text.clone();
                    editors.formatted_text = raw_text.clone();
                    editors.raw.update(cx, |raw, cx| {
                        raw.set_value(raw_text, window, cx);
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
        cx.notify();
    }

    fn send_request(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let index = self.active_tab;
        if self.tabs[index].sending {
            return;
        }
        let request = self.tabs[index].snapshot(cx);
        let history_request = request.clone();
        let request_id = request.id.clone();
        self.tabs[index].title = SharedString::from(request.display_name());
        self.tabs[index].sending = true;
        self.tabs[index].response = None;
        self.tabs[index].response_editors = None;
        self.tabs[index].error = None;
        self.tabs[index].response_revision = self.tabs[index].response_revision.wrapping_add(1);
        self.persistence_dirty = true;
        let response_revision = self.tabs[index].response_revision;
        cx.notify();

        let (event_tx, event_rx) = smol::channel::unbounded();
        let task = cx.background_executor().spawn(async move {
            HttpExecutor::execute_streaming(&request, |event| {
                let _ = event_tx.try_send(event);
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
                (cache, metadata)
            })
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = event_rx.recv().await {
                let _ = cx.update(|window, cx| {
                    let _ = this.update(cx, |this, cx| {
                        this.apply_http_stream_event(
                            &request_id,
                            response_revision,
                            event,
                            window,
                            cx,
                        );
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
                        Ok((cache, response)) => {
                            this.history
                                .push(HistoryEntry::success(history_request.clone(), &response));
                            if let Some(tab) = this.tabs.iter_mut().find(|tab| {
                                tab.draft.id == request_id
                                    && tab.response_revision == response_revision
                            }) {
                                tab.sending = false;
                                if let Some(editors) = tab.response_editors.as_mut() {
                                    editors.finish(cache);
                                } else {
                                    tab.response_editors =
                                        Some(ResponseEditors::new(cache, window, cx));
                                }
                            }
                        }
                        Err(error) => {
                            let message = format!("{error:#}");
                            this.history.push(HistoryEntry::failure(
                                history_request.clone(),
                                message.clone(),
                            ));
                            if let Some(tab) = this.tabs.iter_mut().find(|tab| {
                                tab.draft.id == request_id
                                    && tab.response_revision == response_revision
                            }) {
                                tab.sending = false;
                                tab.error = Some(message);
                            }
                        }
                    }
                    this.persistence_dirty = true;
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn render_top_bar(&self, cx: &mut Context<Self>) -> Div {
        div()
            .h(px(58.))
            .flex()
            .items_center()
            .justify_between()
            .px_4()
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .bg(cx.theme().title_bar)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .size(px(30.))
                            .rounded_lg()
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(cx.theme().primary)
                            .text_color(cx.theme().primary_foreground)
                            .font_weight(FontWeight::BOLD)
                            .child("A"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(Label::new("Alula").font_weight(FontWeight::SEMIBOLD))
                            .child(
                                Label::new("Agent-ready HTTP studio")
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(Tag::success().small().rounded_full().child("MCP ready"))
                    .child(
                        Button::new("settings")
                            .ghost()
                            .small()
                            .icon(IconName::Settings)
                            .label("Settings")
                            .on_click(cx.listener(Self::open_settings)),
                    ),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> Sidebar<SidebarMenu> {
        let app = cx.entity();
        let requests_app = app.clone();
        let environments_app = app.clone();
        let history_app = app.clone();
        Sidebar::left()
            .collapsible(false)
            .w(px(224.))
            .header(
                Label::new("WORKSPACE")
                    .text_xs()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                SidebarMenu::new()
                    .child(
                        SidebarMenuItem::new("Requests")
                            .icon(IconName::SquareTerminal)
                            .active(self.workspace_section == WorkspaceSection::Requests)
                            .on_click(move |_, _, cx| {
                                requests_app.update(cx, |this, cx| {
                                    this.select_workspace_section(WorkspaceSection::Requests, cx)
                                });
                            }),
                    )
                    .child(
                        SidebarMenuItem::new("Environments")
                            .icon(IconName::Globe)
                            .active(self.workspace_section == WorkspaceSection::Environments)
                            .suffix(
                                Tag::secondary()
                                    .small()
                                    .rounded_full()
                                    .child(self.environments.environments.len().to_string()),
                            )
                            .on_click(move |_, _, cx| {
                                environments_app.update(cx, |this, cx| {
                                    this.select_workspace_section(
                                        WorkspaceSection::Environments,
                                        cx,
                                    )
                                });
                            }),
                    )
                    .child(
                        SidebarMenuItem::new("History")
                            .icon(IconName::Redo2)
                            .active(self.workspace_section == WorkspaceSection::History)
                            .suffix(
                                Tag::secondary()
                                    .small()
                                    .rounded_full()
                                    .child(self.history.entries.len().to_string()),
                            )
                            .on_click(move |_, _, cx| {
                                history_app.update(cx, |this, cx| {
                                    this.select_workspace_section(WorkspaceSection::History, cx)
                                });
                            }),
                    ),
            )
            .footer(
                GroupBox::new()
                    .fill()
                    .title(Label::new("Agent access").font_weight(FontWeight::SEMIBOLD))
                    .child(
                        Label::new(
                            "Typed MCP tools can inspect and edit requests without UI automation.",
                        )
                        .text_xs()
                        .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        Button::new("agent-contracts")
                            .link()
                            .small()
                            .label(format!("{} MCP tool contracts", alula::mcp_tools().len())),
                    ),
            )
    }

    fn render_request_tabs(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let tab_app = app.clone();
        let tab_scroll = self.request_tabs_scroll.clone();
        let mut bar = TabBar::new("request-tabs")
            .min_w_0()
            .max_w_full()
            .px_2()
            .track_scroll(&self.request_tabs_scroll)
            .selected_index(self.active_tab)
            .on_click(move |index, _, cx| {
                tab_scroll.scroll_to_item(*index);
                tab_app.update(cx, |this, cx| {
                    if *index < this.tabs.len() {
                        this.active_tab = *index;
                        this.persistence_dirty = true;
                        cx.notify();
                    }
                });
            });
        for (index, tab) in self.tabs.iter().enumerate() {
            let method = tab.draft.method.as_str();
            let label = tab.title.clone();
            let request_id = tab.draft.id.clone();
            let assigned_environment = self
                .environments
                .environment_for_request(&request_id)
                .map(|environment| environment.id.clone());
            let environments = self
                .environments
                .environments
                .iter()
                .map(|environment| (environment.id.clone(), environment.name.clone()))
                .collect::<Vec<_>>();
            let menu_app = app.clone();
            let close_app = app.clone();
            let tab_content = div()
                .size_full()
                .min_w_0()
                .flex()
                .items_center()
                .gap_2()
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
                .context_menu(move |mut menu, _, _| {
                    menu = menu.label("Add to environment");
                    if environments.is_empty() {
                        menu = menu.item(PopupMenuItem::new("No environments yet").disabled(true));
                    } else {
                        for (environment_id, name) in &environments {
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
            bar = bar.child(Tab::new().w(px(210.)).child(tab_content));
        }
        div()
            .w_full()
            .min_w_0()
            .flex()
            .items_center()
            .border_b_1()
            .border_color(cx.theme().border)
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

    fn render_environments(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let mut content = div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .p_5()
            .flex()
            .flex_col()
            .gap_4();
        if self.environments.environments.is_empty() {
            content = content.child(empty_state(
                "No environments yet",
                "Create one, then right-click a request tab to add it",
                cx,
            ));
        } else {
            for environment in &self.environments.environments {
                let mut requests = div().flex().flex_col().gap_1();
                if environment.requests.is_empty() {
                    requests = requests.child(
                        Label::new("Right-click a request tab to add it here")
                            .text_sm()
                            .text_color(cx.theme().muted_foreground),
                    );
                } else {
                    for request in &environment.requests {
                        let open_app = app.clone();
                        let open_request = request.clone();
                        requests = requests.child(
                            div()
                                .w_full()
                                .h(px(42.))
                                .px_3()
                                .flex()
                                .items_center()
                                .gap_3()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().muted.opacity(0.45))
                                .child(
                                    div()
                                        .w(px(54.))
                                        .flex_shrink_0()
                                        .text_xs()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(method_color(request.method, cx))
                                        .child(request.method.as_str()),
                                )
                                .child(
                                    Label::new(request.display_name())
                                        .flex_1()
                                        .min_w_0()
                                        .truncate(),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "open-environment-request-{}",
                                        request.id
                                    )))
                                    .outline()
                                    .small()
                                    .label("Open")
                                    .on_click(
                                        move |_, window, cx| {
                                            open_app.update(cx, |this, cx| {
                                                this.open_saved_request(
                                                    open_request.clone(),
                                                    window,
                                                    cx,
                                                )
                                            });
                                        },
                                    ),
                                ),
                        );
                    }
                }
                content = content.child(
                    GroupBox::new()
                        .fill()
                        .title(
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .child(
                                    Label::new(environment.name.clone())
                                        .font_weight(FontWeight::SEMIBOLD),
                                )
                                .child(
                                    Tag::secondary()
                                        .small()
                                        .rounded_full()
                                        .child(environment.requests.len().to_string()),
                                ),
                        )
                        .child(requests),
                );
            }
        }
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(64.))
                    .px_5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(
                                Label::new("Environments")
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_lg(),
                            )
                            .child(
                                Label::new("Persistent groups for organizing request snapshots")
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground),
                            ),
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
            .child(content)
    }

    fn render_history(&self, cx: &mut Context<Self>) -> Div {
        let app = cx.entity();
        let mut content = div()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .p_5()
            .flex()
            .flex_col()
            .gap_2();
        if self.history.entries.is_empty() {
            content = content.child(empty_state(
                "No request history yet",
                "Each completed or failed send is recorded independently of tabs",
                cx,
            ));
        } else {
            for entry in &self.history.entries {
                let open_app = app.clone();
                let open_request = entry.request.clone();
                let outcome = if let Some(status) = entry.status {
                    format!(
                        "{} · {} ms · {}",
                        status,
                        entry.elapsed_ms.unwrap_or_default(),
                        format_size(entry.size_bytes.unwrap_or_default())
                    )
                } else {
                    entry
                        .error
                        .clone()
                        .unwrap_or_else(|| "Request failed".into())
                };
                content = content.child(
                    div()
                        .w_full()
                        .min_h(px(58.))
                        .px_3()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded(cx.theme().radius)
                        .border_1()
                        .border_color(cx.theme().border)
                        .child(
                            div()
                                .w(px(54.))
                                .flex_shrink_0()
                                .text_xs()
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(method_color(entry.request.method, cx))
                                .child(entry.request.method.as_str()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .child(
                                    Label::new(entry.request.display_name())
                                        .truncate()
                                        .font_weight(FontWeight::SEMIBOLD),
                                )
                                .child(
                                    Label::new(format!(
                                        "{} · {}",
                                        outcome,
                                        relative_history_time(entry.sent_at_unix_ms)
                                    ))
                                    .truncate()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground),
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
                                open_app.update(cx, |this, cx| {
                                    this.open_saved_request(open_request.clone(), window, cx)
                                });
                            }),
                        ),
                );
            }
        }
        div()
            .size_full()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(64.))
                    .px_5()
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
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
                    ),
            )
            .child(content)
    }

    fn render_request_builder(&self, cx: &mut Context<Self>) -> Div {
        let tab = &self.tabs[self.active_tab];
        let method = tab.method.clone();
        let url = tab.url.clone();
        let method_value = tab.draft.method;
        div()
            .w_full()
            .flex_shrink_0()
            .px_5()
            .pt_5()
            .pb_4()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div().w(px(112.)).h(px(44.)).flex_shrink_0().child(
                            Select::new(&method)
                                .large()
                                .text_color(method_color(method_value, cx))
                                .menu_width(px(144.)),
                        ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(44.))
                            .child(Input::new(&url).large().w_full()),
                    )
                    .child(
                        div().w(px(120.)).h(px(44.)).flex_shrink_0().child(
                            Button::new("send")
                                .primary()
                                .large()
                                .size_full()
                                .justify_center()
                                .icon(IconName::ArrowRight)
                                .disabled(tab.sending)
                                .loading(tab.sending)
                                .label(if tab.sending { "Sending" } else { "Send" })
                                .on_click(cx.listener(Self::send_request)),
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
            .min_h(px(240.))
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
            .flex()
            .flex_col()
            .child(tabs.px_4().pt_1())
            .child(match current {
                EditorSection::Body => self.render_body(),
                EditorSection::Parameters | EditorSection::Headers => self.render_pairs(cx),
            })
    }

    fn render_body(&self) -> Div {
        let body = self.tabs[self.active_tab].body.clone();
        div()
            .flex_1()
            .min_h(px(200.))
            .p_3()
            .child(Input::new(&body).h_full())
    }

    fn render_pairs(&self, cx: &mut Context<Self>) -> Div {
        let tab = &self.tabs[self.active_tab];
        let app = cx.entity();
        let pairs: &[PairInputs] = match tab.section {
            EditorSection::Parameters => &tab.parameters,
            EditorSection::Headers => &tab.headers,
            EditorSection::Body => &[],
        };
        let mut list = div().w_full().p_3().flex().flex_col().gap_2();
        for (index, pair) in pairs.iter().enumerate() {
            let key = pair.key.clone();
            let value = pair.value.clone();
            let enabled = pair.enabled;
            let checkbox_app = app.clone();
            list = list.child(
                div()
                    .w_full()
                    .h(px(36.))
                    .flex()
                    .items_center()
                    .gap_2()
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
                            .child(Input::new(&key).small().w_full().disabled(!enabled)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(32.))
                            .flex()
                            .items_center()
                            .child(Input::new(&value).small().w_full().disabled(!enabled)),
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
        list.child(
            div().mt_1().flex().child(
                Button::new("add-pair")
                    .ghost()
                    .small()
                    .icon(IconName::Plus)
                    .label("Add row")
                    .on_click(cx.listener(|this, _, window, cx| this.add_pair(window, cx))),
            ),
        )
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

    fn render_response(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let (formatted_view, keepalive_views) = self.build_formatted_response_views(window, cx);
        let tab = &self.tabs[self.active_tab];
        let header = div()
            .h(px(52.))
            .px_4()
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(Label::new("Response").font_weight(FontWeight::SEMIBOLD))
            .when(tab.response.is_some(), |this| {
                let formatted = tab.response_view == ResponseViewMode::Formatted;
                let app = cx.entity();
                this.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            TabBar::new("response-view-mode")
                                .segmented()
                                .small()
                                .w(px(184.))
                                .selected_index(usize::from(!formatted))
                                .child(Tab::new().w(px(88.)).label("Formatted"))
                                .child(Tab::new().w(px(88.)).label("Raw"))
                                .on_click(move |index, _, cx| {
                                    let mode = if *index == 0 {
                                        ResponseViewMode::Formatted
                                    } else {
                                        ResponseViewMode::Raw
                                    };
                                    app.update(cx, |this, cx| this.set_response_view(mode, cx));
                                }),
                        )
                        .child(
                            Button::new("copy-response")
                                .outline()
                                .small()
                                .icon(IconName::Copy)
                                .label("Copy")
                                .on_click(cx.listener(Self::copy_response)),
                        ),
                )
            });

        let content = if tab.sending && tab.response.is_none() {
            empty_state("Sending request…", "Waiting for response headers", cx)
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
            div()
                .flex_1()
                .min_h_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .h(px(40.))
                        .px_4()
                        .flex()
                        .items_center()
                        .gap_4()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .text_sm()
                        .child(
                            div()
                                .text_color(status_color)
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(format!("{} {}", response.status, response.status_text)),
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
                        .when(tab.sending, |this| {
                            this.child(div().text_color(cx.theme().primary).child("Streaming…"))
                        }),
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
                        }),
                )
        } else {
            empty_state(
                "No response yet",
                "Send the request to inspect status, timing, headers, and body",
                cx,
            )
        };

        div()
            .flex_1()
            .min_h_0()
            .mx_5()
            .mb_5()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().background)
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
        let main_content = match self.workspace_section {
            WorkspaceSection::Requests => div()
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
                        .child(self.render_request_builder(cx))
                        .child(self.render_response(window, cx)),
                ),
            WorkspaceSection::Environments => self.render_environments(cx),
            WorkspaceSection::History => self.render_history(cx),
        };
        div()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .flex()
            .flex_col()
            .child(self.render_top_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .child(self.render_sidebar(cx))
                    .child(div().flex_1().min_w_0().min_h_0().child(main_content)),
            )
            .children(sheet_layer)
            .children(dialog_layer)
            .children(notification_layer)
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

fn method_color(method: HttpMethod, cx: &App) -> Hsla {
    match method {
        HttpMethod::Get => cx.theme().green,
        HttpMethod::Post => cx.theme().blue,
        HttpMethod::Put => cx.theme().yellow,
        HttpMethod::Patch => cx.theme().magenta,
        HttpMethod::Delete => cx.theme().red,
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

fn format_size(bytes: usize) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
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
        _ => format!("{} d ago", elapsed_seconds / 86_400),
    }
}

fn main() -> Result<()> {
    let app = Application::new().with_assets(Assets);
    app.run(|cx| {
        gpui_component::init(cx);
        register_response_languages();
        let theme_path = config_path();
        let theme_config = AppConfig::load_or_create(&theme_path).unwrap_or_else(|error| {
            eprintln!("could not load theme configuration: {error:#}");
            AppConfig::default()
        });
        if let Err(error) = apply_theme(&theme_config, cx) {
            eprintln!("could not apply theme configuration: {error:#}");
        }
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
                let view = cx.new(|cx| {
                    AlulaApp::new(
                        theme_config.clone(),
                        theme_path.clone(),
                        persisted.clone(),
                        window,
                        cx,
                    )
                });
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("failed to open Alula window");
        cx.activate(true);
    });
    Ok(())
}
