use crate::{AppConfig, ShortcutCommand};
use gpui::{KeyBinding, actions};

actions!(
    alula_shortcuts,
    [
        OpenCommandPalette,
        CreateNew,
        CloseTab,
        NextTab,
        PreviousTab,
        SendRequest,
        ShowParameters,
        ShowHeaders,
        ShowBody,
        CopyResponseBody,
        AddParameter,
        AddHeader,
        ShowRequests,
        ShowEnvironments,
        ShowHistory,
        OpenSettings,
        FocusUrl,
        ShowFormattedResponse,
        ShowRawResponse,
        QuitApplication,
    ]
);

fn platform_key_bindings() -> Vec<KeyBinding> {
    #[cfg(target_os = "macos")]
    {
        vec![KeyBinding::new("cmd-q", QuitApplication, None)]
    }

    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

pub fn configured_key_bindings(config: &AppConfig) -> Vec<KeyBinding> {
    let mut bindings = Vec::new();
    macro_rules! bind {
        ($command:expr, $action:expr) => {{
            let source = $command.binding(&config.keybindings).trim();
            if !source.is_empty() {
                // Application shortcuts must match even when no control owns
                // focus. Modal suppression happens in the action handlers.
                bindings.push(KeyBinding::new(source, $action, None));
            }
        }};
    }
    bind!(ShortcutCommand::OpenCommandPalette, OpenCommandPalette);
    bind!(ShortcutCommand::CreateNew, CreateNew);
    bind!(ShortcutCommand::CloseTab, CloseTab);
    bind!(ShortcutCommand::NextTab, NextTab);
    bind!(ShortcutCommand::PreviousTab, PreviousTab);
    bind!(ShortcutCommand::SendRequest, SendRequest);
    bind!(ShortcutCommand::ShowParameters, ShowParameters);
    bind!(ShortcutCommand::ShowHeaders, ShowHeaders);
    bind!(ShortcutCommand::ShowBody, ShowBody);
    bind!(ShortcutCommand::CopyResponseBody, CopyResponseBody);
    bind!(ShortcutCommand::AddParameter, AddParameter);
    bind!(ShortcutCommand::AddHeader, AddHeader);
    bind!(ShortcutCommand::ShowRequests, ShowRequests);
    bind!(ShortcutCommand::ShowEnvironments, ShowEnvironments);
    bind!(ShortcutCommand::ShowHistory, ShowHistory);
    bind!(ShortcutCommand::OpenSettings, OpenSettings);
    bind!(ShortcutCommand::FocusUrl, FocusUrl);
    bind!(
        ShortcutCommand::ShowFormattedResponse,
        ShowFormattedResponse
    );
    bind!(ShortcutCommand::ShowRawResponse, ShowRawResponse);
    bindings
}

pub fn application_key_bindings(config: &AppConfig) -> Vec<KeyBinding> {
    let mut bindings = configured_key_bindings(config);
    // System shortcuts are reserved and must win over conflicting user
    // bindings, so append them last (GPUI gives later bindings precedence).
    bindings.extend(platform_key_bindings());
    bindings
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keymap, Keystroke};

    #[test]
    fn application_shortcuts_match_without_a_focused_key_context() {
        let config = AppConfig::default();
        let keymap = Keymap::new(configured_key_bindings(&config));
        let input = [Keystroke::parse(&config.keybindings.create_new).unwrap()];
        let (bindings, pending) = keymap.bindings_for_input(&input, &[]);

        assert!(!pending);
        assert_eq!(bindings.len(), 1);
        assert!(bindings[0].action().as_any().is::<CreateNew>());

        let input = [Keystroke::parse(&config.keybindings.command_palette).unwrap()];
        let (bindings, pending) = keymap.bindings_for_input(&input, &[]);
        assert!(!pending);
        assert_eq!(bindings.len(), 1);
        assert!(bindings[0].action().as_any().is::<OpenCommandPalette>());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_quit_shortcut_matches_without_a_focused_key_context() {
        let mut config = AppConfig::default();
        config.keybindings.command_palette = "cmd-q".into();
        let keymap = Keymap::new(application_key_bindings(&config));
        let input = [Keystroke::parse("cmd-q").unwrap()];
        let (bindings, pending) = keymap.bindings_for_input(&input, &[]);

        assert!(!pending);
        assert_eq!(bindings.len(), 2);
        assert!(bindings[0].action().as_any().is::<QuitApplication>());
    }
}
