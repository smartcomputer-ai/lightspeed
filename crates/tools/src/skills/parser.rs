use std::collections::BTreeMap;

use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillFrontmatter {
    pub name: String,
    pub description: String,
    pub short_description: Option<String>,
    pub raw_frontmatter: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum SkillParseError {
    #[error("SKILL.md is missing YAML frontmatter")]
    MissingFrontmatter,

    #[error("SKILL.md frontmatter is not terminated")]
    UnterminatedFrontmatter,

    #[error("invalid frontmatter line {line}: expected 'key: value'")]
    InvalidFrontmatterLine { line: usize },

    #[error("frontmatter field '{field}' is missing or empty")]
    MissingRequiredField { field: &'static str },
}

pub fn parse_skill_frontmatter(markdown: &str) -> Result<SkillFrontmatter, SkillParseError> {
    let mut lines = markdown.lines();
    let Some(first) = lines.next() else {
        return Err(SkillParseError::MissingFrontmatter);
    };
    if first.trim() != "---" {
        return Err(SkillParseError::MissingFrontmatter);
    }

    let mut raw_frontmatter = String::new();
    let mut values = BTreeMap::<String, String>::new();
    let mut terminated = false;

    for (index, line) in lines.enumerate() {
        let line_number = index + 2;
        if line.trim() == "---" {
            terminated = true;
            break;
        }

        raw_frontmatter.push_str(line);
        raw_frontmatter.push('\n');

        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if line.starts_with(char::is_whitespace) || trimmed.starts_with("- ") {
            continue;
        }

        let Some((key, value)) = trimmed.split_once(':') else {
            return Err(SkillParseError::InvalidFrontmatterLine { line: line_number });
        };
        let key = key.trim();
        let value = unquote_scalar(value.trim());
        if key.is_empty() {
            return Err(SkillParseError::InvalidFrontmatterLine { line: line_number });
        }
        values.insert(key.to_owned(), value.to_owned());
    }

    if !terminated {
        return Err(SkillParseError::UnterminatedFrontmatter);
    }

    let name = required_string(&values, "name")?;
    let description = required_string(&values, "description")?;
    let short_description = values
        .get("short_description")
        .or_else(|| values.get("short-description"))
        .filter(|value| !value.is_empty())
        .cloned();

    Ok(SkillFrontmatter {
        name,
        description,
        short_description,
        raw_frontmatter,
    })
}

fn required_string(
    values: &BTreeMap<String, String>,
    field: &'static str,
) -> Result<String, SkillParseError> {
    values
        .get(field)
        .filter(|value| !value.is_empty())
        .cloned()
        .ok_or(SkillParseError::MissingRequiredField { field })
}

fn unquote_scalar(value: &str) -> &str {
    if value.len() >= 2 {
        let first = value.as_bytes()[0];
        let last = value.as_bytes()[value.len() - 1];
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return &value[1..value.len() - 1];
        }
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_required_skill_frontmatter() {
        let frontmatter = parse_skill_frontmatter(
            r#"---
name: deploy-review
description: Use when reviewing deployment risk.
short_description: Review deploys
---

Body
"#,
        )
        .expect("parse frontmatter");

        assert_eq!(frontmatter.name, "deploy-review");
        assert_eq!(
            frontmatter.description,
            "Use when reviewing deployment risk."
        );
        assert_eq!(
            frontmatter.short_description.as_deref(),
            Some("Review deploys")
        );
    }

    #[test]
    fn rejects_missing_required_frontmatter_fields() {
        let error = parse_skill_frontmatter(
            r#"---
name: deploy-review
---
"#,
        )
        .expect_err("missing description");

        assert_eq!(
            error,
            SkillParseError::MissingRequiredField {
                field: "description"
            }
        );
    }

    #[test]
    fn rejects_non_scalar_frontmatter_lines() {
        let error = parse_skill_frontmatter(
            r#"---
name: deploy-review
description
---
"#,
        )
        .expect_err("invalid line");

        assert_eq!(error, SkillParseError::InvalidFrontmatterLine { line: 3 });
    }
}
