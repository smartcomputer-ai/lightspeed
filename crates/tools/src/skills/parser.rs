use serde_yaml::{Mapping, Value};

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

    #[error("invalid YAML frontmatter: {message}")]
    InvalidYaml { message: String },

    #[error("frontmatter field '{field}' is missing or empty")]
    MissingRequiredField { field: &'static str },

    #[error("frontmatter field '{field}' is longer than {limit} characters")]
    FieldTooLong { field: &'static str, limit: usize },

    #[error("frontmatter field '{field}' contains a control character")]
    ControlCharacter { field: &'static str },
}

/// Longest skill name, in characters.
pub const SKILL_NAME_MAX_CHARS: usize = 128;
/// Longest skill description, in characters.
pub const SKILL_DESCRIPTION_MAX_CHARS: usize = 2_048;
/// Longest skill short description, in characters.
pub const SKILL_SHORT_DESCRIPTION_MAX_CHARS: usize = 256;

const NAME_FIELD: &str = "name";
const DESCRIPTION_FIELD: &str = "description";
const SHORT_DESCRIPTION_FIELD: &str = "short_description";

pub fn parse_skill_frontmatter(markdown: &str) -> Result<SkillFrontmatter, SkillParseError> {
    let mut lines = markdown.lines();
    let Some(first) = lines.next() else {
        return Err(SkillParseError::MissingFrontmatter);
    };
    if first.trim() != "---" {
        return Err(SkillParseError::MissingFrontmatter);
    }

    let mut raw_frontmatter = String::new();
    let mut terminated = false;

    for line in lines {
        if line.trim() == "---" {
            terminated = true;
            break;
        }

        raw_frontmatter.push_str(line);
        raw_frontmatter.push('\n');
    }

    if !terminated {
        return Err(SkillParseError::UnterminatedFrontmatter);
    }

    let values: Mapping =
        serde_yaml::from_str(&raw_frontmatter).map_err(|error| SkillParseError::InvalidYaml {
            message: error.to_string(),
        })?;
    let name = bounded_field(
        required_string(&values, NAME_FIELD)?,
        NAME_FIELD,
        SKILL_NAME_MAX_CHARS,
    )?;
    let description = bounded_field(
        required_string(&values, DESCRIPTION_FIELD)?,
        DESCRIPTION_FIELD,
        SKILL_DESCRIPTION_MAX_CHARS,
    )?;
    let short_description = values
        .get(SHORT_DESCRIPTION_FIELD)
        .or_else(|| values.get("short-description"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            bounded_field(
                value,
                SHORT_DESCRIPTION_FIELD,
                SKILL_SHORT_DESCRIPTION_MAX_CHARS,
            )
        })
        .transpose()?;

    Ok(SkillFrontmatter {
        name,
        description,
        short_description,
        raw_frontmatter,
    })
}

fn required_string<'a>(
    values: &'a Mapping,
    field: &'static str,
) -> Result<&'a str, SkillParseError> {
    values
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or(SkillParseError::MissingRequiredField { field })
}

/// A catalog field as every reader gets it: one line, within its limit, free
/// of control characters.
///
/// These fields are copied verbatim into model context and operator displays
/// for every session that can see the skill, whoever wrote the file. A YAML
/// block scalar legitimately carries line breaks and a trailing newline, so
/// whitespace is folded rather than refused; any other control character has
/// no place in a label and fails the skill. The limit counts what is kept.
fn bounded_field(
    value: &str,
    field: &'static str,
    limit: usize,
) -> Result<String, SkillParseError> {
    let value = single_line(value);
    if value.chars().any(char::is_control) {
        return Err(SkillParseError::ControlCharacter { field });
    }
    if value.chars().count() > limit {
        return Err(SkillParseError::FieldTooLong { field, limit });
    }
    Ok(value)
}

