// SPDX-FileCopyrightText: 2026 Semiotic AI, Inc.
//
// SPDX-License-Identifier: Apache-2.0

//! JSON to YAML rendering for grep-friendly line-based access.
//!
//! Long strings become YAML block scalars (`>-`) so they span multiple
//! individually-addressable lines instead of one 10KB mega-line.
//! No external YAML library — hand-rendered subset.

use serde_json::Value;

use super::rendered::{RecordSpan, RenderedContent};

const BLOCK_SCALAR_THRESHOLD: usize = 80;
const WRAP_WIDTH: usize = 78;

/// Render a JSON value to grep-friendly YAML lines.
pub fn render_to_lines(value: &Value) -> RenderedContent {
    let mut lines = Vec::new();
    let mut record_spans = Vec::new();
    match value {
        Value::Array(arr) if arr.is_empty() => {
            lines.push("(empty)".to_string());
        }
        Value::Array(arr) if arr.iter().all(Value::is_object) => {
            render_object_array(arr, "", 2, &mut lines, &mut record_spans);
        }
        Value::Array(arr) => {
            for item in arr {
                lines.push(render_scalar(item));
            }
        }
        Value::Object(fields) => {
            render_root_mapping(fields, &mut lines, &mut record_spans);
        }
        Value::Null => lines.push("null".to_string()),
        _ => lines.push(render_scalar(value)),
    }
    RenderedContent::try_from_lines_with_record_spans(lines, record_spans)
        .expect("JSON renderer must produce valid record spans")
}

fn render_object_array(
    arr: &[Value],
    base_indent: &str,
    continuation_indent: usize,
    lines: &mut Vec<String>,
    record_spans: &mut Vec<RecordSpan>,
) {
    for obj in arr {
        let fields = obj.as_object().expect("checked in caller");
        let start = lines.len();
        render_object_item(
            fields,
            base_indent,
            continuation_indent,
            lines,
            record_spans,
        );
        let end = lines.len();
        if start < end {
            record_spans.push(RecordSpan { start, end });
        }
    }
}

/// Render a root object, emitting record spans for every direct array-of-objects field.
/// Scalar siblings such as `message` remain outside the record spans. Nested arrays inside an
/// item are part of the parent record and do not get independent spans.
fn render_root_mapping(
    fields: &serde_json::Map<String, Value>,
    lines: &mut Vec<String>,
    record_spans: &mut Vec<RecordSpan>,
) {
    for (key, val) in fields {
        render_field("", key, val, 2, lines, record_spans, true);
    }
}

/// Render one object as a YAML sequence item using `base_indent` for the
/// `"- "` / `"  "` first-vs-continuation prefix.
fn render_object_item(
    fields: &serde_json::Map<String, Value>,
    base_indent: &str,
    continuation_indent: usize,
    lines: &mut Vec<String>,
    record_spans: &mut Vec<RecordSpan>,
) {
    for (fi, (key, val)) in fields.iter().enumerate() {
        let prefix = if fi == 0 {
            format!("{base_indent}- ")
        } else {
            format!("{base_indent}  ")
        };
        // Nested arrays inside a record item belong to the parent record, so they never emit
        // their own spans.
        render_field(
            &prefix,
            key,
            val,
            continuation_indent,
            lines,
            record_spans,
            false,
        );
    }
}

fn render_mapping(
    value: &Value,
    indent: usize,
    lines: &mut Vec<String>,
    record_spans: &mut Vec<RecordSpan>,
    emit_spans: bool,
) {
    let Some(obj) = value.as_object() else {
        lines.push(format!("{}{}", " ".repeat(indent), render_scalar(value)));
        return;
    };
    for (key, val) in obj {
        render_field(
            &" ".repeat(indent),
            key,
            val,
            indent + 2,
            lines,
            record_spans,
            emit_spans,
        );
    }
}

