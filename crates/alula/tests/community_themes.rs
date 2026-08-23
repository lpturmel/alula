use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use alula::{AppConfig, ThemeModePreference};
use serde::Deserialize;
use url::Url;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registry {
    schema_version: u32,
    updated_at: String,
    themes: Vec<RegistryTheme>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryTheme {
    id: String,
    name: String,
    author: String,
    description: String,
    mode: ThemeModePreference,
    license: String,
    license_url: String,
    source_url: String,
    file: String,
    tags: Vec<String>,
    preview: ThemePreview,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemePreview {
    background: String,
    foreground: String,
    accent: String,
    primary: String,
}

fn registry_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../alula-themes")
}

fn non_empty(value: &str) -> bool {
    !value.trim().is_empty()
}

fn is_registry_id(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn assert_https_url(value: &str) {
    let url = Url::parse(value).unwrap_or_else(|error| panic!("invalid URL {value}: {error}"));
    assert_eq!(url.scheme(), "https", "registry URLs must use HTTPS");
    assert!(
        url.host_str().is_some(),
        "registry URL has no host: {value}"
    );
}

#[test]
fn community_registry_and_every_theme_pass_the_production_parser() {
    let root = registry_root();
    let source = fs::read_to_string(root.join("registry.toml")).unwrap();
    let registry: Registry = toml::from_str(&source).unwrap();

    assert_eq!(registry.schema_version, 1);
    assert_eq!(
        registry.updated_at.len(),
        10,
        "updated_at must be YYYY-MM-DD"
    );
    assert!(registry.updated_at.as_bytes()[4] == b'-');
    assert!(registry.updated_at.as_bytes()[7] == b'-');
    assert!(registry.themes.len() >= 9);

    let notices = fs::read_to_string(root.join("THIRD_PARTY_NOTICES.md")).unwrap();
    let mut ids = BTreeSet::new();
    let mut names = BTreeSet::new();
    let mut registered_files = BTreeSet::new();

    for entry in &registry.themes {
        assert!(is_registry_id(&entry.id), "invalid theme id: {}", entry.id);
        assert!(ids.insert(entry.id.as_str()), "duplicate id: {}", entry.id);
        assert!(
            names.insert(entry.name.as_str()),
            "duplicate name: {}",
            entry.name
        );
        assert!(non_empty(&entry.author));
        assert!(non_empty(&entry.description));
        assert!(non_empty(&entry.license));
        assert!(!entry.tags.is_empty(), "{} has no tags", entry.id);
        assert!(entry.tags.iter().all(|tag| non_empty(tag)));
        assert_https_url(&entry.license_url);
        assert_https_url(&entry.source_url);
        assert!(
            notices.contains(&entry.source_url),
            "{} is missing from THIRD_PARTY_NOTICES.md",
            entry.id
        );

        let expected_file = format!("themes/{}.toml", entry.id);
        assert_eq!(entry.file, expected_file);
        let relative = Path::new(&entry.file);
        assert!(!relative.is_absolute());
        assert!(
            relative
                .components()
                .all(|component| matches!(component, Component::Normal(_))),
            "registry paths may not traverse directories: {}",
            entry.file
        );

        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).unwrap();
        assert!(metadata.is_file());
        assert!(!metadata.file_type().is_symlink());
        assert!(
            metadata.len() <= 64 * 1024,
            "theme file is unexpectedly large"
        );

        let config = AppConfig::load(&path)
            .unwrap_or_else(|error| panic!("{} failed the production parser: {error:#}", entry.id));
        assert_eq!(config.version, 1);
        assert_eq!(config.theme.name, entry.name);
        assert_eq!(config.theme.mode, entry.mode);
        assert_eq!(config.theme.colors.background, entry.preview.background);
        assert_eq!(config.theme.colors.foreground, entry.preview.foreground);
        assert_eq!(config.theme.colors.accent, entry.preview.accent);
        assert_eq!(config.theme.colors.primary, entry.preview.primary);
        registered_files.insert(path.canonicalize().unwrap());
    }

    let theme_files = fs::read_dir(root.join("themes"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "toml")
        })
        .map(|path| path.canonicalize().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(registered_files, theme_files);
}

#[test]
fn community_registry_schemas_are_valid_json() {
    let root = registry_root();
    for name in ["registry.schema.json", "theme.schema.json"] {
        let source = fs::read_to_string(root.join("schemas").join(name)).unwrap();
        let schema: serde_json::Value = serde_json::from_str(&source).unwrap();
        assert_eq!(
            schema.get("$schema").and_then(serde_json::Value::as_str),
            Some("https://json-schema.org/draft/2020-12/schema")
        );
    }
}