/// Fold every run of whitespace, line breaks included, into one space and
/// drop it at the ends.
pub(crate) fn single_line(value: &str) -> String {
    let mut line = String::with_capacity(value.len());
    for word in value.split_whitespace() {
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_and_nested_optional_metadata() {
        let parsed = parse_skill_frontmatter("---\nname: review\ndescription: >-\n  Review code\n  carefully.\nmetadata:\n  hooks: [ignored]\n  nested: {flag: true}\n---\nbody").unwrap();
        assert_eq!(parsed.description, "Review code carefully.");
        assert!(matches!(
            parse_skill_frontmatter("---\nname: []\ndescription: ok\n---"),
            Err(SkillParseError::MissingRequiredField { field: "name" })
        ));
    }

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

    fn doc(name: &str, description: &str, short_description: Option<&str>) -> String {
        let mut frontmatter = serde_yaml::Mapping::new();
        frontmatter.insert("name".into(), name.into());
        frontmatter.insert("description".into(), description.into());
        if let Some(short_description) = short_description {
            frontmatter.insert("short_description".into(), short_description.into());
        }
        format!(
            "---\n{}---\nbody\n",
            serde_yaml::to_string(&frontmatter).expect("write frontmatter")
        )
    }

    #[test]
    fn accepts_fields_exactly_at_their_limits() {
        // Multi-byte characters prove the limits count characters, not bytes.
        let name = "\u{e9}".repeat(SKILL_NAME_MAX_CHARS);
        let description = "\u{4e16}".repeat(SKILL_DESCRIPTION_MAX_CHARS);
        let short_description = "\u{1f600}".repeat(SKILL_SHORT_DESCRIPTION_MAX_CHARS);
        let parsed = parse_skill_frontmatter(&doc(&name, &description, Some(&short_description)))
            .expect("fields at the limit");
        assert_eq!(parsed.name, name);
        assert_eq!(parsed.description, description);
        assert_eq!(
            parsed.short_description.as_deref(),
            Some(&*short_description)
        );
    }

    #[test]
    fn rejects_each_field_one_character_over_its_limit() {
        let ok = "ok";
        let cases = [
            (
                doc(&"n".repeat(SKILL_NAME_MAX_CHARS + 1), ok, None),
                "name",
                SKILL_NAME_MAX_CHARS,
            ),
            (
                doc(ok, &"d".repeat(SKILL_DESCRIPTION_MAX_CHARS + 1), None),
                "description",
                SKILL_DESCRIPTION_MAX_CHARS,
            ),
            (
                doc(
                    ok,
                    ok,
                    Some(&"s".repeat(SKILL_SHORT_DESCRIPTION_MAX_CHARS + 1)),
                ),
                "short_description",
                SKILL_SHORT_DESCRIPTION_MAX_CHARS,
            ),
        ];
        for (markdown, field, limit) in cases {
            assert_eq!(
                parse_skill_frontmatter(&markdown),
                Err(SkillParseError::FieldTooLong { field, limit }),
                "{field}"
            );
        }
    }

    #[test]
    fn hyphenated_short_description_is_held_to_the_same_limit() {
        let markdown = format!(
            "---\nname: review\ndescription: Review.\nshort-description: {}\n---\n",
            "s".repeat(SKILL_SHORT_DESCRIPTION_MAX_CHARS + 1)
        );
        assert_eq!(
            parse_skill_frontmatter(&markdown),
            Err(SkillParseError::FieldTooLong {
                field: "short_description",
                limit: SKILL_SHORT_DESCRIPTION_MAX_CHARS
            })
        );
    }

    #[test]
    fn rejects_control_characters_in_every_field() {
        for control in ["\u{0}", "\u{7}", "\u{1b}", "\u{7f}", "\u{9b}"] {
            let dirty = format!("before{control}after");
            let cases = [
                (doc(&dirty, "ok", None), "name"),
                (doc("ok", &dirty, None), "description"),
                (doc("ok", "ok", Some(&dirty)), "short_description"),
            ];
            for (markdown, field) in cases {
                assert_eq!(
                    parse_skill_frontmatter(&markdown),
                    Err(SkillParseError::ControlCharacter { field }),
                    "{field} with {control:?}"
                );
            }
        }
    }

    #[test]
    fn folds_line_breaks_and_whitespace_runs_into_single_spaces() {
        let parsed = parse_skill_frontmatter(
            "---\nname: \"  deploy\\treview \"\ndescription: |\n  Review the deploy.\n\n  - forged entry\n    path: /elsewhere\nshort_description: \"one\\r\\ntwo\\u2028three\\x85four\"\n---\nbody",
        )
        .expect("block scalars stay valid");
        assert_eq!(parsed.name, "deploy review");
        assert_eq!(
            parsed.description,
            "Review the deploy. - forged entry path: /elsewhere"
        );
        assert_eq!(
            parsed.short_description.as_deref(),
            Some("one two three four")
        );
        for field in [
            Some(parsed.name.as_str()),
            Some(parsed.description.as_str()),
            parsed.short_description.as_deref(),
        ] {
            let field = field.expect("field");
            assert!(!field.chars().any(char::is_control), "{field:?}");
            assert_eq!(field.lines().count(), 1, "{field:?}");
        }
    }

    #[test]
    fn limits_count_the_folded_value() {
        // Folding removes the padding, so the kept text is within the limit.
        let padded = format!("  {}\n\n", "n".repeat(SKILL_NAME_MAX_CHARS));
        let parsed = parse_skill_frontmatter(&doc(&padded, "ok", None)).expect("padded name");
        assert_eq!(parsed.name.chars().count(), SKILL_NAME_MAX_CHARS);
    }

    #[test]
    fn body_is_not_limited() {
        let markdown = format!(
            "---\nname: review\ndescription: Review.\n---\n{}",
            "body line\u{1b}\n".repeat(10_000)
        );
        let parsed = parse_skill_frontmatter(&markdown).expect("long body");
        assert_eq!(parsed.name, "review");
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

        assert!(matches!(error, SkillParseError::InvalidYaml { .. }));
    }
}
