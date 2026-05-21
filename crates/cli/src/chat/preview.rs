pub(crate) fn compact_preview(value: &str, max_chars: usize) -> String {
    let out = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if max_chars == 0 {
        return String::new();
    }
    if out.chars().count() <= max_chars {
        return out;
    }
    if max_chars <= 3 {
        return ".".repeat(max_chars);
    }

    let mut truncated = out
        .chars()
        .take(max_chars.saturating_sub(3))
        .collect::<String>();
    truncated.push_str("...");
    truncated
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_preview_truncates_on_char_boundary() {
        let preview = compact_preview(&"é".repeat(100), 80);

        assert!(preview.ends_with("..."));
        assert_eq!(preview.chars().count(), 80);
    }

    #[test]
    fn compact_preview_collapses_whitespace() {
        assert_eq!(
            compact_preview("alpha\n\n beta\tgamma", 80),
            "alpha beta gamma"
        );
    }
}
