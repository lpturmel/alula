use std::path::PathBuf;

use gpui::prelude::*;
use gpui::*;
use gpui_component::{
    ActiveTheme as _, Colorize as _, Sizable as _, Theme,
    button::{Button, ButtonVariants as _},
    color_picker::{ColorPicker, ColorPickerEvent, ColorPickerState},
    input::{Input, InputEvent, InputState},
    label::Label,
    scroll::ScrollableElement as _,
    switch::Switch,
    tab::{Tab, TabBar},
    text::TextView,
};

use crate::{AppConfig, ThemeModePreference, import_editor_theme, save_config_location};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    General,
    Agent,
    Theme,
}

impl SettingsPage {
    const ALL: [Self; 3] = [Self::General, Self::Agent, Self::Theme];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Agent => "Agent",
            Self::Theme => "Theme",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThemeSection {
    Interface,
    Syntax,
    Import,
}

impl ThemeSection {
    const ALL: [Self; 3] = [Self::Interface, Self::Syntax, Self::Import];

    fn label(self) -> &'static str {
        match self {
            Self::Interface => "Interface",
            Self::Syntax => "Syntax",
            Self::Import => "Import & agents",
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ColorKey {
    Background,
    Surface,
    Foreground,
    MutedForeground,
    Border,
    Accent,
    AccentForeground,
    Primary,
    PrimaryForeground,
    Sidebar,
    SidebarForeground,
    Selection,
    Success,
    Warning,
    Danger,
    Info,
    EditorBackground,
    EditorForeground,
    Comment,
    Keyword,
    String,
    Number,
    Function,
    Type,
    Variable,
    Property,
    Tag,
    Attribute,
    Boolean,
    Constant,
    Punctuation,
    Operator,
}

impl ColorKey {
    const INTERFACE: [Self; 16] = [
        Self::Background,
        Self::Surface,
        Self::Foreground,
        Self::MutedForeground,
        Self::Border,
        Self::Accent,
        Self::AccentForeground,
        Self::Primary,
        Self::PrimaryForeground,
        Self::Sidebar,
        Self::SidebarForeground,
        Self::Selection,
        Self::Success,
        Self::Warning,
        Self::Danger,
        Self::Info,
    ];
    const SYNTAX: [Self; 16] = [
        Self::EditorBackground,
        Self::EditorForeground,
        Self::Comment,
        Self::Keyword,
        Self::String,
        Self::Number,
        Self::Function,
        Self::Type,
        Self::Variable,
        Self::Property,
        Self::Tag,
        Self::Attribute,
        Self::Boolean,
        Self::Constant,
        Self::Punctuation,
        Self::Operator,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Background => "Background",
            Self::Surface => "Surface",
            Self::Foreground => "Foreground",
            Self::MutedForeground => "Muted foreground",
            Self::Border => "Border",
            Self::Accent => "Accent",
            Self::AccentForeground => "Accent foreground",
            Self::Primary => "Primary",
            Self::PrimaryForeground => "Primary foreground",
            Self::Sidebar => "Sidebar",
            Self::SidebarForeground => "Sidebar foreground",
            Self::Selection => "Selection",
            Self::Success => "Success",
            Self::Warning => "Warning",
            Self::Danger => "Danger",
            Self::Info => "Info",
            Self::EditorBackground => "Editor background",
            Self::EditorForeground => "Editor foreground",
            Self::Comment => "Comment",
            Self::Keyword => "Keyword",
            Self::String => "String",
            Self::Number => "Number",
            Self::Function => "Function",
            Self::Type => "Type",
            Self::Variable => "Variable",
            Self::Property => "Property",
            Self::Tag => "Tag",
            Self::Attribute => "Attribute",
            Self::Boolean => "Boolean",
            Self::Constant => "Constant",
            Self::Punctuation => "Punctuation",
            Self::Operator => "Operator",
        }
    }

    fn get<'a>(self, config: &'a AppConfig) -> &'a str {
        let c = &config.theme.colors;
        let s = &config.syntax;
        match self {
            Self::Background => &c.background,
            Self::Surface => &c.surface,
            Self::Foreground => &c.foreground,
            Self::MutedForeground => &c.muted_foreground,
            Self::Border => &c.border,
            Self::Accent => &c.accent,
            Self::AccentForeground => &c.accent_foreground,
            Self::Primary => &c.primary,
            Self::PrimaryForeground => &c.primary_foreground,
            Self::Sidebar => &c.sidebar,
            Self::SidebarForeground => &c.sidebar_foreground,
            Self::Selection => &c.selection,
            Self::Success => &c.success,
            Self::Warning => &c.warning,
            Self::Danger => &c.danger,
            Self::Info => &c.info,
            Self::EditorBackground => &s.editor_background,
            Self::EditorForeground => &s.editor_foreground,
            Self::Comment => &s.comment,
            Self::Keyword => &s.keyword,
            Self::String => &s.string,
            Self::Number => &s.number,
            Self::Function => &s.function,
            Self::Type => &s.type_color,
            Self::Variable => &s.variable,
            Self::Property => &s.property,
            Self::Tag => &s.tag,
            Self::Attribute => &s.attribute,
            Self::Boolean => &s.boolean,
            Self::Constant => &s.constant,
            Self::Punctuation => &s.punctuation,
            Self::Operator => &s.operator,
        }
    }

