#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormattedBody {
    pub text: String,
    pub language: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedFormattedBody {
    pub display: FormattedBody,
    pub markdown: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseBodyCache {
    pub formatted: CachedFormattedBody,
    pub raw: FormattedBody,
}

impl ResponseBodyCache {
    pub fn new(body: &str, content_type: Option<&str>) -> Self {
        Self::from_owned(body.to_owned(), content_type)
    }

    pub fn from_owned(body: String, content_type: Option<&str>) -> Self {
        let formatted = format_response_body(&body, content_type);
        let markdown = chunked_fenced_code_blocks(formatted.language, &formatted.text);
        let raw = FormattedBody {
            language: formatted.language,
            text: body,
        };
        Self {
            formatted: CachedFormattedBody {
                display: formatted,
                markdown,
            },
            raw,
        }
    }
}

const HIGHLIGHT_CHUNK_TARGET_BYTES: usize = 2 * 1024;

fn bounded_utf8_segments(mut text: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    while text.len() > HIGHLIGHT_CHUNK_TARGET_BYTES {
        let mut end = HIGHLIGHT_CHUNK_TARGET_BYTES;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        segments.push(&text[..end]);
        text = &text[end..];
    }
    if !text.is_empty() {
        segments.push(text);
    }
    segments
}

/// Build several top-level code blocks so gpui-components' scrollable TextView
/// can virtualize a large response. A single giant code block is one list item
/// and forces GPUI to shape the entire payload whenever it is painted.
pub fn chunked_fenced_code_blocks(language: &str, body: &str) -> String {
    let mut blocks = Vec::new();
    let mut chunk = String::new();
    let mut chunk_language = language;
    let mut embedded_language = None;

    let flush = |blocks: &mut Vec<String>, chunk: &mut String, language: &str| {
        if !chunk.is_empty() {
            blocks.push(fenced_code_block(language, chunk.trim_end_matches('\n')));
            chunk.clear();
        }
    };

    for line in body.split_inclusive('\n') {
        for segment in bounded_utf8_segments(line) {
            let trimmed = segment.trim_start().to_ascii_lowercase();
            let line_language = if matches!(language, "html" | "xml") {
                if trimmed.starts_with("</script") || trimmed.starts_with("</style") {
                    embedded_language = None;
                    language
                } else {
                    embedded_language.unwrap_or(language)
                }
            } else {
                language
            };

            if line_language != chunk_language
                || (!chunk.is_empty()
                    && chunk.len().saturating_add(segment.len()) > HIGHLIGHT_CHUNK_TARGET_BYTES)
            {
                flush(&mut blocks, &mut chunk, chunk_language);
                chunk_language = line_language;
            }
            chunk.push_str(segment);

            if matches!(language, "html" | "xml") {
                if trimmed.starts_with("<script") && !trimmed.starts_with("</script") {
                    flush(&mut blocks, &mut chunk, chunk_language);
                    chunk_language = "javascript";
                    embedded_language = Some("javascript");
                } else if trimmed.starts_with("<style") && !trimmed.starts_with("</style") {
                    flush(&mut blocks, &mut chunk, chunk_language);
                    chunk_language = "css";
                    embedded_language = Some("css");
                }
            }
        }
    }
    flush(&mut blocks, &mut chunk, chunk_language);

    blocks.join("\n\n")
}

pub fn fenced_code_block(language: &str, body: &str) -> String {
    let mut longest_run = 0_usize;
    let mut current_run = 0_usize;
    for ch in body.chars() {
        if ch == '`' {
            current_run += 1;
            longest_run = longest_run.max(current_run);
        } else {
            current_run = 0;
        }
    }
    let fence = "`".repeat((longest_run + 1).max(3));
    format!("{fence}{language}\n{body}\n{fence}")
}

pub fn syntax_language(content_type: Option<&str>, body: &str) -> &'static str {
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
    if content_type.contains("json") || looks_like_json(body) {
        "json"
    } else if content_type.contains("html") || looks_like_html(body) {
        "html"
    } else if content_type.contains("css") {
        "css"
    } else if content_type.contains("javascript")
        || content_type.contains("ecmascript")
        || content_type.contains("typescript")
    {
        "javascript"
    } else if content_type.contains("xml") {
        "xml"
    } else {
        "text"
    }
}

