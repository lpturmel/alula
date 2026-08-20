use std::{
    collections::hash_map::DefaultHasher,
    env, fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
};

use anyhow::{Context as _, Result, bail};
use gpui::Hsla;
use gpui_component::{
    Colorize, ThemeConfig as GpuiThemeConfig, ThemeConfigColors, ThemeMode,
    highlighter::HighlightThemeStyle,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeModePreference {
    Light,
    Dark,
}

impl ThemeModePreference {
    pub fn gpui(self) -> ThemeMode {
        match self {
            Self::Light => ThemeMode::Light,
            Self::Dark => ThemeMode::Dark,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: u32,
    pub application: ApplicationSettings,
    pub agent: AgentSettings,
    pub theme: ThemeSettings,
    pub syntax: SyntaxPalette,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ApplicationSettings {
    /// Empty means the platform default. A non-empty value is also used as a
    /// redirect when Alula starts from its default configuration file.
    pub config_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSettings {
    /// Reserved loopback port for Alula's agent/MCP service.
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSettings {
    pub name: String,
    pub mode: ThemeModePreference,
    pub radius: usize,
    pub radius_large: usize,
    pub colors: ThemePalette,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemePalette {
    pub background: String,
    pub surface: String,
    pub foreground: String,
    pub muted_foreground: String,
    pub border: String,
    pub accent: String,
    pub accent_foreground: String,
    pub primary: String,
    pub primary_foreground: String,
    pub sidebar: String,
    pub sidebar_foreground: String,
    pub selection: String,
    pub success: String,
    pub warning: String,
    pub danger: String,
    pub info: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(default)]
pub struct SyntaxPalette {
    pub editor_background: String,
    pub editor_foreground: String,
    pub comment: String,
    pub keyword: String,
    pub string: String,
    pub number: String,
    pub function: String,
    pub type_color: String,
    pub variable: String,
    pub property: String,
    pub tag: String,
    pub attribute: String,
    pub boolean: String,
    pub constant: String,
    pub punctuation: String,
    pub operator: String,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: 1,
            application: ApplicationSettings::default(),
            agent: AgentSettings::default(),
            theme: ThemeSettings::default(),
            syntax: SyntaxPalette::default(),
        }
    }
}

impl Default for ApplicationSettings {
    fn default() -> Self {
        Self {
            config_path: String::new(),
        }
    }
}

impl Default for AgentSettings {
    fn default() -> Self {
        Self { port: 37_421 }
    }
}

impl Default for ThemeSettings {
    fn default() -> Self {
        Self {
            name: "Alula Dark".into(),
            mode: ThemeModePreference::Dark,
            radius: 6,
            radius_large: 8,
            colors: ThemePalette::default(),
        }
    }
}

impl Default for ThemePalette {
    fn default() -> Self {
        Self {
            background: "#0a0a0a".into(),
            surface: "#171717".into(),
            foreground: "#fafafa".into(),
            muted_foreground: "#a3a3a3".into(),
            border: "#262626".into(),
            accent: "#262626".into(),
            accent_foreground: "#fafafa".into(),
            primary: "#fafafa".into(),
            primary_foreground: "#171717".into(),
            sidebar: "#0a0a0a".into(),
            sidebar_foreground: "#e5e5e5".into(),
            selection: "#2563eb66".into(),
            success: "#22c55e".into(),
            warning: "#eab308".into(),
            danger: "#ef4444".into(),
            info: "#0ea5e9".into(),
        }
    }
}

impl Default for SyntaxPalette {
    fn default() -> Self {
        Self {
            editor_background: "#171717".into(),
            editor_foreground: "#caccca".into(),
            comment: "#9e9e9e".into(),
            keyword: "#c28b12".into(),
            string: "#62ba46".into(),
            number: "#e1d797".into(),
            function: "#fdd888".into(),
            type_color: "#b5af9a".into(),
            variable: "#caccca".into(),
            property: "#d4be98".into(),
            tag: "#e7cb8f".into(),
            attribute: "#e7cb8f".into(),
            boolean: "#e1d797".into(),
            constant: "#e1d797".into(),
            punctuation: "#caccca".into(),
            operator: "#b5af9a".into(),
        }
    }
}

pub fn default_config_path() -> PathBuf {
    if let Some(path) = env::var_os("ALULA_CONFIG") {
        return PathBuf::from(path);
    }
    if let Some(root) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(root).join("alula/config.toml");
    }
    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".config/alula/config.toml");
    }
    PathBuf::from("alula.toml")
}

pub fn config_path() -> PathBuf {
    if env::var_os("ALULA_CONFIG").is_some() {
        return default_config_path();
    }

    let default = default_config_path();
    resolve_config_path(&default)
}

fn resolve_config_path(default: &Path) -> PathBuf {
    let selected =
        read_location_file(&location_path(default)).unwrap_or_else(|| default.to_path_buf());
    if let Ok(source) = fs::read_to_string(&selected)
        && let Ok(value) = toml::from_str::<toml::Value>(&source)
        && let Some(path) = value
            .get("application")
            .and_then(|value| value.get("config_path"))
            .and_then(toml::Value::as_str)
            .filter(|value| !value.trim().is_empty())
    {
        return PathBuf::from(path);
    }
    selected
}

pub fn save_config_location(path: &Path) -> Result<()> {
    if env::var_os("ALULA_CONFIG").is_some() {
        bail!("ALULA_CONFIG overrides the configuration path for this process");
    }
    let locator = location_path(&default_config_path());
    if let Some(parent) = locator.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let source = toml::to_string_pretty(&ConfigLocation {
        path: path.to_path_buf(),
    })
    .context("failed to serialize configuration location")?;
    let temporary = locator.with_extension("toml.tmp");
    fs::write(&temporary, source)
        .with_context(|| format!("failed to write {}", temporary.display()))?;
    fs::rename(&temporary, &locator)
        .with_context(|| format!("failed to replace {}", locator.display()))?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct ConfigLocation {
    path: PathBuf,
}

fn location_path(default: &Path) -> PathBuf {
    default.with_file_name("location.toml")
}

fn read_location_file(path: &Path) -> Option<PathBuf> {
    let source = fs::read_to_string(path).ok()?;
    toml::from_str::<ConfigLocation>(&source)
        .ok()
        .map(|value| value.path)
}

impl AppConfig {
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let source = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let config: Self = toml::from_str(&source)
            .with_context(|| format!("invalid TOML in {}", path.display()))?;
        config.validate()?;
        Ok(config)
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }
        let config = Self::default();
        config.save(path)?;
        Ok(config)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let source = toml::to_string_pretty(self).context("failed to serialize theme config")?;
        let temporary = path.with_extension("toml.tmp");
        fs::write(&temporary, source)
            .with_context(|| format!("failed to write {}", temporary.display()))?;
        fs::rename(&temporary, path)
            .with_context(|| format!("failed to replace {}", path.display()))?;
        Ok(())
    }

    pub fn to_toml(&self) -> Result<String> {
        self.validate()?;
        toml::to_string_pretty(self).context("failed to serialize theme config")
    }

    pub fn from_toml(source: &str) -> Result<Self> {
        let config: Self = toml::from_str(source).context("invalid Alula theme TOML")?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        for (name, value) in self.color_entries() {
            if Hsla::parse_hex(value).is_err() {
                bail!("{name} must be a hex color (#RRGGBB or #RRGGBBAA), got {value}");
            }
        }
        if self.theme.name.trim().is_empty() {
            bail!("theme.name cannot be empty");
        }
        if self.theme.radius > 32 || self.theme.radius_large > 48 {
            bail!("theme radii are outside the supported range");
        }
        if self.agent.port == 0 {
            bail!("agent.port must be between 1 and 65535");
        }
        Ok(())
    }

    pub fn to_gpui_theme(&self) -> Result<Rc<GpuiThemeConfig>> {
        self.validate()?;
        let palette = &self.theme.colors;
        let mut colors = ThemeConfigColors::default();
        colors.background = color(&palette.background);
        colors.foreground = color(&palette.foreground);
        colors.border = color(&palette.border);
        colors.input = color(&palette.border);
        colors.group_box = color(&palette.surface);
        colors.group_box_foreground = color(&palette.foreground);
        colors.accent = color(&palette.accent);
        colors.accent_foreground = color(&palette.accent_foreground);
        colors.muted = color(&palette.surface);
        colors.muted_foreground = color(&palette.muted_foreground);
        colors.popover = color(&palette.surface);
        colors.popover_foreground = color(&palette.foreground);
        colors.primary = color(&palette.primary);
        colors.primary_foreground = color(&palette.primary_foreground);
        colors.secondary = color(&palette.surface);
        colors.secondary_foreground = color(&palette.foreground);
        colors.sidebar = color(&palette.sidebar);
        colors.sidebar_foreground = color(&palette.sidebar_foreground);
        colors.sidebar_border = color(&palette.border);
        colors.sidebar_accent = color(&palette.accent);
        colors.sidebar_accent_foreground = color(&palette.accent_foreground);
        colors.selection = color(&palette.selection);
        colors.success = color(&palette.success);
        colors.warning = color(&palette.warning);
        colors.danger = color(&palette.danger);
        colors.info = color(&palette.info);
        colors.tab = color(&palette.background);
        colors.tab_active = color(&palette.surface);
        colors.tab_active_foreground = color(&palette.foreground);
        colors.tab_bar = color(&palette.background);
        colors.tab_bar_segmented = color(&palette.surface);
        colors.tab_foreground = color(&palette.muted_foreground);
        colors.list = color(&palette.background);
        colors.list_head = color(&palette.surface);
        colors.list_hover = color(&palette.surface);
        colors.title_bar = color(&palette.surface);
        colors.title_bar_border = color(&palette.border);
        colors.primary_hover = colors.primary.clone();
        colors.primary_active = colors.primary.clone();
        colors.secondary_hover = colors.accent.clone();
        colors.secondary_active = colors.accent.clone();
        colors.list_active = colors.accent.clone();
        colors.list_active_border = colors.primary.clone();

        let syntax = &self.syntax;
        let highlight: HighlightThemeStyle = serde_json::from_value(json!({
            "editor.background": syntax.editor_background,
            "editor.foreground": syntax.editor_foreground,
            "editor.active_line.background": palette.surface,
            "editor.line_number": palette.muted_foreground,
            "editor.active_line_number": palette.foreground,
            "syntax": {
                "attribute": { "color": syntax.attribute },
                "boolean": { "color": syntax.boolean },
                "comment": { "color": syntax.comment, "font_style": "italic" },
                "comment.doc": { "color": syntax.comment, "font_style": "italic" },
                "constant": { "color": syntax.constant },
                "function": { "color": syntax.function },
                "keyword": { "color": syntax.keyword },
                "number": { "color": syntax.number },
                "operator": { "color": syntax.operator },
                "property": { "color": syntax.property },
                "punctuation": { "color": syntax.punctuation },
                "string": { "color": syntax.string },
                "string.escape": { "color": syntax.string },
                "tag": { "color": syntax.tag },
                "tag.doctype": { "color": syntax.tag },
                "type": { "color": syntax.type_color },
                "variable": { "color": syntax.variable }
            }
        }))
        .context("failed to build syntax highlight theme")?;

        Ok(Rc::new(GpuiThemeConfig {
            name: self.theme.name.clone().into(),
            mode: self.theme.mode.gpui(),
            radius: Some(self.theme.radius),
            radius_lg: Some(self.theme.radius_large),
            colors,
            highlight: Some(highlight),
            ..Default::default()
        }))
    }

    fn color_entries(&self) -> Vec<(&'static str, &str)> {
        let c = &self.theme.colors;
        let s = &self.syntax;
        vec![
            ("theme.colors.background", &c.background),
            ("theme.colors.surface", &c.surface),
            ("theme.colors.foreground", &c.foreground),
            ("theme.colors.muted_foreground", &c.muted_foreground),
            ("theme.colors.border", &c.border),
            ("theme.colors.accent", &c.accent),
            ("theme.colors.accent_foreground", &c.accent_foreground),
            ("theme.colors.primary", &c.primary),
            ("theme.colors.primary_foreground", &c.primary_foreground),
            ("theme.colors.sidebar", &c.sidebar),
            ("theme.colors.sidebar_foreground", &c.sidebar_foreground),
            ("theme.colors.selection", &c.selection),
            ("theme.colors.success", &c.success),
            ("theme.colors.warning", &c.warning),
            ("theme.colors.danger", &c.danger),
            ("theme.colors.info", &c.info),
            ("syntax.editor_background", &s.editor_background),
            ("syntax.editor_foreground", &s.editor_foreground),
            ("syntax.comment", &s.comment),
            ("syntax.keyword", &s.keyword),
            ("syntax.string", &s.string),
            ("syntax.number", &s.number),
            ("syntax.function", &s.function),
            ("syntax.type_color", &s.type_color),
            ("syntax.variable", &s.variable),
            ("syntax.property", &s.property),
            ("syntax.tag", &s.tag),
            ("syntax.attribute", &s.attribute),
            ("syntax.boolean", &s.boolean),
            ("syntax.constant", &s.constant),
            ("syntax.punctuation", &s.punctuation),
            ("syntax.operator", &s.operator),
        ]
    }
}

fn color(value: &str) -> Option<gpui::SharedString> {
    Some(value.to_owned().into())
}

pub fn import_editor_theme(path: &Path) -> Result<AppConfig> {
    let source = fs::read_to_string(path)
        .with_context(|| format!("failed to read theme {}", path.display()))?;
    if path.extension().and_then(|value| value.to_str()) == Some("toml") {
        return AppConfig::from_toml(&source);
    }
    let value: Value = serde_json::from_str(&source)
        .with_context(|| format!("invalid JSON theme {}", path.display()))?;
    if value.get("tokenColors").is_some() || value.get("type").is_some() {
        import_vscode_theme(&value)
    } else if value.get("themes").is_some() || value.get("style").is_some() {
        import_zed_theme(&value)
    } else {
        bail!("unsupported theme format; expected Alula TOML, VS Code JSON, or Zed JSON")
    }
}

fn import_vscode_theme(value: &Value) -> Result<AppConfig> {
    let mut config = AppConfig::default();
    config.theme.name = string_at(value, "name")
        .unwrap_or("Imported VS Code theme")
        .into();
    config.theme.mode = if string_at(value, "type") == Some("light") {
        ThemeModePreference::Light
    } else {
        ThemeModePreference::Dark
    };
    let colors = value.get("colors").and_then(Value::as_object);
    apply_first(
        colors,
        &["editor.background"],
        &mut config.theme.colors.background,
    );
    apply_first(
        colors,
        &[
            "editorWidget.background",
            "sideBar.background",
            "panel.background",
        ],
        &mut config.theme.colors.surface,
    );
    apply_first(
        colors,
        &["editor.foreground", "foreground"],
        &mut config.theme.colors.foreground,
    );
    apply_first(
        colors,
        &["descriptionForeground"],
        &mut config.theme.colors.muted_foreground,
    );
    apply_first(
        colors,
        &["panel.border", "input.border"],
        &mut config.theme.colors.border,
    );
    apply_first(
        colors,
        &["focusBorder", "activityBarBadge.background"],
        &mut config.theme.colors.accent,
    );
    apply_first(
        colors,
        &["button.background"],
        &mut config.theme.colors.primary,
    );
    apply_first(
        colors,
        &["button.foreground"],
        &mut config.theme.colors.primary_foreground,
    );
    apply_first(
        colors,
        &["sideBar.background"],
        &mut config.theme.colors.sidebar,
    );
    apply_first(
        colors,
        &["sideBar.foreground"],
        &mut config.theme.colors.sidebar_foreground,
    );
    apply_first(
        colors,
        &["editor.selectionBackground"],
        &mut config.theme.colors.selection,
    );
    apply_first(
        colors,
        &["editorError.foreground"],
        &mut config.theme.colors.danger,
    );
    apply_first(
        colors,
        &["editorWarning.foreground"],
        &mut config.theme.colors.warning,
    );
    config.syntax.editor_background = config.theme.colors.background.clone();
    config.syntax.editor_foreground = config.theme.colors.foreground.clone();

    if let Some(rules) = value.get("tokenColors").and_then(Value::as_array) {
        for rule in rules {
            let Some(foreground) = rule
                .get("settings")
                .and_then(|settings| settings.get("foreground"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if Hsla::parse_hex(foreground).is_err() {
                continue;
            }
            let scopes = scopes(rule.get("scope"));
            apply_scope_color(&scopes, foreground, &mut config.syntax);
        }
    }
    config.validate()?;
    Ok(config)
}

fn import_zed_theme(value: &Value) -> Result<AppConfig> {
    let theme = value
        .get("themes")
        .and_then(Value::as_array)
        .and_then(|themes| themes.first())
        .unwrap_or(value);
    let style = theme.get("style").unwrap_or(theme);
    let mut config = AppConfig::default();
    config.theme.name = string_at(theme, "name")
        .unwrap_or("Imported Zed theme")
        .into();
    config.theme.mode = if string_at(theme, "appearance") == Some("light") {
        ThemeModePreference::Light
    } else {
        ThemeModePreference::Dark
    };
    apply_value(style, "background", &mut config.theme.colors.background);
    apply_value(
        style,
        "editor.background",
        &mut config.syntax.editor_background,
    );
    apply_value(style, "text", &mut config.theme.colors.foreground);
    apply_value(
        style,
        "editor.foreground",
        &mut config.syntax.editor_foreground,
    );
    apply_value(style, "border", &mut config.theme.colors.border);
    apply_value(style, "border.focused", &mut config.theme.colors.accent);
    apply_value(style, "panel.background", &mut config.theme.colors.surface);
    apply_value(
        style,
        "elevated_surface.background",
        &mut config.theme.colors.surface,
    );
    apply_value(
        style,
        "element.selected",
        &mut config.theme.colors.selection,
    );
    apply_value(style, "status.error", &mut config.theme.colors.danger);
    apply_value(style, "status.warning", &mut config.theme.colors.warning);
    apply_value(style, "status.success", &mut config.theme.colors.success);

    if let Some(syntax) = style.get("syntax").and_then(Value::as_object) {
        for (scope, value) in syntax {
            let color = value
                .get("color")
                .and_then(Value::as_str)
                .or_else(|| value.as_str());
            if let Some(color) = color {
                apply_scope_color(&[scope.as_str()], color, &mut config.syntax);
            }
        }
    }
    if config.syntax.editor_background == SyntaxPalette::default().editor_background {
        config.syntax.editor_background = config.theme.colors.background.clone();
    }
    if config.syntax.editor_foreground == SyntaxPalette::default().editor_foreground {
        config.syntax.editor_foreground = config.theme.colors.foreground.clone();
    }
    config.validate()?;
    Ok(config)
}

fn apply_first(
    colors: Option<&serde_json::Map<String, Value>>,
    keys: &[&str],
    target: &mut String,
) {
    let Some(colors) = colors else { return };
    for key in keys {
        if let Some(value) = colors.get(*key).and_then(Value::as_str)
            && Hsla::parse_hex(value).is_ok()
        {
            *target = value.into();
            return;
        }
    }
}

fn apply_value(object: &Value, key: &str, target: &mut String) {
    if let Some(value) = object.get(key).and_then(Value::as_str)
        && Hsla::parse_hex(value).is_ok()
    {
        *target = value.into();
    }
}

fn string_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn scopes(value: Option<&Value>) -> Vec<&str> {
    match value {
        Some(Value::String(scope)) => scope.split(',').map(str::trim).collect(),
        Some(Value::Array(scopes)) => scopes.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    }
}

fn apply_scope_color(scopes: &[&str], color: &str, syntax: &mut SyntaxPalette) {
    for scope in scopes {
        let scope = scope.to_ascii_lowercase();
        if scope.contains("comment") {
            syntax.comment = color.into();
        } else if scope.contains("string") {
            syntax.string = color.into();
        } else if scope.contains("keyword") || scope.contains("storage") {
            syntax.keyword = color.into();
        } else if scope.contains("numeric") || scope.contains("number") {
            syntax.number = color.into();
        } else if scope.contains("function") || scope.contains("method") {
            syntax.function = color.into();
        } else if scope.contains("type") || scope.contains("class") {
            syntax.type_color = color.into();
        } else if scope.contains("property") {
            syntax.property = color.into();
        } else if scope.contains("attribute") {
            syntax.attribute = color.into();
        } else if scope.contains("tag") {
            syntax.tag = color.into();
        } else if scope.contains("boolean") {
            syntax.boolean = color.into();
        } else if scope.contains("constant") {
            syntax.constant = color.into();
        } else if scope.contains("operator") {
            syntax.operator = color.into();
        } else if scope.contains("punctuation") {
            syntax.punctuation = color.into();
        } else if scope.contains("variable") {
            syntax.variable = color.into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_toml_and_builds_gpui_theme() {
        let config = AppConfig::default();
        let source = config.to_toml().unwrap();
        let decoded = AppConfig::from_toml(&source).unwrap();
        assert_eq!(decoded, config);
        assert!(decoded.to_gpui_theme().is_ok());
    }

    #[test]
    fn imports_vscode_colors_and_tokens() {
        let value = json!({
            "name": "Ocean Test",
            "type": "dark",
            "colors": {
                "editor.background": "#101820",
                "editor.foreground": "#f0f0f0",
                "focusBorder": "#00aaff"
            },
            "tokenColors": [{
                "scope": ["comment", "punctuation.definition.comment"],
                "settings": { "foreground": "#667788" }
            }]
        });
        let config = import_vscode_theme(&value).unwrap();
        assert_eq!(config.theme.name, "Ocean Test");
        assert_eq!(config.theme.colors.background, "#101820");
        assert_eq!(config.syntax.comment, "#667788");
    }

    #[test]
    fn validates_agent_port_and_round_trips_application_settings() {
        let mut config = AppConfig::default();
        config.agent.port = 45_678;
        config.application.config_path = "/tmp/alula-custom.toml".into();
        let decoded = AppConfig::from_toml(&config.to_toml().unwrap()).unwrap();
        assert_eq!(decoded.agent.port, 45_678);
        assert_eq!(decoded.application.config_path, "/tmp/alula-custom.toml");

        config.agent.port = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn resolves_a_configuration_redirect_from_toml() {
        let directory =
            std::env::temp_dir().join(format!("alula-config-redirect-{}", std::process::id()));
        let default = directory.join("config.toml");
        let custom = directory.join("custom.toml");
        let mut config = AppConfig::default();
        config.application.config_path = custom.display().to_string();
        config.save(&default).unwrap();

        assert_eq!(resolve_config_path(&default), custom);
        std::fs::remove_file(default).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
}