fn render_field(
    prefix: &str,
    key: &str,
    val: &Value,
    continuation_indent: usize,
    lines: &mut Vec<String>,
    record_spans: &mut Vec<RecordSpan>,
    emit_spans: bool,
) {
    match val {
        Value::String(s) if needs_block_scalar(s) => {
            lines.push(format!("{prefix}{key}: >-"));
            let indent = " ".repeat(continuation_indent);
            for wrapped in wrap_text(s, WRAP_WIDTH.saturating_sub(continuation_indent)) {
                lines.push(format!("{indent}{wrapped}"));
            }
        }
        Value::String(s) => {
            lines.push(format!("{prefix}{key}: {}", quote_if_needed(s)));
        }
        Value::Object(_) => {
            lines.push(format!("{prefix}{key}:"));
            render_mapping(val, continuation_indent, lines, record_spans, emit_spans);
        }
        Value::Array(arr) if arr.is_empty() => {
            lines.push(format!("{prefix}{key}: []"));
        }
        Value::Array(arr) if arr.iter().all(|v| !v.is_object() && !v.is_array()) => {
            let items: Vec<String> = arr.iter().map(render_scalar).collect();
            let inline = format!("{prefix}{key}: [{}]", items.join(", "));
            if inline.len() <= BLOCK_SCALAR_THRESHOLD {
                lines.push(inline);
            } else {
                lines.push(format!("{prefix}{key}:"));
                let indent = " ".repeat(continuation_indent);
                for item in arr {
                    lines.push(format!("{indent}- {}", render_scalar(item)));
                }
            }
        }
        Value::Array(arr) => {
            let all_objects = arr.iter().all(Value::is_object);
            lines.push(format!("{prefix}{key}:"));
            let indent = " ".repeat(continuation_indent);
            if all_objects && emit_spans {
                render_object_array(arr, &indent, continuation_indent + 4, lines, record_spans);
            } else {
                for item in arr {
                    if let Some(obj) = item.as_object() {
                        render_object_item(
                            obj,
                            &indent,
                            continuation_indent + 4,
                            lines,
                            record_spans,
                        );
                    } else {
                        lines.push(format!("{indent}- {}", render_scalar(item)));
                    }
                }
            }
        }
        _ => {
            lines.push(format!("{prefix}{key}: {}", render_scalar(val)));
        }
    }
}

fn needs_block_scalar(s: &str) -> bool {
    s.len() > BLOCK_SCALAR_THRESHOLD || s.contains('\n')
}

fn render_scalar(val: &Value) -> String {
    match val {
        Value::String(s) => quote_if_needed(s),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => "null".to_string(),
        _ => val.to_string(),
    }
}

/// Quote a string value if it contains YAML-ambiguous characters.
fn quote_if_needed(s: &str) -> String {
    if s.is_empty()
        || s.contains(": ")
        || s.contains(" #")
        || s.starts_with('#')
        || s.starts_with('\'')
        || s.starts_with('"')
        || s.starts_with('{')
        || s.starts_with('[')
        || s.starts_with('*')
        || s.starts_with('&')
        || s.starts_with('!')
        || s.starts_with('|')
        || s.starts_with('>')
        || s.starts_with('%')
        || s.starts_with('@')
        || s.starts_with('`')
        || s.contains('\n')
        || is_yaml_keyword(s)
    {
        format!("\"{}\"", escape_yaml_string(s))
    } else {
        s.to_string()
    }
}

fn is_yaml_keyword(s: &str) -> bool {
    matches!(
        s.to_lowercase().as_str(),
        "null" | "true" | "false" | "yes" | "no" | "on" | "off" | "~"
    )
}