pub fn format_response_body(body: &str, content_type: Option<&str>) -> FormattedBody {
    let language = syntax_language(content_type, body);
    let text = match language {
        "json" => serde_json::from_str::<serde_json::Value>(body)
            .and_then(|value| serde_json::to_string_pretty(&value))
            .unwrap_or_else(|_| body.to_owned()),
        "html" | "xml" => format_markup(body),
        "css" | "javascript" => format_braced_source(body),
        _ => body.to_owned(),
    };
    FormattedBody { text, language }
}

fn looks_like_json(body: &str) -> bool {
    let body = body.trim();
    (body.starts_with('{') && body.ends_with('}')) || (body.starts_with('[') && body.ends_with(']'))
}

fn looks_like_html(body: &str) -> bool {
    let body = body.trim_start().to_ascii_lowercase();
    body.starts_with("<!doctype html") || body.starts_with("<html")
}

fn format_markup(source: &str) -> String {
    const VOID_TAGS: [&str; 14] = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];

    let mut lines = Vec::new();
    let mut indent = 0_usize;
    let mut rest = source.trim();
    while !rest.is_empty() {
        let Some(open) = rest.find('<') else {
            push_line(&mut lines, indent, rest);
            break;
        };
        push_line(&mut lines, indent, &rest[..open]);
        rest = &rest[open..];
        let Some(close) = rest.find('>') else {
            push_line(&mut lines, indent, rest);
            break;
        };
        let tag = &rest[..=close];
        let normalized = tag
            .trim_start_matches('<')
            .trim_start_matches('/')
            .trim_start_matches('!')
            .trim_start_matches('?')
            .split(|ch: char| ch.is_whitespace() || ch == '>' || ch == '/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let is_closing = tag.starts_with("</");
        let is_special = tag.starts_with("<!") || tag.starts_with("<?");
        let is_void = tag.trim_end().ends_with("/>")
            || is_special
            || VOID_TAGS.contains(&normalized.as_str());
        if is_closing {
            indent = indent.saturating_sub(1);
        }
        push_line(&mut lines, indent, tag);
        if !is_closing && !is_void {
            indent += 1;
        }
        rest = &rest[close + 1..];
        if !is_closing && matches!(normalized.as_str(), "script" | "style") {
            let closing_tag = format!("</{normalized}");
            if let Some(closing_start) = rest.to_ascii_lowercase().find(&closing_tag) {
                let embedded = &rest[..closing_start];
                for line in format_braced_source(embedded).lines() {
                    push_line(&mut lines, indent, line);
                }
                rest = &rest[closing_start..];
            }
        }
    }
    lines.join("\n")
}

fn format_braced_source(source: &str) -> String {
    let mut output = String::with_capacity(source.len() + source.len() / 4);
    let mut indent = 0_usize;
    let mut quote = None;
    let mut escaped = false;
    let mut pending_space = false;

    for ch in source.chars() {
        if let Some(delimiter) = quote {
            output.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
            continue;
        }

        match ch {
            '\'' | '"' | '`' => {
                write_pending_space(&mut output, &mut pending_space);
                quote = Some(ch);
                output.push(ch);
            }
            '{' => {
                trim_trailing_space(&mut output);
                output.push_str(" {\n");
                indent += 1;
                write_indent(&mut output, indent);
                pending_space = false;
            }
            '}' => {
                trim_trailing_whitespace(&mut output);
                output.push('\n');
                indent = indent.saturating_sub(1);
                write_indent(&mut output, indent);
                output.push('}');
                pending_space = false;
            }
            ';' => {
                trim_trailing_space(&mut output);
                output.push_str(";\n");
                write_indent(&mut output, indent);
                pending_space = false;
            }
            '\n' | '\r' | '\t' | ' ' => pending_space = true,
            _ => {
                write_pending_space(&mut output, &mut pending_space);
                output.push(ch);
            }
        }
    }
    wrap_long_code_lines(output.trim(), 160)
}

