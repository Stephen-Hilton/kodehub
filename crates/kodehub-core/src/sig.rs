//! Signature parsing for run-now parameter forms and harness generation.
//!
//! Signatures are stored name-free, e.g. Rust `(store: &UserStore, username: &str) -> AuthResult`
//! or Python `(username: str, password: str) -> dict`.

use serde::{Deserialize, Serialize};

use crate::model::Language;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TypeSpec {
    Int,
    Float,
    Bool,
    Str,
    Option {
        inner: Box<TypeSpec>,
    },
    List {
        inner: Box<TypeSpec>,
    },
    Map {
        value: Box<TypeSpec>,
    },
    /// A user-defined struct/class/enum; resolved against the graph by the caller.
    Named {
        name: String,
    },
    Unsupported {
        raw: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub ty: TypeSpec,
    /// Original type text (e.g. `&str`).
    pub raw: String,
    /// Rust: passed by `&` / `&mut` reference.
    #[serde(default)]
    pub by_ref: bool,
    #[serde(default)]
    pub mutable: bool,
    #[serde(default)]
    pub default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParsedSignature {
    pub params: Vec<Param>,
    pub returns: Option<String>,
    /// Takes `self` (Rust) or `self` first param (Python) — not runnable standalone.
    pub has_self: bool,
    pub generics: bool,
    pub is_async: bool,
}

/// Field of a struct/class body, used to build nested forms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: TypeSpec,
    pub raw: String,
}

/// Split on top-level commas, respecting (), [], <>, {}.
pub fn split_top(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    let mut prev = ' ';
    for ch in s.chars() {
        match ch {
            '(' | '[' | '<' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            '>' if prev != '-' => depth -= 1,
            _ => {}
        }
        if ch == sep && depth == 0 {
            out.push(cur.trim().to_string());
            cur.clear();
        } else {
            cur.push(ch);
        }
        prev = ch;
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// Find the index of the `)` matching the `(` at `open`.
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0;
    for (i, ch) in s.char_indices().skip_while(|(i, _)| *i < open) {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

pub fn parse_signature(language: Language, sig: &str) -> ParsedSignature {
    let sig = sig.trim();
    let generics = sig.starts_with('<');
    let open = sig.find('(').unwrap_or(0);
    let close = matching_paren(sig, open).unwrap_or(sig.len().saturating_sub(1));
    let inner = if sig.len() > open + 1 && close > open {
        &sig[open + 1..close]
    } else {
        ""
    };
    let after = sig.get(close + 1..).unwrap_or("").trim();
    let returns = after
        .strip_prefix("->")
        .map(|r| r.trim().trim_end_matches(':').trim().to_string())
        .filter(|r| !r.is_empty());
    let mut params = Vec::new();
    let mut has_self = false;
    for p in split_top(inner, ',') {
        if p.is_empty() {
            continue;
        }
        let p_noann = p.split(':').next().unwrap_or("").trim();
        if matches!(p_noann, "self" | "&self" | "&mut self" | "mut self" | "cls")
            || p.trim() == "self"
        {
            has_self = true;
            continue;
        }
        if p.starts_with('*') {
            params.push(Param {
                name: p.trim_start_matches('*').to_string(),
                ty: TypeSpec::Unsupported { raw: p.clone() },
                raw: p.clone(),
                by_ref: false,
                mutable: false,
                default: None,
            });
            continue;
        }
        let (name_part, rest) = match p.split_once(':') {
            Some((n, r)) => (n.trim(), r.trim()),
            None => (p.split('=').next().unwrap_or("").trim(), ""),
        };
        let (ty_raw, default) = match language {
            Language::Python => match split_top(rest, '=').as_slice() {
                [t, d, ..] => (t.trim().to_string(), Some(d.trim().to_string())),
                [t] => (t.trim().to_string(), None),
                [] => (String::new(), None),
            },
            Language::Rust => (rest.to_string(), None),
        };
        let name = name_part.trim_start_matches("mut ").trim().to_string();
        let (ty, by_ref, mutable) = match language {
            Language::Rust => parse_rust_type_ref(&ty_raw),
            Language::Python => {
                if ty_raw.is_empty() {
                    (
                        TypeSpec::Unsupported {
                            raw: "unannotated".into(),
                        },
                        false,
                        false,
                    )
                } else {
                    (parse_python_type(&ty_raw), false, false)
                }
            }
        };
        params.push(Param {
            name,
            ty,
            raw: ty_raw,
            by_ref,
            mutable,
            default,
        });
    }
    ParsedSignature {
        params,
        returns,
        has_self,
        generics,
        is_async: false,
    }
}

fn parse_rust_type_ref(raw: &str) -> (TypeSpec, bool, bool) {
    let t = raw.trim();
    if let Some(rest) = t.strip_prefix('&') {
        let rest = rest.trim_start();
        let rest = if rest.starts_with('\'') {
            rest.split_once(' ').map(|x| x.1).unwrap_or("")
        } else {
            rest
        };
        if let Some(r) = rest.strip_prefix("mut ") {
            return (parse_rust_type(r), true, true);
        }
        return (parse_rust_type(rest), true, false);
    }
    (parse_rust_type(t), false, false)
}

fn generic_args(t: &str, head: &str) -> Option<Vec<String>> {
    let rest = t.strip_prefix(head)?.trim_start();
    let inner = rest.strip_prefix('<')?.strip_suffix('>')?;
    Some(split_top(inner, ','))
}

pub fn parse_rust_type(raw: &str) -> TypeSpec {
    let t = raw.trim();
    let t = t
        .trim_start_matches("std::collections::")
        .trim_start_matches("std::string::");
    match t {
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => return TypeSpec::Int,
        "f32" | "f64" => return TypeSpec::Float,
        "bool" => return TypeSpec::Bool,
        "String" | "str" | "char" => return TypeSpec::Str,
        _ => {}
    }
    if let Some(a) = generic_args(t, "Option") {
        if a.len() == 1 {
            return TypeSpec::Option {
                inner: Box::new(parse_rust_type(&a[0])),
            };
        }
    }
    if let Some(a) = generic_args(t, "Vec") {
        if a.len() == 1 {
            return TypeSpec::List {
                inner: Box::new(parse_rust_type(&a[0])),
            };
        }
    }
    if let Some(inner) = t.strip_prefix('[').and_then(|x| x.strip_suffix(']')) {
        if !inner.contains(';') {
            return TypeSpec::List {
                inner: Box::new(parse_rust_type(inner)),
            };
        }
    }
    for head in ["HashMap", "BTreeMap"] {
        if let Some(a) = generic_args(t, head) {
            if a.len() == 2 && parse_rust_type(&a[0]) == TypeSpec::Str {
                return TypeSpec::Map {
                    value: Box::new(parse_rust_type(&a[1])),
                };
            }
        }
    }
    if is_ident(t) && t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return TypeSpec::Named { name: t.into() };
    }
    TypeSpec::Unsupported { raw: raw.into() }
}

pub fn parse_python_type(raw: &str) -> TypeSpec {
    let t = raw.trim().trim_matches('"').trim_matches('\'');
    let union: Vec<String> = split_top(t, '|');
    if union.len() == 2 && union.iter().any(|u| u == "None") {
        let other = union.iter().find(|u| *u != "None").unwrap();
        return TypeSpec::Option {
            inner: Box::new(parse_python_type(other)),
        };
    }
    match t {
        "int" => return TypeSpec::Int,
        "float" => return TypeSpec::Float,
        "bool" => return TypeSpec::Bool,
        "str" => return TypeSpec::Str,
        _ => {}
    }
    let sub = |head: &str| -> Option<Vec<String>> {
        let rest = t.strip_prefix(head)?;
        let inner = rest.strip_prefix('[')?.strip_suffix(']')?;
        Some(split_top(inner, ','))
    };
    for head in ["Optional", "typing.Optional"] {
        if let Some(a) = sub(head) {
            if a.len() == 1 {
                return TypeSpec::Option {
                    inner: Box::new(parse_python_type(&a[0])),
                };
            }
        }
    }
    for head in ["list", "List", "typing.List"] {
        if let Some(a) = sub(head) {
            if a.len() == 1 {
                return TypeSpec::List {
                    inner: Box::new(parse_python_type(&a[0])),
                };
            }
        }
    }
    for head in ["dict", "Dict", "typing.Dict"] {
        if let Some(a) = sub(head) {
            if a.len() == 2 && parse_python_type(&a[0]) == TypeSpec::Str {
                return TypeSpec::Map {
                    value: Box::new(parse_python_type(&a[1])),
                };
            }
        }
    }
    if is_ident(t) && t.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        return TypeSpec::Named { name: t.into() };
    }
    TypeSpec::Unsupported { raw: raw.into() }
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
        && !s.chars().next().unwrap().is_ascii_digit()
}

/// Parse fields from a struct body (Rust `pub name: Type,` lines) or a Python class body
/// (`name: type` annotations at class level).
pub fn parse_fields(language: Language, body: &str) -> Vec<Field> {
    let mut out = Vec::new();
    match language {
        Language::Rust => {
            let cleaned: String = body
                .lines()
                .filter(|l| {
                    let t = l.trim();
                    !t.starts_with("//") && !t.starts_with("#[")
                })
                .collect::<Vec<_>>()
                .join("\n");
            for f in split_top(&cleaned, ',') {
                let f = f
                    .trim()
                    .trim_start_matches("pub(crate) ")
                    .trim_start_matches("pub ");
                if let Some((n, t)) = f.split_once(':') {
                    out.push(Field {
                        name: n.trim().to_string(),
                        ty: parse_rust_type(t),
                        raw: t.trim().to_string(),
                    });
                }
            }
        }
        Language::Python => {
            for line in body.lines() {
                if line.starts_with(' ') || line.starts_with('\t') {
                    continue; // only class-level annotations
                }
                let l = line.trim();
                if l.starts_with("def ") || l.starts_with('@') || l.starts_with('#') {
                    continue;
                }
                if let Some((n, rest)) = l.split_once(':') {
                    let n = n.trim();
                    if !is_ident(n) {
                        continue;
                    }
                    let t = rest.split('=').next().unwrap_or("").trim();
                    out.push(Field {
                        name: n.to_string(),
                        ty: parse_python_type(t),
                        raw: t.to_string(),
                    });
                }
            }
        }
    }
    out
}

/// Parse unit/struct variant names from a Rust enum body.
pub fn parse_enum_variants(body: &str) -> Vec<String> {
    split_top(body, ',')
        .into_iter()
        .map(|v| {
            v.lines()
                .filter(|l| !l.trim().starts_with("#[") && !l.trim().starts_with("//"))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter_map(|v| {
            let name: String = v
                .trim()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_signature() {
        let s = parse_signature(
            Language::Rust,
            "(store: &UserStore, username: &str, ids: Vec<i64>, m: HashMap<String, Option<f64>>) -> AuthResult",
        );
        assert_eq!(s.params.len(), 4);
        assert_eq!(
            s.params[0].ty,
            TypeSpec::Named {
                name: "UserStore".into()
            }
        );
        assert!(s.params[0].by_ref);
        assert_eq!(s.params[1].ty, TypeSpec::Str);
        assert_eq!(
            s.params[2].ty,
            TypeSpec::List {
                inner: Box::new(TypeSpec::Int)
            }
        );
        assert_eq!(
            s.params[3].ty,
            TypeSpec::Map {
                value: Box::new(TypeSpec::Option {
                    inner: Box::new(TypeSpec::Float)
                })
            }
        );
        assert_eq!(s.returns.as_deref(), Some("AuthResult"));
        assert!(!s.has_self);
    }

    #[test]
    fn rust_method_and_unsupported() {
        let s = parse_signature(Language::Rust, "(&self, f: fn(i32) -> i32) -> Option<User>");
        assert!(s.has_self);
        assert!(matches!(s.params[0].ty, TypeSpec::Unsupported { .. }));
    }

    #[test]
    fn python_signature() {
        let s = parse_signature(
            Language::Python,
            "(username: str, password: str, tries: int = 3, tags: list[str] | None = None) -> dict",
        );
        assert_eq!(s.params.len(), 4);
        assert_eq!(s.params[2].default.as_deref(), Some("3"));
        assert_eq!(
            s.params[3].ty,
            TypeSpec::Option {
                inner: Box::new(TypeSpec::List {
                    inner: Box::new(TypeSpec::Str)
                })
            }
        );
        let u = parse_signature(Language::Python, "(x)");
        assert!(matches!(u.params[0].ty, TypeSpec::Unsupported { .. }));
    }

    #[test]
    fn fields() {
        let f = parse_fields(
            Language::Rust,
            "pub username: String,\npub password_hash: String,\npub locked: bool,",
        );
        assert_eq!(f.len(), 3);
        assert_eq!(f[2].ty, TypeSpec::Bool);
        let p = parse_fields(
            Language::Python,
            "username: str\nlocked: bool = False\n\ndef x(self):\n    y: int = 1",
        );
        assert_eq!(p.len(), 2);
        assert_eq!(
            parse_enum_variants("Success { username: String },\nInvalidCredentials,"),
            vec!["Success", "InvalidCredentials"]
        );
    }
}
