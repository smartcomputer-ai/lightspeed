//! Patch hunk seek/matching helpers.
//!
//! Adapted from Codex's `codex-apply-patch` matcher.

pub(crate) fn seek_sequence(
    lines: &[String],
    pattern: &[String],
    start: usize,
    eof: bool,
) -> Option<usize> {
    if pattern.is_empty() {
        return Some(start);
    }
    if pattern.len() > lines.len() {
        return None;
    }

    let search_start = if eof && lines.len() >= pattern.len() {
        lines.len() - pattern.len()
    } else {
        start
    };

    for index in search_start..=lines.len().saturating_sub(pattern.len()) {
        if lines[index..index + pattern.len()] == *pattern {
            return Some(index);
        }
    }

    for index in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut matches = true;
        for (pattern_index, expected) in pattern.iter().enumerate() {
            if lines[index + pattern_index].trim_end() != expected.trim_end() {
                matches = false;
                break;
            }
        }
        if matches {
            return Some(index);
        }
    }

    for index in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut matches = true;
        for (pattern_index, expected) in pattern.iter().enumerate() {
            if lines[index + pattern_index].trim() != expected.trim() {
                matches = false;
                break;
            }
        }
        if matches {
            return Some(index);
        }
    }

    for index in search_start..=lines.len().saturating_sub(pattern.len()) {
        let mut matches = true;
        for (pattern_index, expected) in pattern.iter().enumerate() {
            if normalize_line(&lines[index + pattern_index]) != normalize_line(expected) {
                matches = false;
                break;
            }
        }
        if matches {
            return Some(index);
        }
    }

    None
}

fn normalize_line(line: &str) -> String {
    line.trim()
        .chars()
        .map(|character| match character {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => '\'',
            '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => '"',
            '\u{00A0}' | '\u{2002}' | '\u{2003}' | '\u{2004}' | '\u{2005}' | '\u{2006}'
            | '\u{2007}' | '\u{2008}' | '\u{2009}' | '\u{200A}' | '\u{202F}' | '\u{205F}'
            | '\u{3000}' => ' ',
            other => other,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::seek_sequence;

    fn lines(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn seek_sequence_finds_exact_match() {
        assert_eq!(
            seek_sequence(
                &lines(&["foo", "bar", "baz"]),
                &lines(&["bar", "baz"]),
                0,
                false
            ),
            Some(1)
        );
    }

    #[test]
    fn seek_sequence_ignores_trailing_whitespace() {
        assert_eq!(
            seek_sequence(
                &lines(&["foo   ", "bar\t"]),
                &lines(&["foo", "bar"]),
                0,
                false
            ),
            Some(0)
        );
    }

    #[test]
    fn seek_sequence_returns_none_when_pattern_is_longer() {
        assert_eq!(
            seek_sequence(&lines(&["one"]), &lines(&["one", "two"]), 0, false),
            None
        );
    }

    #[test]
    fn seek_sequence_normalizes_common_unicode_punctuation() {
        assert_eq!(
            seek_sequence(
                &lines(&["local import \u{2013} top\u{2011}level"]),
                &lines(&["local import - top-level"]),
                0,
                false
            ),
            Some(0)
        );
    }
}
