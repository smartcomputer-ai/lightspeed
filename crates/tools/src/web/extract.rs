//! Minimal text extraction for fetched web responses.

use regex::Regex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WebContentKind {
    Html,
    Json,
    Text,
}

pub(crate) fn classify_content_type(content_type: Option<&str>) -> Option<WebContentKind> {
    let content_type = content_type?;
    let media_type = content_type
        .split(';')
        .next()
        .unwrap_or(content_type)
        .trim()
        .to_ascii_lowercase();
    match media_type.as_str() {
        "text/html" | "application/xhtml+xml" => Some(WebContentKind::Html),
        "application/json" | "text/json" => Some(WebContentKind::Json),
        "text/plain" | "text/markdown" | "text/x-markdown" | "application/markdown" => {
            Some(WebContentKind::Text)
        }
        media_type if media_type.ends_with("+json") => Some(WebContentKind::Json),
        _ => None,
    }
}

pub(crate) fn extract_text(bytes: &[u8], kind: WebContentKind, max_chars: usize) -> (String, bool) {
    let text = String::from_utf8_lossy(bytes);
    let extracted = match kind {
        WebContentKind::Html => html_to_text(&text),
        WebContentKind::Json | WebContentKind::Text => normalize_whitespace_lines(&text),
    };
    truncate_chars(extracted, max_chars)
}

fn html_to_text(html: &str) -> String {
    let without_scripts = regex_replace(
        "(?is)<script\\b[^>]*>.*?</script>|<style\\b[^>]*>.*?</style>|<noscript\\b[^>]*>.*?</noscript>",
        html,
        " ",
    );
    let with_breaks = regex_replace(
        "(?i)<\\s*(br|/p|/div|/li|/h[1-6]|/tr)\\b[^>]*>",
        &without_scripts,
        "\n",
    );
    let without_tags = regex_replace("(?is)<[^>]+>", &with_breaks, " ");
    normalize_whitespace_lines(&decode_html_entities(&without_tags))
}

fn regex_replace(pattern: &str, text: &str, replacement: &str) -> String {
    Regex::new(pattern)
        .expect("valid regex")
        .replace_all(text, replacement)
        .into_owned()
}

fn decode_html_entities(text: &str) -> String {
    text.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

fn normalize_whitespace_lines(text: &str) -> String {
    let mut output = String::new();
    let mut previous_blank = false;
    for line in text.lines() {
        let normalized = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty() {
            if !previous_blank && !output.is_empty() {
                output.push('\n');
                previous_blank = true;
            }
            continue;
        }
        if !output.is_empty() && !output.ends_with('\n') {
            output.push('\n');
        }
        output.push_str(&normalized);
        previous_blank = false;
    }
    output.trim().to_owned()
}

fn truncate_chars(text: String, max_chars: usize) -> (String, bool) {
    if text.chars().count() <= max_chars {
        return (text, false);
    }
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    truncated.push_str("\n[truncated]");
    (truncated, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_allowed_content_types() {
        assert_eq!(
            classify_content_type(Some("text/html; charset=utf-8")),
            Some(WebContentKind::Html)
        );
        assert_eq!(
            classify_content_type(Some("application/activity+json")),
            Some(WebContentKind::Json)
        );
        assert_eq!(
            classify_content_type(Some("application/octet-stream")),
            None
        );
    }

    #[test]
    fn extracts_html_text_without_scripts() {
        let html = r#"
            <html>
              <head><style>.x{}</style><script>alert(1)</script></head>
              <body><h1>Title &amp; More</h1><p>Hello<br>world</p></body>
            </html>
        "#;

        let (text, truncated) = extract_text(html.as_bytes(), WebContentKind::Html, 1000);

        assert!(!truncated);
        assert!(text.contains("Title & More"));
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("alert"));
    }

    #[test]
    fn truncates_by_chars() {
        let (text, truncated) = extract_text("abcdef".as_bytes(), WebContentKind::Text, 3);

        assert!(truncated);
        assert_eq!(text, "abc\n[truncated]");
    }
}