fn escape_yaml_string(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

/// Wrap text at word boundaries to fit within `max_width`.
fn wrap_text(text: &str, max_width: usize) -> Vec<String> {
    let max_width = max_width.max(20);
    let mut result = Vec::new();

    for paragraph in text.split('\n') {
        if paragraph.is_empty() {
            result.push(String::new());
            continue;
        }
        let words: Vec<&str> = paragraph.split_whitespace().collect();
        if words.is_empty() {
            result.push(String::new());
            continue;
        }
        let mut current_line = String::new();
        for word in &words {
            if current_line.is_empty() {
                current_line = word.to_string();
            } else if current_line.len() + 1 + word.len() <= max_width {
                current_line.push(' ');
                current_line.push_str(word);
            } else {
                result.push(current_line);
                current_line = word.to_string();
            }
        }
        if !current_line.is_empty() {
            result.push(current_line);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn render_to_lines_matrix() {
        struct Case {
            label: &'static str,
            value: serde_json::Value,
            check: fn(&RenderedContent),
        }

        let cases: &[Case] = &[
            Case {
                label: "scalar_string",
                value: json!("hello world"),
                check: |rc| {
                    assert_eq!(rc.line_count(), 1);
                    assert_eq!(rc.get_line(0), Some("hello world"));
                },
            },
            Case {
                label: "scalar_number",
                value: json!(42),
                check: |rc| assert_eq!(rc.get_line(0), Some("42")),
            },
            Case {
                label: "null_value",
                value: json!(null),
                check: |rc| assert_eq!(rc.get_line(0), Some("null")),
            },
            Case {
                label: "empty_array",
                value: json!([]),
                check: |rc| assert_eq!(rc.get_line(0), Some("(empty)")),
            },
            Case {
                label: "array_of_scalars",
                value: json!(["a", "b", "c"]),
                check: |rc| {
                    assert_eq!(rc.lines().collect::<Vec<_>>(), vec!["a", "b", "c"]);
                },
            },
            Case {
                label: "simple_object",
                value: json!({"name": "alice", "age": 30}),
                check: |rc| {
                    let lines: Vec<&str> = rc.lines().collect();
                    assert!(lines.contains(&"name: alice"));
                    assert!(lines.contains(&"age: 30"));
                },
            },
            Case {
                label: "array_of_objects",
                value: json!([
                    {"user": "alice", "role": "admin"},
                    {"user": "bob", "role": "viewer"}
                ]),
                check: |rc| {
                    let lines: Vec<&str> = rc.lines().collect();
                    assert!(lines.contains(&"- user: alice"));
                    assert!(lines.contains(&"  role: admin"));
                    assert!(lines.contains(&"- user: bob"));
                },
            },
            Case {
                label: "yaml_keyword_quoted",
                value: json!({"val": "null"}),
                check: |rc| assert!(rc.lines().any(|l| l.contains("\"null\""))),
            },
            Case {
                label: "colon_space_quoted",
                value: json!({"channel": "#general: main"}),
                check: |rc| assert!(rc.lines().any(|l| l.contains("\"#general: main\""))),
            },
            Case {
                label: "hash_prefix_quoted",
                value: json!({"channel": "#general"}),
                check: |rc| assert!(rc.lines().any(|l| l.contains("\"#general\""))),
            },
            Case {
                label: "empty_string_quoted",
                value: json!({"val": ""}),
                check: |rc| assert!(rc.lines().any(|l| l.contains("\"\""))),
            },
        ];

        for case in cases {
            let rc = render_to_lines(&case.value);
            (case.check)(&rc);
            assert!(!case.label.is_empty(), "matrix case label");
        }

        let long_text = "a ".repeat(100);
        let rc = render_to_lines(&json!({"message": long_text.trim()}));
        let lines: Vec<&str> = rc.lines().collect();
        assert_eq!(lines[0], "message: >-", "long_string block opener");
        assert!(lines.len() > 2);
        for line in &lines[1..] {
            assert!(line.len() <= WRAP_WIDTH + 5, "wrapped line too long");
        }

        let rc = render_to_lines(&json!({"note": "line one\nline two\nline three"}));
        let lines: Vec<&str> = rc.lines().collect();
        assert_eq!(lines[0], "note: >-");
        assert!(lines.iter().any(|l| l.trim() == "line one"));
        assert!(lines.iter().any(|l| l.trim() == "line two"));

        let huge = "deployment ".repeat(1000);
        let rc = render_to_lines(&json!({"body": huge.trim()}));
        assert!(rc.line_count() > 50, "10KB body line count");
        assert!(rc.lines().any(|l| l.contains("deployment")));
    }

    #[test]
    fn records_are_tracked_for_root_array() {
        let rc = render_to_lines(&json!([
            {"id": "one", "status": "open"},
            {"id": "two", "status": "closed"}
        ]));
        assert_eq!(
            rc.record_spans(),
            &[
                RecordSpan { start: 0, end: 2 },
                RecordSpan { start: 2, end: 4 }
            ]
        );
    }

    #[test]
    fn one_root_record_collection_allows_scalar_siblings() {
        let rc = render_to_lines(&json!({
            "tasks": [
                {"id": "one", "status": "open"},
                {"id": "two", "status": "closed", "tags": [{"name": "nested"}]}
            ],
            "message": "Found two tasks"
        }));
        assert_eq!(
            rc.record_spans(),
            &[
                RecordSpan { start: 1, end: 3 },
                RecordSpan { start: 3, end: 7 }
            ]
        );
        assert_eq!(rc.get_line(7), Some("message: Found two tasks"));
    }

    #[test]
    fn multiple_root_record_collections_each_get_spans() {
        let rc = render_to_lines(&json!({
            "tasks": [{"id": "one"}],
            "agents": [{"id": "two"}]
        }));
        assert_eq!(
            rc.record_spans(),
            &[
                RecordSpan { start: 1, end: 2 },
                RecordSpan { start: 3, end: 4 }
            ]
        );
    }

    #[test]
    fn nested_record_collection_gets_spans() {
        let rc = render_to_lines(&json!({
            "data": {
                "tasks": [{"id": "one"}, {"id": "two"}]
            }
        }));
        assert_eq!(
            rc.record_spans(),
            &[
                RecordSpan { start: 2, end: 3 },
                RecordSpan { start: 3, end: 4 }
            ]
        );
    }

    #[test]
    fn nested_array_inside_record_does_not_get_own_spans() {
        let rc = render_to_lines(&json!({
            "tasks": [
                {"id": "one", "comments": [{"user": "alice"}]}
            ]
        }));
        // Only the task record gets a span; the nested `comments` array is part of it.
        assert_eq!(rc.record_spans(), &[RecordSpan { start: 1, end: 4 }]);
    }
}
