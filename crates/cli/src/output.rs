//! Human-readable records and tables; machine output always remains JSON.
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;

pub(crate) fn show(json: bool, value: &impl Serialize) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", render(&serde_json::to_value(value)?, 0));
    }
    Ok(())
}

pub(crate) fn table(value: &impl Serialize, columns: &[(&str, &str)], empty: &str) -> Result<()> {
    let value = serde_json::to_value(value)?;
    let rows = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("table requires an array"))?;
    if rows.is_empty() {
        println!("{empty}");
        return Ok(());
    }
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|row| columns.iter().map(|(key, _)| scalar(&row[*key])).collect())
        .collect();
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, (_, title))| {
            cells
                .iter()
                .map(|row| row[i].chars().count())
                .max()
                .unwrap_or(0)
                .max(title.len())
        })
        .collect();
    let print_row = |row: Vec<String>| {
        println!(
            "{}",
            row.iter()
                .enumerate()
                .map(|(i, cell)| format!(
                    "{}{}",
                    cell,
                    " ".repeat(widths[i].saturating_sub(cell.chars().count()))
                ))
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
        )
    };
    print_row(columns.iter().map(|(_, title)| title.to_string()).collect());
    for row in cells {
        print_row(row);
    }
    Ok(())
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "—".into(),
        Value::String(s) => s
            .chars()
            .flat_map(|c| {
                if c.is_control() {
                    c.escape_default().collect::<Vec<_>>()
                } else {
                    vec![c]
                }
            })
            .collect(),
        _ => value.to_string(),
    }
}

fn label(key: &str) -> String {
    let mut result = String::new();
    for (i, c) in key.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            result.push(' ');
        }
        if i == 0 {
            result.extend(c.to_uppercase());
        } else {
            result.extend(c.to_lowercase());
        }
    }
    result
}

fn render(value: &Value, indent: usize) -> String {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(fields) if !fields.is_empty() => fields
            .iter()
            .map(|(key, value)| {
                if value.is_object() || value.is_array() {
                    format!("{pad}{}:\n{}", label(key), render(value, indent + 2))
                } else {
                    format!("{pad}{}: {}", label(key), scalar(value))
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Array(items) if !items.is_empty() => items
            .iter()
            .map(|value| render(value, indent))
            .collect::<Vec<_>>()
            .join("\n\n"),
        Value::Object(_) | Value::Array(_) => format!("{pad}(none)"),
        _ => format!("{pad}{}", scalar(value)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn records_distinguish_empty_and_missing_and_escape_terminal_control() {
        let text = render(
            &serde_json::json!({"displayName":"name\u{1b}[2J", "activeRun":null, "attachments":[]}),
            0,
        );
        assert!(text.contains("Display name: name\\u{1b}[2J"));
        assert!(text.contains("Active run: —"));
        assert!(text.contains("Attachments:\n  (none)"));
    }
}
