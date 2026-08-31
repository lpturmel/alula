use std::{collections::HashMap, fmt, ops::Range};

use anyhow::{Context as _, Result};

use crate::{Environment, EnvironmentVariable, KeyValueField, RequestDraft};

pub const VARIABLE_OPEN: &str = "{{";
pub const VARIABLE_CLOSE: &str = "}}";
const KEYRING_SERVICE: &str = "dev.alula.environment-variable";
const INDEX_MIN_VARIABLES: usize = 16;
const INDEX_MIN_REFERENCES: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VariableErrorKind {
    InvalidSyntax,
    MissingVariable(String),
    MissingSecret(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableError {
    pub range: Range<usize>,
    pub kind: VariableErrorKind,
}

impl fmt::Display for VariableError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            VariableErrorKind::InvalidSyntax => write!(formatter, "invalid variable syntax"),
            VariableErrorKind::MissingVariable(name) => {
                write!(
                    formatter,
                    "variable `{name}` is not defined in this environment"
                )
            }
            VariableErrorKind::MissingSecret(name) => {
                write!(
                    formatter,
                    "secret variable `{name}` has no value in the credential store"
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TemplateInspection {
    pub references: Vec<Range<usize>>,
    pub errors: Vec<VariableError>,
}

impl TemplateInspection {
    pub fn is_valid_reference(&self) -> bool {
        !self.references.is_empty() && self.errors.is_empty()
    }
}

pub fn valid_variable_name(name: &str) -> bool {
    let Some((&first, remaining)) = name.as_bytes().split_first() else {
        return false;
    };
    (first == b'_' || first.is_ascii_alphabetic())
        && remaining.iter().all(|byte| {
            *byte == b'_' || *byte == b'-' || *byte == b'.' || byte.is_ascii_alphanumeric()
        })
}

pub fn inspect_template(source: &str, environment: Option<&Environment>) -> TemplateInspection {
    let variable_index = environment.and_then(|environment| variable_index(source, environment));
    let mut references = Vec::new();
    let errors = visit_template(
        source,
        environment,
        variable_index.as_ref(),
        |start, end, _| {
            references.push(start..end);
        },
    );
    TemplateInspection { references, errors }
}

fn variable_index<'a>(
    source: &str,
    environment: &'a Environment,
) -> Option<HashMap<&'a str, &'a EnvironmentVariable>> {
    if environment.variables.len() < INDEX_MIN_VARIABLES
        || source
            .match_indices(VARIABLE_OPEN)
            .take(INDEX_MIN_REFERENCES)
            .count()
            < INDEX_MIN_REFERENCES
    {
        return None;
    }
    let mut index = HashMap::with_capacity(environment.variables.len());
    for variable in &environment.variables {
        index.entry(variable.name.as_str()).or_insert(variable);
    }
    Some(index)
}

fn visit_template<'a>(
    source: &'a str,
    environment: Option<&'a Environment>,
    variable_index: Option<&HashMap<&'a str, &'a EnvironmentVariable>>,
    mut visit_reference: impl FnMut(usize, usize, &'a str),
) -> Vec<VariableError> {
    let mut errors = Vec::new();
    let mut cursor = 0;

    while cursor < source.len() {
        let rest = &source[cursor..];
        let next_open = rest.find(VARIABLE_OPEN).map(|offset| cursor + offset);
        let next_close = rest.find(VARIABLE_CLOSE).map(|offset| cursor + offset);
        if let Some(close) = next_close
            && next_open.is_none_or(|open| close < open)
        {
            errors.push(VariableError {
                range: close..close + VARIABLE_CLOSE.len(),
                kind: VariableErrorKind::InvalidSyntax,
            });
            cursor = close + VARIABLE_CLOSE.len();
            continue;
        }
        let Some(open) = next_open else {
            break;
        };
        let content_start = open + VARIABLE_OPEN.len();
        let Some(close) = next_close else {
            errors.push(VariableError {
                range: open..source.len(),
                kind: VariableErrorKind::InvalidSyntax,
            });
            break;
        };
        let end = close + VARIABLE_CLOSE.len();
        let name = &source[content_start..close];
        if !valid_variable_name(name) {
            errors.push(VariableError {
                range: open..end,
                kind: VariableErrorKind::InvalidSyntax,
            });
        } else if let Some(variable) = match variable_index {
            Some(index) => index.get(name).copied(),
            None => environment.and_then(|environment| {
                environment
                    .variables
                    .iter()
                    .find(|variable| variable.name == name)
            }),
        } {
            if variable.secret && variable.value.is_none() {
                errors.push(VariableError {
                    range: open..end,
                    kind: VariableErrorKind::MissingSecret(name.to_owned()),
                });
            } else {
                visit_reference(open, end, variable.value.as_deref().unwrap_or_default());
            }
        } else {
            errors.push(VariableError {
                range: open..end,
                kind: VariableErrorKind::MissingVariable(name.to_owned()),
            });
        }
        cursor = end;
    }

    errors
}

pub fn resolve_template(
    source: &str,
    environment: Option<&Environment>,
) -> Result<String, Vec<VariableError>> {
    let Some(environment) = environment else {
        let errors = visit_template(source, None, None, |_, _, _| {});
        return if errors.is_empty() {
            Ok(source.to_owned())
        } else {
            Err(errors)
        };
    };
    let variable_index = variable_index(source, environment);
    let mut resolved = String::with_capacity(source.len());
    let mut cursor = 0;
    let errors = visit_template(
        source,
        Some(environment),
        variable_index.as_ref(),
        |start, end, value| {
            resolved.push_str(&source[cursor..start]);
            resolved.push_str(value);
            cursor = end;
        },
    );
    if !errors.is_empty() {
        return Err(errors);
    }
    resolved.push_str(&source[cursor..]);
    Ok(resolved)
}

pub fn resolve_request(
    request: &RequestDraft,
    environment: Option<&Environment>,
) -> Result<RequestDraft, Vec<String>> {
    let mut errors = Vec::new();
    let resolve = |label: &str, source: &str, errors: &mut Vec<String>| {
        resolve_template(source, environment).unwrap_or_else(|failures| {
            errors.extend(
                failures
                    .into_iter()
                    .map(|error| format!("{label}: {error}")),
            );
            source.to_owned()
        })
    };
    let resolve_fields = |label: &str, fields: &[KeyValueField], errors: &mut Vec<String>| {
        fields
            .iter()
            .map(|field| {
                if !field.enabled {
                    return field.clone();
                }
                KeyValueField {
                    id: field.id.clone(),
                    enabled: field.enabled,
                    key: resolve(&format!("{label} key"), &field.key, errors),
                    value: resolve(&format!("{label} value"), &field.value, errors),
                }
            })
            .collect()
    };
    let resolved = RequestDraft {
        id: request.id.clone(),
        name: request.name.clone(),
        method: request.method,
        url: resolve("URL", &request.url, &mut errors),
        parameters: resolve_fields("Parameter", &request.parameters, &mut errors),
        headers: resolve_fields("Header", &request.headers, &mut errors),
        body: resolve("Body", &request.body, &mut errors),
    };
    if errors.is_empty() {
        Ok(resolved)
    } else {
        Err(errors)
    }
}

fn keyring_account(environment_id: &str, variable_id: &str) -> String {
    format!("{environment_id}:{variable_id}")
}

pub fn store_secret(environment_id: &str, variable_id: &str, value: &str) -> Result<()> {
    keyring::Entry::new(
        KEYRING_SERVICE,
        &keyring_account(environment_id, variable_id),
    )
    .context("could not access the OS credential store")?
    .set_password(value)
    .context("could not save the secret in the OS credential store")
}

pub fn load_secret(environment_id: &str, variable_id: &str) -> Result<Option<String>> {
    let entry = keyring::Entry::new(
        KEYRING_SERVICE,
        &keyring_account(environment_id, variable_id),
    )
    .context("could not access the OS credential store")?;
    match entry.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error).context("could not read the secret from the OS credential store"),
    }
}

pub fn delete_secret(environment_id: &str, variable_id: &str) -> Result<()> {
    let entry = keyring::Entry::new(
        KEYRING_SERVICE,
        &keyring_account(environment_id, variable_id),
    )
    .context("could not access the OS credential store")?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => {
            Err(error).context("could not remove the secret from the OS credential store")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EnvironmentVariable;

    fn environment() -> Environment {
        let mut environment = Environment::new("Local");
        environment.variables = vec![
            EnvironmentVariable::public("base_url", "https://example.com"),
            EnvironmentVariable::secret("token", Some("very-secret".into())),
        ];
        environment
    }

    #[test]
    fn resolves_variables_across_a_template() {
        assert_eq!(
            resolve_template("{{base_url}}/users?token={{token}}", Some(&environment())).unwrap(),
            "https://example.com/users?token=very-secret"
        );
    }

    #[test]
    fn reports_syntax_missing_and_unavailable_secret_errors() {
        let mut environment = environment();
        environment.variables[1].value = None;
        assert!(matches!(
            inspect_template("{{", Some(&environment)).errors[0].kind,
            VariableErrorKind::InvalidSyntax
        ));
        assert!(matches!(
            inspect_template("{{unknown}}", Some(&environment)).errors[0].kind,
            VariableErrorKind::MissingVariable(_)
        ));
        assert!(matches!(
            inspect_template("{{token}}", Some(&environment)).errors[0].kind,
            VariableErrorKind::MissingSecret(_)
        ));
    }

    #[test]
    fn indexed_resolution_preserves_first_duplicate_variable() {
        let mut environment = Environment::new("Large");
        environment.variables = (0..16)
            .map(|index| EnvironmentVariable::public(format!("value_{index}"), index.to_string()))
            .collect();
        environment
            .variables
            .push(EnvironmentVariable::public("value_0", "duplicate"));
        let template = "{{value_0}}".repeat(INDEX_MIN_REFERENCES);

        assert_eq!(
            resolve_template(&template, Some(&environment)).unwrap(),
            "0".repeat(INDEX_MIN_REFERENCES)
        );
    }

    #[test]
    fn validates_variable_names() {
        assert!(valid_variable_name("api.token-1"));
        assert!(!valid_variable_name("1token"));
        assert!(!valid_variable_name("api token"));
    }

    #[test]
    fn resolves_every_request_surface_but_ignores_disabled_rows() {
        let mut request = RequestDraft {
            url: "{{base_url}}/users".into(),
            body: r#"{"token":"{{token}}"}"#.into(),
            ..RequestDraft::default()
        };
        request.parameters = vec![KeyValueField::new("account", "{{token}}")];
        request.headers = vec![
            KeyValueField::new("Authorization", "Bearer {{token}}"),
            KeyValueField {
                enabled: false,
                ..KeyValueField::new("Ignored", "{{missing}}")
            },
        ];

        let resolved = resolve_request(&request, Some(&environment())).unwrap();
        assert_eq!(resolved.url, "https://example.com/users");
        assert_eq!(resolved.parameters[0].value, "very-secret");
        assert_eq!(resolved.headers[0].value, "Bearer very-secret");
        assert_eq!(resolved.headers[1].value, "{{missing}}");
        assert_eq!(resolved.body, r#"{"token":"very-secret"}"#);

        // The saved draft remains a template and therefore cannot leak a secret
        // into workspace or history persistence.
        assert_eq!(request.headers[0].value, "Bearer {{token}}");
    }
}
