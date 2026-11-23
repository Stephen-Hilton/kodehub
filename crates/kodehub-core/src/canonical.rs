//! Canonical JSON and BLAKE3 hashing.
//!
//! Canonical form: object keys sorted by byte order, no insignificant whitespace, strings escaped
//! by `serde_json`'s rules. We serialise by hand rather than trusting `serde_json::Value` ordering,
//! because a dependency enabling `preserve_order` would silently change key order.

use serde_json::Value;

use crate::model::VersionContent;

/// Serialise a JSON value canonically (sorted keys, compact).
pub fn canonical_json(v: &Value) -> String {
    let mut out = String::new();
    write_canonical(v, &mut out);
    out
}

fn write_canonical(v: &Value, out: &mut String) {
    match v {
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
            out.push_str(&serde_json::to_string(v).expect("scalar serialises"));
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("key serialises"));
                out.push(':');
                write_canonical(&map[k.as_str()], out);
            }
            out.push('}');
        }
    }
}

/// Normalise source text: line endings to `\n`, trailing whitespace stripped from every line,
/// trailing blank lines removed.
pub fn normalize_text(s: &str) -> String {
    let unified = s.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<&str> = unified.split('\n').map(|l| l.trim_end()).collect();
    let mut joined = lines.join("\n");
    while joined.ends_with('\n') {
        joined.pop();
    }
    joined
}

/// The canonical JSON document that is hashed for a version.
pub fn canonical_content(c: &VersionContent) -> String {
    let v = serde_json::json!({
        "kind": c.kind.as_str(),
        "language": c.language.as_str(),
        "signature": normalize_text(&c.signature).trim().to_string(),
        "body": normalize_text(&c.body),
        "attributes": c.attributes.clone(),
    });
    canonical_json(&v)
}

/// BLAKE3 hex digest of a version's canonical content.
pub fn content_hash(c: &VersionContent) -> String {
    blake3::hash(canonical_content(c).as_bytes())
        .to_hex()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keys_are_sorted_recursively() {
        let v = json!({"b": 1, "a": {"z": [3, {"y": 1, "x": 2}], "c": null}});
        assert_eq!(
            canonical_json(&v),
            r#"{"a":{"c":null,"z":[3,{"x":2,"y":1}]},"b":1}"#
        );
    }

    #[test]
    fn normalisation_strips_crlf_and_trailing_ws() {
        assert_eq!(normalize_text("a  \r\nb\t\r\n\r\n"), "a\nb");
        assert_eq!(normalize_text("  x\n"), "  x");
    }
}
