/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Formatting helpers for DevTools UI.

use devtools_traits::{DebuggerValue, ObjectPreview, PropertyDescriptor};

/// Format a byte count into a human-readable string.
pub(crate) fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Truncate a string to `max_chars` characters, appending `…` if truncated.
/// UTF-8 safe: uses `char_indices` not byte slicing.
pub(crate) fn truncate_chars(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{truncated}…")
}

/// Format a [`DebuggerValue`] for display in Console / Network / Application.
/// Object previews show inline summaries like `{name: "foo", age: 30}` or `[1, 2, 3]`.
pub(crate) fn format_debugger_value(value: &DebuggerValue) -> String {
    match value {
        DebuggerValue::VoidValue => "undefined".to_string(),
        DebuggerValue::NullValue(_) => "null".to_string(),
        DebuggerValue::BooleanValue(b) => b.to_string(),
        DebuggerValue::NumberValue(n) => {
            if n.fract() == 0.0 && n.is_finite() {
                format!("{}", *n as i64)
            } else {
                n.to_string()
            }
        },
        DebuggerValue::StringValue(s) => format!("\"{}\"", truncate_chars(s, 200)),
        DebuggerValue::ObjectValue { class, preview, .. } => {
            format_object_preview(class, preview.as_deref())
        },
    }
}

/// Format an object preview inline (e.g. `{name: "foo"}`, `[1, 2, 3]`, `ƒ bar()`).
fn format_object_preview(class: &str, preview: Option<&ObjectPreview>) -> String {
    let Some(preview) = preview else {
        return format!("[object {class}]");
    };

    // Function preview.
    if let Some(func) = &preview.function {
        let name = func
            .name
            .as_deref()
            .or(func.display_name.as_deref())
            .unwrap_or("anonymous");
        let params = func.parameter_names.join(", ");
        return format!("ƒ {name}({params})");
    }

    // Array preview.
    if let Some(items) = &preview.items {
        let shown: Vec<String> = items
            .iter()
            .take(10)
            .map(format_debugger_value)
            .collect();
        let suffix = if items.len() > 10 { ", …" } else { "" };
        return format!("[{}{}]", shown.join(", "), suffix);
    }

    // Map entries preview.
    if let Some(entries) = &preview.entries {
        let shown: Vec<String> = entries
            .iter()
            .take(5)
            .map(|(k, v)| {
                format!(
                    "{} => {}",
                    format_debugger_value(k),
                    format_debugger_value(v)
                )
            })
            .collect();
        let suffix = if entries.len() > 5 { ", …" } else { "" };
        return format!("Map({{{}{}}})", shown.join(", "), suffix);
    }

    // Generic object preview.
    if let Some(props) = &preview.own_properties {
        let shown: Vec<String> = props
            .iter()
            .take(5)
            .map(format_property_brief)
            .collect();
        let total = preview
            .own_properties_length
            .map(|n| n as usize)
            .unwrap_or(props.len());
        let ellipsis = if total > shown.len() { ", …" } else { "" };
        return format!("{{{}{}}}", shown.join(", "), ellipsis);
    }

    // Array-like without items but with length.
    if let Some(len) = preview.array_length {
        return format!("[Array({len})]");
    }

    format!("[object {class}]")
}

/// Format a property as `name: value` (value truncated).
fn format_property_brief(prop: &PropertyDescriptor) -> String {
    let val = format_debugger_value(&prop.value);
    format!("{}: {}", prop.name, truncate_chars(&val, 40))
}

/// Case-insensitive substring search (ASCII-only, zero allocation).
pub(crate) fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay = haystack.as_bytes();
    let need = needle.as_bytes();
    if need.len() > hay.len() {
        return false;
    }
    hay.windows(need.len()).any(|w| {
        w.iter()
            .zip(need.iter())
            .all(|(a, b)| a.to_ascii_lowercase() == b.to_ascii_lowercase())
    })
}

