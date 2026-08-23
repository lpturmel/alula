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
    ]
);

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
}
