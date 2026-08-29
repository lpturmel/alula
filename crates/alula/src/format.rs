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

/// Remove layout-only whitespace before a response enters the formatted view.
/// The raw response remains unchanged in `ResponseBodyCache`.
pub fn trim_response_formatting_start(body: &str) -> &str {
    body.trim_start_matches([' ', '\t', '\r', '\n'])
}

const HIGHLIGHT_CHUNK_TARGET_BYTES: usize = 2 * 1024;

#[cfg(test)]
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
    let estimated_blocks = body.len().div_ceil(HIGHLIGHT_CHUNK_TARGET_BYTES);
    let mut output = String::with_capacity(
        body.len()
            .saturating_add(estimated_blocks.saturating_mul(language.len() + 10)),
    );
    let mut chunk_start = None;
    let mut chunk_end = 0_usize;
    let mut chunk_language = language;
    let mut embedded_language = None;

    let flush =
        |output: &mut String, chunk_start: &mut Option<usize>, chunk_end: usize, language: &str| {
            if let Some(start) = chunk_start.take() {
                append_fenced_code_block(
                    output,
                    language,
                    body[start..chunk_end].trim_end_matches('\n'),
                );
            }
        };

    let mut line_start = 0_usize;
    for line in body.split_inclusive('\n') {
        let mut remaining = line;
        let mut consumed = 0_usize;
        while !remaining.is_empty() {
            let end = bounded_utf8_end(remaining);
            let segment = &remaining[..end];
            remaining = &remaining[end..];
            let segment_start = line_start + consumed;
            let segment_end = segment_start + segment.len();
            consumed += segment.len();
            let trimmed = segment.trim_start();
            let line_language = if matches!(language, "html" | "xml") {
                if starts_with_ignore_ascii_case(trimmed, "</script")
                    || starts_with_ignore_ascii_case(trimmed, "</style")
                {
                    embedded_language = None;
                    language
                } else {
                    embedded_language.unwrap_or(language)
                }
            } else {
                language
            };

            let chunk_len = chunk_start
                .map(|start| chunk_end.saturating_sub(start))
                .unwrap_or_default();
            if line_language != chunk_language
                || (chunk_start.is_some()
                    && chunk_len.saturating_add(segment.len()) > HIGHLIGHT_CHUNK_TARGET_BYTES)
            {
                flush(&mut output, &mut chunk_start, chunk_end, chunk_language);
                chunk_language = line_language;
            }
            chunk_start.get_or_insert(segment_start);
            chunk_end = segment_end;

            if matches!(language, "html" | "xml") {
                if starts_with_ignore_ascii_case(trimmed, "<script")
                    && !starts_with_ignore_ascii_case(trimmed, "</script")
                {
                    flush(&mut output, &mut chunk_start, chunk_end, chunk_language);
                    chunk_language = "javascript";
                    embedded_language = Some("javascript");
                } else if starts_with_ignore_ascii_case(trimmed, "<style")
                    && !starts_with_ignore_ascii_case(trimmed, "</style")
                {
                    flush(&mut output, &mut chunk_start, chunk_end, chunk_language);
                    chunk_language = "css";
                    embedded_language = Some("css");
                }
            }
        }
        line_start += line.len();
    }
    flush(&mut output, &mut chunk_start, chunk_end, chunk_language);

    output
}

pub fn fenced_code_block(language: &str, body: &str) -> String {
    let mut output = String::with_capacity(body.len().saturating_add(language.len() + 10));
    append_fenced_code_block(&mut output, language, body);
    output
}

fn append_fenced_code_block(output: &mut String, language: &str, body: &str) {
    append_fenced_code_block_with_length(output, language, body, fenced_code_block_length(body));
}

fn fenced_code_block_length(body: &str) -> usize {
    if body.as_bytes().contains(&b'`') {
        let mut longest_run = 0_usize;
        let mut current_run = 0_usize;
        for byte in body.bytes() {
            if byte == b'`' {
                current_run += 1;
                longest_run = longest_run.max(current_run);
            } else {
                current_run = 0;
            }
        }
        (longest_run + 1).max(3)
    } else {
        3
    }
}