fn wrap_long_code_lines(source: &str, max_columns: usize) -> String {
    let mut output = String::with_capacity(source.len() + source.len() / max_columns);
    let mut quote = None;
    let mut escaped = false;
    let mut column = 0_usize;
    let mut line_indent = String::new();
    let mut at_line_start = true;

    for ch in source.chars() {
        if ch == '\n' {
            output.push(ch);
            column = 0;
            line_indent.clear();
            at_line_start = true;
            continue;
        }

        if at_line_start {
            if ch == ' ' || ch == '\t' {
                line_indent.push(ch);
            } else {
                at_line_start = false;
            }
        }

        output.push(ch);
        column += 1;

        if let Some(delimiter) = quote {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == delimiter {
                quote = None;
            }
        } else if matches!(ch, '\'' | '"' | '`') {
            quote = Some(ch);
        } else if ch == ',' && column >= max_columns {
            output.push('\n');
            output.push_str(&line_indent);
            output.push_str("  ");
            column = line_indent.chars().count() + 2;
            at_line_start = false;
        }
    }

    output
}

fn push_line(lines: &mut Vec<String>, indent: usize, value: &str) {
    let value = value.trim();
    if !value.is_empty() {
        lines.push(format!("{}{}", "  ".repeat(indent), value));
    }
}

fn write_indent(output: &mut String, indent: usize) {
    output.push_str(&"  ".repeat(indent));
}

fn write_pending_space(output: &mut String, pending_space: &mut bool) {
    if *pending_space && !output.ends_with([' ', '\n']) {
        output.push(' ');
    }
    *pending_space = false;
}

fn trim_trailing_space(output: &mut String) {
    while output.ends_with(' ') {
        output.pop();
    }
}

fn trim_trailing_whitespace(output: &mut String) {
    while output.ends_with([' ', '\n', '\t', '\r']) {
        output.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_json() {
        let formatted =
            format_response_body(r#"{"ok":true,"items":[1,2]}"#, Some("application/json"));
        assert_eq!(formatted.language, "json");
        assert!(formatted.text.contains("\n  \"ok\": true"));
    }

    #[test]
    fn formats_html_with_indentation() {
        let formatted = format_response_body(
            "<!doctype html><html><body><h1>Hello</h1></body></html>",
            Some("text/html"),
        );
        assert_eq!(formatted.language, "html");
        assert!(formatted.text.contains("  <body>"));
        assert!(formatted.text.contains("    <h1>"));
    }

    #[test]
    fn formats_embedded_scripts_into_bounded_lines() {
        let values = (0..200)
            .map(|number| number.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let formatted = format_response_body(
            &format!("<html><script>const values=[{values}];</script></html>"),
            Some("text/html"),
        );

        assert!(formatted.text.contains("<script>\n"));
        assert!(
            formatted
                .text
                .lines()
                .all(|line| line.chars().count() <= 170)
        );
        let markdown = chunked_fenced_code_blocks(formatted.language, &formatted.text);
        assert!(markdown.contains("```html\n"));
        assert!(markdown.contains("```javascript\n"));
    }

    #[test]
    fn formats_css_and_preserves_quoted_semicolons() {
        let formatted = format_response_body(
            r#"body{color:red;background:url("data:x;y");}"#,
            Some("text/css"),
        );
        assert_eq!(formatted.language, "css");
        assert!(formatted.text.contains("color:red;\n"));
        assert!(formatted.text.contains(r#"url("data:x;y")"#));
    }

    #[test]
    fn chunks_long_utf8_lines_without_splitting_characters() {
        let body = "é".repeat(10_000);
        let segments = bounded_utf8_segments(&body);

        assert!(
            segments
                .iter()
                .all(|segment| segment.len() <= HIGHLIGHT_CHUNK_TARGET_BYTES)
        );
        assert_eq!(segments.concat(), body);
    }

    #[test]
    fn precomputes_both_views_for_a_large_response_body() {
        let body = format!(
            "[{}]",
            (0..25_000)
                .map(|number| number.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        let cache = ResponseBodyCache::new(&body, Some("application/json"));

        assert_eq!(cache.raw.text, body);
        assert_eq!(cache.raw.language, "json");
        assert!(cache.formatted.display.text.contains("\n  24999\n"));
        assert_eq!(cache.formatted.display.language, "json");
        assert!(cache.formatted.markdown.starts_with("```json\n"));
        assert!(cache.formatted.markdown.contains("\n  24999\n"));
        assert!(cache.formatted.markdown.ends_with("\n```"));
        assert!(cache.formatted.markdown.matches("```json\n").count() > 10);
    }
}
