//! Infer `calls` / `depends_on` edges from a version's text by matching identifiers against
//! display names in the same language. Used for agent-proposed code so edges can't drift from
//! bodies. Comments and string literals are ignored.

use std::collections::{BTreeMap, BTreeSet};

use crate::graph::{short_name, CodeGraph};
use crate::model::{CodeEdgeKind, Kind, Language};

/// Strip `//`/`#` comments and string literals (approximate, good enough for identifiers).
pub fn strip_noise(language: Language, text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    let comment = |i: usize| match language {
        Language::Rust => chars[i] == '/' && chars.get(i + 1) == Some(&'/'),
        Language::Python => chars[i] == '#',
    };
    while i < chars.len() {
        let c = chars[i];
        if comment(i) {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '"' || (language == Language::Python && c == '\'') {
            let q = c;
            i += 1;
            while i < chars.len() && chars[i] != q {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            out.push(' ');
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

/// Identifiers and `A::b` / `A.b` paths in text.
fn tokens(text: &str) -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
    let re_id = regex::Regex::new(r"[A-Za-z_][A-Za-z0-9_]*").unwrap();
    let re_path =
        regex::Regex::new(r"([A-Za-z_][A-Za-z0-9_]*)\s*(?:::|\.)\s*([A-Za-z_][A-Za-z0-9_]*)")
            .unwrap();
    let re_dot = regex::Regex::new(r"\.\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(").unwrap();
    let ids = re_id
        .find_iter(text)
        .filter(|m| !text[..m.start()].trim_end().ends_with('.'))
        .map(|m| m.as_str().to_string())
        .collect();
    let paths = re_path
        .captures_iter(text)
        .map(|c| format!("{}::{}", &c[1], &c[2]))
        .collect();
    let dots = re_dot
        .captures_iter(text)
        .map(|c| c[1].to_string())
        .collect();
    (ids, paths, dots)
}

/// References from `text` (signature + body of symbol `self_id`) to other symbols.
pub fn references(
    g: &CodeGraph,
    self_id: &str,
    language: Language,
    text: &str,
) -> Vec<(String, CodeEdgeKind)> {
    let clean = strip_noise(language, text);
    let (ids, paths, dots) = tokens(&clean);
    let mut method_short: BTreeMap<String, usize> = BTreeMap::new();
    for s in g.symbols.values().filter(|s| s.language == language) {
        if s.display_name.contains("::") || s.display_name.contains('.') {
            *method_short.entry(short_name(&s.display_name)).or_default() += 1;
        }
    }
    let self_class = g.class_of(self_id).map(|c| c.display_name.clone());
    let mut out = Vec::new();
    for s in g.symbols.values() {
        if s.language != language || s.id == self_id || s.kind == Kind::Test {
            continue;
        }
        let is_method = s.display_name.contains("::") || s.display_name.contains('.');
        let hit = if is_method {
            let short = short_name(&s.display_name);
            let class = s.display_name.split(['.', ':']).next().unwrap_or("");
            let qualified = paths.contains(&format!("{class}::{short}"))
                || (Some(class.to_string()) == self_class
                    && (paths.contains(&format!("Self::{short}"))
                        || paths.contains(&format!("self::{short}"))));
            let unique_dot = dots.contains(&short) && method_short.get(&short) == Some(&1);
            qualified || unique_dot
        } else {
            ids.contains(&s.display_name)
        };
        if hit {
            let kind = if s.kind.is_type() {
                CodeEdgeKind::DependsOn
            } else {
                CodeEdgeKind::Calls
            };
            out.push((s.id.clone(), kind));
        }
    }
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::CodeSymbol;
    use crate::model::Visibility;

    fn sym(g: &mut CodeGraph, id: &str, name: &str, kind: Kind) {
        g.symbols.insert(
            id.into(),
            CodeSymbol {
                id: id.into(),
                display_name: name.into(),
                kind,
                language: Language::Rust,
                visibility: Visibility::Public,
                tags: vec![],
                version: "v".into(),
            },
        );
    }

    #[test]
    fn infers_calls_and_types() {
        let mut g = CodeGraph::default();
        sym(&mut g, "vc", "verify_credentials", Kind::Function);
        sym(&mut g, "vp", "verify_password", Kind::Function);
        sym(&mut g, "fu", "UserStore::find_user", Kind::Function);
        sym(&mut g, "us", "UserStore", Kind::Class);
        sym(&mut g, "ar", "AuthResult", Kind::Enum);
        sym(&mut g, "hp", "hash_password", Kind::Function);
        let text = "(store: &UserStore) -> AuthResult\nmatch store.find_user(u) { Some(x) if verify_password(p, &x.h) => 1, _ => AuthResult::InvalidCredentials }\n// hash_password mentioned in a comment\nlet s = \"hash_password\";";
        let r = references(&g, "vc", Language::Rust, text);
        let ids: Vec<&str> = r.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(ids, vec!["ar", "fu", "us", "vp"]);
        assert!(r.contains(&("us".into(), CodeEdgeKind::DependsOn)));
        assert!(r.contains(&("vp".into(), CodeEdgeKind::Calls)));
    }
}

/// Token 3-gram shingles of a body (comments/strings stripped, identifiers kept).
pub fn shingles(language: Language, text: &str) -> BTreeSet<String> {
    let clean = strip_noise(language, text);
    let re = regex::Regex::new(r"[A-Za-z_][A-Za-z0-9_]*|\d+|[^\sA-Za-z0-9_]").unwrap();
    let toks: Vec<&str> = re.find_iter(&clean).map(|m| m.as_str()).collect();
    toks.windows(3).map(|w| w.join(" ")).collect()
}

/// Jaccard similarity of two shingle sets.
pub fn similarity(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

#[cfg(test)]
mod near_tests {
    use super::*;
    #[test]
    fn near_duplicates_score_high() {
        let a = shingles(
            Language::Rust,
            "let x = parse(input);\nif x > 0 { x * 2 } else { 0 }",
        );
        let b = shingles(
            Language::Rust,
            "let x = parse(input);\nif x > 0 { x * 3 } else { 0 } // tweak",
        );
        let c = shingles(Language::Rust, "vec.iter().map(|v| v.len()).sum()");
        assert!(similarity(&a, &b) > 0.6);
        assert!(similarity(&a, &c) < 0.2);
    }
}