fn append_fenced_code_block_with_length(
    output: &mut String,
    language: &str,
    body: &str,
    fence_length: usize,
) {
    if !output.is_empty() {
        output.push_str("\n\n");
    }
    output.extend(std::iter::repeat_n('`', fence_length));
    output.push_str(language);
    output.push('\n');
    output.push_str(body);
    output.push('\n');
    output.extend(std::iter::repeat_n('`', fence_length));
}

fn bounded_utf8_end(text: &str) -> usize {
    if text.len() <= HIGHLIGHT_CHUNK_TARGET_BYTES {
        return text.len();
    }
    let mut end = HIGHLIGHT_CHUNK_TARGET_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

fn starts_with_ignore_ascii_case(value: &str, prefix: &str) -> bool {
    value
        .get(..prefix.len())
        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(prefix))
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
    let body = trim_response_formatting_start(body);
    let language = syntax_language(content_type, body);
    let text = match language {
        "json" => format_json(body).unwrap_or_else(|| body.to_owned()),
        "html" | "xml" => format_markup(body),
        "css" | "javascript" => format_braced_source(body),
        _ => body.to_owned(),
    };
    FormattedBody { text, language }
}

/// Formats an editable request body while reporting invalid or unsupported
/// input instead of silently returning it unchanged.
pub fn format_request_body(
    body: &str,
    content_type: Option<&str>,
) -> Result<FormattedBody, String> {
    let body = body.trim();
    if body.is_empty() {
        return Err("the request body is empty".into());
    }
    let language = syntax_language(content_type, body);
    let text = match language {
        "json" => format_json_result(body)?,
        "html" | "xml" => format_markup(body),
        "css" | "javascript" => format_braced_source(body),
        _ => {
            return Err("the body format could not be detected; add a Content-Type header".into());
        }
    };
    Ok(FormattedBody { text, language })
}

/// Pretty-print JSON directly from the parser into the output buffer. This
/// avoids building a complete `serde_json::Value` tree and then walking it a
/// second time, substantially reducing allocations for large responses.
fn format_json(body: &str) -> Option<String> {
    format_json_result(body).ok()
}

fn format_json_result(body: &str) -> Result<String, String> {
    let mut deserializer = serde_json::Deserializer::from_str(body);
    let mut output = Vec::with_capacity(body.len().saturating_add(body.len() / 2));
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut output, formatter);
    serde_transcode::transcode(&mut deserializer, &mut serializer)
        .map_err(|error| error.to_string())?;
    deserializer.end().map_err(|error| error.to_string())?;
    String::from_utf8(output).map_err(|error| error.to_string())
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
    fn formats_request_json_and_reports_invalid_input() {
        let formatted =
            format_request_body(r#"{"ok":true,"items":[1,2]}"#, Some("application/json")).unwrap();
        assert_eq!(formatted.language, "json");
        assert_eq!(
            formatted.text,
            "{\n  \"ok\": true,\n  \"items\": [\n    1,\n    2\n  ]\n}"
        );

        let error = format_request_body(r#"{"ok":}"#, Some("application/json")).unwrap_err();
        assert!(!error.is_empty());
    }

    #[test]
    fn request_formatter_detects_json_without_a_content_type() {
        let formatted = format_request_body("[1,2]", None).unwrap();
        assert_eq!(formatted.language, "json");
        assert_eq!(formatted.text, "[\n  1,\n  2\n]");
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

    #[test]
    fn trims_layout_whitespace_only_from_the_formatted_response_start() {
        let body = "\n \t\r{\"ok\":true}\n";
        let cache = ResponseBodyCache::new(body, Some("application/json"));

        assert_eq!(cache.raw.text, body);
        assert_eq!(cache.formatted.display.text, "{\n  \"ok\": true\n}");
        assert!(cache.formatted.markdown.starts_with("```json\n{"));
    }
}