    fn set(self, config: &mut AppConfig, value: String) {
        let c = &mut config.theme.colors;
        let s = &mut config.syntax;
        *match self {
            Self::Background => &mut c.background,
            Self::Surface => &mut c.surface,
            Self::Foreground => &mut c.foreground,
            Self::MutedForeground => &mut c.muted_foreground,
            Self::Border => &mut c.border,
            Self::Accent => &mut c.accent,
            Self::AccentForeground => &mut c.accent_foreground,
            Self::Primary => &mut c.primary,
            Self::PrimaryForeground => &mut c.primary_foreground,
            Self::Sidebar => &mut c.sidebar,
            Self::SidebarForeground => &mut c.sidebar_foreground,
            Self::Selection => &mut c.selection,
            Self::Success => &mut c.success,
            Self::Warning => &mut c.warning,
            Self::Danger => &mut c.danger,
            Self::Info => &mut c.info,
            Self::EditorBackground => &mut s.editor_background,
            Self::EditorForeground => &mut s.editor_foreground,
            Self::Comment => &mut s.comment,
            Self::Keyword => &mut s.keyword,
            Self::String => &mut s.string,
            Self::Number => &mut s.number,
            Self::Function => &mut s.function,
            Self::Type => &mut s.type_color,
            Self::Variable => &mut s.variable,
            Self::Property => &mut s.property,
            Self::Tag => &mut s.tag,
            Self::Attribute => &mut s.attribute,
            Self::Boolean => &mut s.boolean,
            Self::Constant => &mut s.constant,
            Self::Punctuation => &mut s.punctuation,
            Self::Operator => &mut s.operator,
        } = value;
    }
}

pub fn apply_theme(config: &AppConfig, cx: &mut App) -> anyhow::Result<()> {
    let theme = config.to_gpui_theme()?;
    Theme::global_mut(cx).apply_config(&theme);
    Theme::change(config.theme.mode.gpui(), None, cx);
    cx.refresh_windows();
    Ok(())
}

pub struct SettingsView {
    original: AppConfig,
    working: AppConfig,
    path: PathBuf,
    page: SettingsPage,
    theme_section: ThemeSection,
    config_file: Entity<InputState>,
    agent_port: Entity<InputState>,
    name: Entity<InputState>,
    radius: Entity<InputState>,
    radius_large: Entity<InputState>,
    colors: Vec<(ColorKey, Entity<ColorPickerState>)>,
    status: Option<(bool, String)>,
}