/// Current wall-clock time as `HH:MM:SS.mmm`.
pub(crate) fn now_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let ms = now.subsec_millis();
    let secs = now.as_secs() % 86400;
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}.{ms:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_primitives() {
        assert_eq!(format_debugger_value(&DebuggerValue::VoidValue), "undefined");
        assert_eq!(format_debugger_value(&DebuggerValue::NullValue(false)), "null");
        assert_eq!(format_debugger_value(&DebuggerValue::BooleanValue(true)), "true");
        assert_eq!(format_debugger_value(&DebuggerValue::NumberValue(42.0)), "42");
        assert_eq!(format_debugger_value(&DebuggerValue::NumberValue(3.14)), "3.14");
        assert_eq!(
            format_debugger_value(&DebuggerValue::StringValue("hello".into())),
            "\"hello\""
        );
    }

    #[test]
    fn format_object_without_preview() {
        let val = DebuggerValue::ObjectValue {
            actor: None,
            class: "Object".into(),
            own_property_length: None,
            preview: None,
            prototype: None,
        };
        assert_eq!(format_debugger_value(&val), "[object Object]");
    }

    #[test]
    fn format_array_preview() {
        let items = vec![
            DebuggerValue::NumberValue(1.0),
            DebuggerValue::NumberValue(2.0),
            DebuggerValue::NumberValue(3.0),
        ];
        let val = DebuggerValue::ObjectValue {
            actor: None,
            class: "Array".into(),
            own_property_length: None,
            preview: Some(Box::new(ObjectPreview {
                kind: "ArrayLike".into(),
                size: None,
                entries: None,
                own_properties: None,
                own_properties_length: None,
                function: None,
                array_length: Some(3),
                items: Some(items),
            })),
            prototype: None,
        };
        assert_eq!(format_debugger_value(&val), "[1, 2, 3]");
    }

    #[test]
    fn format_object_with_properties() {
        let props = vec![
            PropertyDescriptor {
                name: "name".into(),
                value: DebuggerValue::StringValue("foo".into()),
                configurable: true,
                enumerable: true,
                writable: true,
                is_accessor: false,
            },
            PropertyDescriptor {
                name: "age".into(),
                value: DebuggerValue::NumberValue(30.0),
                configurable: true,
                enumerable: true,
                writable: true,
                is_accessor: false,
            },
        ];
        let val = DebuggerValue::ObjectValue {
            actor: None,
            class: "Object".into(),
            own_property_length: Some(2),
            preview: Some(Box::new(ObjectPreview {
                kind: "Object".into(),
                size: None,
                entries: None,
                own_properties: Some(props),
                own_properties_length: Some(2),
                function: None,
                array_length: None,
                items: None,
            })),
            prototype: None,
        };
        assert_eq!(format_debugger_value(&val), "{name: \"foo\", age: 30}");
    }

    #[test]
    fn format_function_preview() {
        let val = DebuggerValue::ObjectValue {
            actor: None,
            class: "Function".into(),
            own_property_length: None,
            preview: Some(Box::new(ObjectPreview {
                kind: "Function".into(),
                size: None,
                entries: None,
                own_properties: None,
                own_properties_length: None,
                function: Some(devtools_traits::FunctionPreview {
                    name: Some("greet".into()),
                    display_name: None,
                    parameter_names: vec!["a".into(), "b".into()],
                    is_async: None,
                    is_generator: None,
                }),
                array_length: None,
                items: None,
            })),
            prototype: None,
        };
        assert_eq!(format_debugger_value(&val), "ƒ greet(a, b)");
    }

    #[test]
    fn truncate_chars_utf8_safe() {
        assert_eq!(truncate_chars("hello", 10), "hello");
        assert_eq!(truncate_chars("hello world", 5), "hello…");
        let s = "héllo wörld";
        let t = truncate_chars(s, 4);
        assert!(t.ends_with('…'));
    }

    #[test]
    fn truncate_chars_multibyte_boundary() {
        // A string where byte index would split a multi-byte char.
        let s: String = "é".repeat(2000); // 2000 chars, 4000 bytes
        let t = truncate_chars(&s, 1999);
        assert!(t.ends_with('…'));
        assert_eq!(t.chars().count(), 2000); // 1999 + ellipsis
        // Exactly at limit — no truncation, no panic.
        let t2 = truncate_chars(&s, 2000);
        assert_eq!(t2, s);
    }

    #[test]
    fn contains_ignore_ascii_case_works() {
        assert!(contains_ignore_ascii_case("Hello World", "hello"));
        assert!(contains_ignore_ascii_case("Hello World", "WORLD"));
        assert!(contains_ignore_ascii_case("abc", ""));
        assert!(!contains_ignore_ascii_case("abc", "abcd"));
        assert!(contains_ignore_ascii_case("ABC", "abc"));
    }
}
