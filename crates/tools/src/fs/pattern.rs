//! The path pattern grammar shared by every glob and include filter.
//!
//! Patterns match paths relative to the walk's root, gitignore-style: `*`
//! and `?` match within one segment, `**` spans segments, `[...]` is a
//! character class and `{a,b}` an alternation. A pattern without `/`
//! matches an entry's name at any depth; a `/` anywhere but at the end
//! anchors it to the root; a trailing `/` matches directories only. The
//! backslash is an ordinary character, as in POSIX names. Environment
//! daemons apply the same grammar to `fs/globFiles`, `fs/searchText`,
//! `fs/scan` and transfer captures, so the generic fallback and a native
//! walk select the same entries.

use globset::{GlobBuilder, GlobMatcher};

/// One compiled path pattern.
#[derive(Clone, Debug)]
pub struct PathPattern {
    matcher: GlobMatcher,
    /// The pattern ended in `/`: only directories match.
    directory_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("invalid path pattern {pattern:?}: {message}")]
pub struct PathPatternError {
    pub pattern: String,
    pub message: String,
}

impl PathPattern {
    pub fn new(pattern: &str, ignore_case: bool) -> Result<Self, PathPatternError> {
        let invalid = |message: String| PathPatternError {
            pattern: pattern.to_owned(),
            message,
        };
        let (body, directory_only) = match pattern.strip_suffix('/') {
            Some(body) if !body.is_empty() => (body, true),
            _ => (pattern, false),
        };
        if body.is_empty() || body == "/" {
            return Err(invalid("pattern must not be empty".into()));
        }
        let glob = if let Some(anchored) = body.strip_prefix('/') {
            anchored.to_owned()
        } else if body.contains('/') {
            body.to_owned()
        } else {
            format!("**/{body}")
        };
        let matcher = GlobBuilder::new(&glob)
            .literal_separator(true)
            .backslash_escape(false)
            .case_insensitive(ignore_case)
            .build()
            .map_err(|error| invalid(error.kind().to_string()))?
            .compile_matcher();
        Ok(Self {
            matcher,
            directory_only,
        })
    }

    /// Whether the entry at `relative` (segments joined by `/`, no leading
    /// `/`) below the walk's root matches; `is_dir` says whether that entry
    /// is a directory.
    pub fn matches(&self, relative: &str, is_dir: bool) -> bool {
        (is_dir || !self.directory_only) && self.matcher.is_match(relative)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(pattern: &str, path: &str) -> bool {
        PathPattern::new(pattern, false)
            .unwrap()
            .matches(path, false)
    }

    #[test]
    fn star_stays_within_one_segment() {
        assert!(matches("src/*.rs", "src/lib.rs"));
        assert!(!matches("src/*.rs", "src/deep/nested.rs"));
        assert!(matches("src/**/*.rs", "src/deep/nested.rs"));
        assert!(matches("**/*.rs", "lib.rs"));
    }

    #[test]
    fn a_pattern_without_a_slash_matches_names_at_any_depth() {
        assert!(matches("*.md", "README.md"));
        assert!(matches("*.md", "docs/guide/intro.md"));
        assert!(matches("SKILL.md", "review/SKILL.md"));
    }

    #[test]
    fn a_leading_slash_anchors_to_the_root() {
        assert!(matches("/*.md", "README.md"));
        assert!(!matches("/*.md", "docs/intro.md"));
        assert!(matches("/docs/*.md", "docs/intro.md"));
    }

    #[test]
    fn classes_alternations_and_case() {
        assert!(matches("*.{rs,toml}", "Cargo.toml"));
        assert!(matches("file[0-9].txt", "dir/file7.txt"));
        assert!(!matches("*.RS", "lib.rs"));
        assert!(
            PathPattern::new("*.RS", true)
                .unwrap()
                .matches("lib.rs", false)
        );
        assert!(PathPattern::new("", false).is_err());
        assert!(PathPattern::new("/", false).is_err());
        assert!(PathPattern::new("[", false).is_err());
    }

    #[test]
    fn a_slash_inside_anchors_like_a_leading_one() {
        assert!(matches("docs/*.md", "docs/intro.md"));
        assert!(!matches("docs/*.md", "nested/docs/intro.md"));
    }

    #[test]
    fn a_trailing_slash_matches_directories_only() {
        let build = PathPattern::new("build/", false).unwrap();
        assert!(build.matches("build", true));
        assert!(build.matches("nested/build", true));
        assert!(!build.matches("build", false));
        let anchored = PathPattern::new("/build/", false).unwrap();
        assert!(anchored.matches("build", true));
        assert!(!anchored.matches("nested/build", true));
    }

    #[test]
    fn the_backslash_is_an_ordinary_character() {
        assert!(matches("a\\*.txt", "a\\b.txt"));
        assert!(!matches("a\\*.txt", "a*.txt"));
        assert!(matches("*.txt", "dir/back\\slash.txt"));
    }
}