impl SettingsView {
    pub fn new(
        mut config: AppConfig,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        if config.application.config_path.trim().is_empty() {
            config.application.config_path = path.display().to_string();
        }
        let name =
            cx.new(|cx| InputState::new(window, cx).default_value(config.theme.name.clone()));
        let radius =
            cx.new(|cx| InputState::new(window, cx).default_value(config.theme.radius.to_string()));
        let radius_large = cx.new(|cx| {
            InputState::new(window, cx).default_value(config.theme.radius_large.to_string())
        });
        let config_file =
            cx.new(|cx| InputState::new(window, cx).default_value(path.display().to_string()));
        let agent_port =
            cx.new(|cx| InputState::new(window, cx).default_value(config.agent.port.to_string()));

        cx.subscribe(&name, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.working.theme.name = state.read(cx).value().to_string();
                this.preview(cx);
            }
        })
        .detach();
        cx.subscribe(&radius, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && let Ok(value) = state.read(cx).value().parse()
            {
                this.working.theme.radius = value;
                this.preview(cx);
            }
        })
        .detach();
        cx.subscribe(&radius_large, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && let Ok(value) = state.read(cx).value().parse()
            {
                this.working.theme.radius_large = value;
                this.preview(cx);
            }
        })
        .detach();
        cx.subscribe(&config_file, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.working.application.config_path = state.read(cx).value().to_string();
            }
        })
        .detach();
        cx.subscribe(&agent_port, |this, state, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change)
                && let Ok(port) = state.read(cx).value().parse()
            {
                this.working.agent.port = port;
            }
        })
        .detach();

        let mut colors = Vec::new();
        for key in ColorKey::INTERFACE.into_iter().chain(ColorKey::SYNTAX) {
            let value = Hsla::parse_hex(key.get(&config)).expect("validated config color");
            let state = cx.new(|cx| ColorPickerState::new(window, cx).default_value(value));
            cx.subscribe(&state, move |this, _, event: &ColorPickerEvent, cx| {
                let ColorPickerEvent::Change(Some(color)) = event else {
                    return;
                };
                key.set(&mut this.working, color.to_hex());
                this.preview(cx);
            })
            .detach();
            colors.push((key, state));
        }

        Self {
            original: config.clone(),
            working: config,
            path,
            page: SettingsPage::General,
            theme_section: ThemeSection::Interface,
            config_file,
            agent_port,
            name,
            radius,
            radius_large,
            colors,
            status: None,
        }
    }

    pub fn save(&mut self, cx: &mut App) -> bool {
        let configured_path = self.config_file.read(cx).value().to_string();
        let path = PathBuf::from(configured_path.trim());
        if path.as_os_str().is_empty() {
            self.status = Some((false, "Configuration file path cannot be empty.".into()));
            cx.refresh_windows();
            return false;
        }
        let port = match self.agent_port.read(cx).value().parse::<u16>() {
            Ok(port) if port > 0 => port,
            _ => {
                self.status = Some((false, "Agent port must be between 1 and 65535.".into()));
                cx.refresh_windows();
                return false;
            }
        };
        self.working.application.config_path = path.display().to_string();
        self.working.agent.port = port;
        match self
            .working
            .save(&path)
            .and_then(|_| save_config_location(&path))
        {
            Ok(()) => {
                self.path = path;
                self.original = self.working.clone();
                true
            }
            Err(error) => {
                self.status = Some((false, format!("Could not save settings: {error:#}")));
                cx.refresh_windows();
                false
            }
        }
    }

    pub fn restore(&mut self, cx: &mut App) -> bool {
        let _ = apply_theme(&self.original, cx);
        true
    }

    fn preview(&mut self, cx: &mut App) {
        match apply_theme(&self.working, cx) {
            Ok(()) => self.status = None,
            Err(error) => self.status = Some((false, error.to_string())),
        }
        cx.refresh_windows();
    }

    fn set_mode(&mut self, dark: bool, cx: &mut Context<Self>) {
        self.working.theme.mode = if dark {
            ThemeModePreference::Dark
        } else {
            ThemeModePreference::Light
        };
        self.preview(cx);
        cx.notify();
    }

    fn reset(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let defaults = AppConfig::default();
        self.working.theme = defaults.theme;
        self.working.syntax = defaults.syntax;
        self.sync_controls(window, cx);
        self.preview(cx);
        self.status = Some((true, "Default theme loaded as a live preview.".into()));
        cx.notify();
    }

    fn reload(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        match AppConfig::load(&self.path) {
            Ok(config) => {
                self.working = config;
                self.sync_controls(window, cx);
                self.preview(cx);
                self.status = Some((true, "Reloaded the TOML configuration.".into()));
            }
            Err(error) => self.status = Some((false, format!("Reload failed: {error:#}"))),
        }
        cx.notify();
    }

    fn choose_import(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import theme".into()),
        });
        cx.spawn_in(window, async move |view, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| this.import_path(path, window, cx));
            });
        })
        .detach();
    }

    fn choose_config_file(&mut self, _: &ClickEvent, window: &mut Window, cx: &mut Context<Self>) {
        let current = PathBuf::from(self.config_file.read(cx).value().as_ref());
        let directory = current
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let suggested_name = current
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("config.toml");
        let receiver = cx.prompt_for_new_path(directory, Some(suggested_name));
        cx.spawn_in(window, async move |view, cx| {
            let Ok(Ok(Some(path))) = receiver.await else {
                return;
            };
            let _ = cx.update(|window, cx| {
                let _ = view.update(cx, |this, cx| {
                    this.working.application.config_path = path.display().to_string();
                    this.config_file.update(cx, |state, cx| {
                        state.set_value(path.display().to_string(), window, cx)
                    });
                    this.status = Some((
                        true,
                        "The configuration will move when settings are saved.".into(),
                    ));
                    cx.notify();
                });
            });
        })
        .detach();
    }

    fn import_path(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match import_editor_theme(&path) {
            Ok(mut config) => {
                config.application = self.working.application.clone();
                config.agent = self.working.agent.clone();
                self.working = config;
                self.sync_controls(window, cx);
                self.preview(cx);
                self.status = Some((
                    true,
                    format!("Imported {} as a live preview.", path.display()),
                ));
            }
            Err(error) => self.status = Some((false, format!("Import failed: {error:#}"))),
        }
        cx.notify();
    }

    fn sync_controls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.config_file.update(cx, |state, cx| {
            state.set_value(
                if self.working.application.config_path.is_empty() {
                    self.path.display().to_string()
                } else {
                    self.working.application.config_path.clone()
                },
                window,
                cx,
            )
        });
        self.agent_port.update(cx, |state, cx| {
            state.set_value(self.working.agent.port.to_string(), window, cx)
        });
        self.name.update(cx, |state, cx| {
            state.set_value(self.working.theme.name.clone(), window, cx)
        });
        self.radius.update(cx, |state, cx| {
            state.set_value(self.working.theme.radius.to_string(), window, cx)
        });
        self.radius_large.update(cx, |state, cx| {
            state.set_value(self.working.theme.radius_large.to_string(), window, cx)
        });
        for (key, state) in &self.colors {
            let value = Hsla::parse_hex(key.get(&self.working)).expect("validated imported color");
            state.update(cx, |state, cx| state.set_value(value, window, cx));
        }
    }

    fn render_colors(&self, keys: &[ColorKey], cx: &App) -> Div {
        let mut list = div().flex().flex_col().gap_1();
        for key in keys {
            let Some((_, state)) = self.colors.iter().find(|(candidate, _)| {
                std::mem::discriminant(candidate) == std::mem::discriminant(key)
            }) else {
                continue;
            };
            let hex = state
                .read(cx)
                .value()
                .map(|color| color.to_hex())
                .unwrap_or_default();
            list = list.child(
                div()
                    .h(px(42.))
                    .px_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .rounded(cx.theme().radius)
                    .hover(|this| this.bg(cx.theme().list_hover))
                    .child(Label::new(key.label()).text_sm())
                    .child(ColorPicker::new(state).small().label(hex)),
            );
        }
        list
    }

    fn render_general(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(section_heading(
                "Configuration",
                "Choose where Alula stores the settings shared by the app and its agent bridge.",
            ))
            .child(
                div()
                    .flex()
                    .items_end()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(field("Configuration file", Input::new(&self.config_file))),
                    )
                    .child(
                        Button::new("choose-config-file")
                            .outline()
                            .label("Choose…")
                            .on_click(cx.listener(Self::choose_config_file)),
                    ),
            )
            .child(
                Label::new(format!(
                    "Current file: {}. Leave this unchanged to keep the platform default.",
                    self.path.display()
                ))
                .text_xs()
                .text_color(cx.theme().muted_foreground),
            )
    }

    fn render_agent(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_col()
            .gap_5()
            .child(section_heading(
                "Agent service",
                "Configure the loopback port reserved for Alula's MCP and agent integrations.",
            ))
            .child(
                div()
                    .w(px(280.))
                    .child(field("Agent port", Input::new(&self.agent_port))),
            )
            .child(
                div()
                    .p_4()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .child(
                        Label::new("Use an unprivileged port from 1 to 65535. A changed port takes effect when the agent service next starts.")
                            .text_sm(),
                    ),
            )
    }

    fn render_interface(&self, cx: &mut Context<Self>) -> Div {
        let dark = self.working.theme.mode == ThemeModePreference::Dark;
        let app = cx.entity();
        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .grid()
                    .grid_cols(2)
                    .gap_3()
                    .child(field("Theme name", Input::new(&self.name)))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                Label::new("Appearance")
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground),
                            )
                            .child(
                                Switch::new("theme-mode")
                                    .checked(dark)
                                    .label(if dark { "Dark" } else { "Light" })
                                    .on_click(move |checked, _, cx| {
                                        app.update(cx, |this, cx| this.set_mode(*checked, cx));
                                    }),
                            ),
                    )
                    .child(field("Corner radius", Input::new(&self.radius)))
                    .child(field("Large corner radius", Input::new(&self.radius_large))),
            )
            .child(section_heading(
                "Interface palette",
                "Pick a native palette color or enter an exact hex value in the picker.",
            ))
            .child(self.render_colors(&ColorKey::INTERFACE, cx))
    }

    fn render_syntax(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let preview = "```javascript\n// Live syntax theme preview\nconst request = await fetch(\"https://api.example.com\", {\n  method: \"POST\",\n  body: JSON.stringify({ enabled: true, retries: 3 })\n});\n```";
        let preview_id = ("theme-syntax-preview", self.working.fingerprint());
        div()
            .flex()
            .gap_5()
            .child(
                div()
                    .w(px(360.))
                    .flex_shrink_0()
                    .child(self.render_colors(&ColorKey::SYNTAX, cx)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(section_heading(
                        "Live preview",
                        "Syntax highlighting updates immediately with the selected colors.",
                    ))
                    .child(
                        div()
                            .h(px(300.))
                            .p_3()
                            .rounded(cx.theme().radius_lg)
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().background)
                            .child(
                                TextView::markdown(preview_id, preview, window, cx)
                                    .selectable(true)
                                    .scrollable(true)
                                    .size_full(),
                            ),
                    ),
            )
    }

    fn render_import(&self, cx: &mut Context<Self>) -> Div {
        div().flex().flex_col().gap_5()
            .child(section_heading("Import an editor theme", "Alula accepts its native TOML plus VS Code and Zed JSON themes. Imported values are previewed before you save."))
            .child(
                div().flex().gap_2()
                    .child(Button::new("import-theme").primary().label("Choose theme file…").on_click(cx.listener(Self::choose_import)))
                    .child(Button::new("reload-theme").outline().label("Reload TOML").on_click(cx.listener(Self::reload)))
                    .child(Button::new("reset-theme").ghost().label("Reset defaults").on_click(cx.listener(Self::reset))),
            )
            .child(
                div().p_4().rounded(cx.theme().radius_lg).border_1().border_color(cx.theme().border).bg(cx.theme().muted)
                    .flex().flex_col().gap_2()
                    .child(Label::new("Configuration file").font_weight(FontWeight::SEMIBOLD))
                    .child(Label::new(self.path.display().to_string()).text_sm().text_color(cx.theme().muted_foreground))
                    .child(Label::new("MCP agents can call get_theme_schema, preview_theme, save_theme, and import_theme. Agent-created themes use this exact validation and storage path.").text_sm()),
            )
    }

    fn render_theme_page(&self, window: &mut Window, cx: &mut Context<Self>) -> Div {
        let current = self.theme_section;
        let app = cx.entity();
        let mut tabs = TabBar::new("theme-settings-tabs")
            .segmented()
            .small()
            .w_full()
            .selected_index(current as usize)
            .on_click(move |index, _, cx| {
                if let Some(section) = ThemeSection::ALL.get(*index).copied() {
                    app.update(cx, |this, cx| {
                        this.theme_section = section;
                        cx.notify();
                    });
                }
            });
        for section in ThemeSection::ALL {
            tabs = tabs.child(Tab::new().flex_1().label(section.label()));
        }
        let content = match current {
            ThemeSection::Interface => self.render_interface(cx),
            ThemeSection::Syntax => self.render_syntax(window, cx),
            ThemeSection::Import => self.render_import(cx),
        };
        div().flex().flex_col().gap_4().child(tabs).child(content)
    }
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let current = self.page;
        let app = cx.entity();
        let mut tabs = TabBar::new("settings-pages")
            .segmented()
            .w_full()
            .selected_index(current as usize)
            .on_click(move |index, _, cx| {
                if let Some(page) = SettingsPage::ALL.get(*index).copied() {
                    app.update(cx, |this, cx| {
                        this.page = page;
                        cx.notify();
                    });
                }
            });
        for page in SettingsPage::ALL {
            tabs = tabs.child(Tab::new().flex_1().label(page.label()));
        }
        let content = match current {
            SettingsPage::General => self.render_general(cx),
            SettingsPage::Agent => self.render_agent(cx),
            SettingsPage::Theme => self.render_theme_page(window, cx),
        };
        div()
            .w_full()
            .h(px(570.))
            .flex()
            .flex_col()
            .gap_4()
            .child(tabs)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .pr_2()
                    .child(content),
            )
            .when_some(self.status.clone(), |this, (success, message)| {
                this.child(Label::new(message).text_xs().text_color(if success {
                    cx.theme().success
                } else {
                    cx.theme().danger
                }))
            })
    }
}

fn field(label: &'static str, input: Input) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(Label::new(label).text_xs())
        .child(input)
}

fn section_heading(title: &'static str, description: &'static str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(Label::new(title).font_weight(FontWeight::SEMIBOLD))
        .child(Label::new(description).text_xs())
}
