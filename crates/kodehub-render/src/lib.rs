//! Graph → temporary Rust / Python projects (doc 03). Pure and deterministic: the same closure
//! always renders byte-identical files. Writing to disk is the caller's job (`write_to`).

mod python;
mod rust;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use kodehub_core::graph::GraphError;
use kodehub_core::model::{Kind, Language};
use kodehub_core::sig::ParsedSignature;
use kodehub_core::CodeGraph;
use serde::{Deserialize, Serialize};

/// Bump whenever rendered output changes: it is part of every build-cache key, so stale
/// artifacts from an older renderer are never reused (decisions.md D-036).
pub const GENERATOR: &str = "kodehub-render/4";

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error(transparent)]
    Graph(#[from] GraphError),
    #[error("conflicting dependency `{name}`: {a} vs {b}")]
    DependencyConflict { name: String, a: String, b: String },
    #[error("not runnable: {0}")]
    NotRunnable(String),
    #[error("{0}")]
    Invalid(String),
}

/// A namespace re-export (projection layer).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NamespaceSpec {
    pub name: String,
    /// (symbol id, public name)
    pub exposes: Vec<(String, String)>,
}

/// Request a run-now harness for one function.
#[derive(Debug, Clone)]
pub struct HarnessSpec {
    pub symbol: String,
}

#[derive(Debug, Clone)]
pub struct RenderRequest<'a> {
    pub graph: &'a CodeGraph,
    pub roots: Vec<String>,
    pub language: Language,
    /// Follow `tested_by` edges from roots (test builds of code roots).
    pub include_tests: bool,
    pub namespaces: Vec<NamespaceSpec>,
    pub harness: Option<HarnessSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestMember {
    pub symbol: String,
    pub display_name: String,
    pub kind: Kind,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RenderManifest {
    pub generator: String,
    pub language: Language,
    pub roots: Vec<String>,
    pub closure_hash: String,
    pub include_tests: bool,
    pub harness: Option<String>,
    pub members: Vec<ManifestMember>,
    pub tests: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RenderedProject {
    /// Relative path → contents.
    pub files: BTreeMap<String, String>,
    /// PyO3 extension crate for `binds_to` edges (Python projects only, M8).
    pub ext: Option<Box<RenderedProject>>,
    pub manifest: RenderManifest,
    /// Ordered members (symbol id, version hash).
    pub order: Vec<(String, String)>,
}

impl RenderedProject {
    pub fn write_to(&self, dir: &Path) -> std::io::Result<()> {
        for (rel, content) in &self.files {
            let p = dir.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(p, content)?;
        }
        Ok(())
    }

    /// Display names of the test-kind members, in render order.
    pub fn test_names(&self) -> Vec<String> {
        self.manifest.tests.clone()
    }
}

pub fn indent(text: &str, n: usize) -> String {
    let pad = " ".repeat(n);
    text.lines()
        .map(|l| {
            if l.trim().is_empty() {
                String::new()
            } else {
                format!("{pad}{l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub(crate) struct Prepared<'a> {
    pub graph: &'a CodeGraph,
    pub members: BTreeMap<String, String>,
    pub order: Vec<String>,
    pub closure_hash: String,
}

fn prepare<'a>(req: &RenderRequest<'a>) -> Result<Prepared<'a>, RenderError> {
    let g = req.graph;
    let mut roots = req.roots.clone();
    if let Some(h) = &req.harness {
        if !roots.contains(&h.symbol) {
            roots.push(h.symbol.clone());
        }
    }
    let all = g.closure(&roots, req.include_tests)?;
    // Only members of the requested language are rendered (cross-language binds_to targets are
    // built separately — see M8).
    let members: BTreeMap<String, String> = all
        .into_iter()
        .filter(|(id, _)| g.symbols[id].language == req.language)
        .collect();
    let set: BTreeSet<String> = members.keys().cloned().collect();
    let order = g.topo_order(&set);
    let closure_hash = g.closure_hash_of(&roots, req.include_tests)?;
    Ok(Prepared {
        graph: g,
        members,
        order,
        closure_hash,
    })
}

/// Render a project for `req`.
pub fn render(req: &RenderRequest) -> Result<RenderedProject, RenderError> {
    let p = prepare(req)?;
    let ext = if req.language == Language::Python {
        render_ext_for(&p, req)?
    } else {
        None
    };
    let mut files = match req.language {
        Language::Rust => rust::render(&p, req)?,
        Language::Python => python::render(&p, req)?,
    };
    let members: Vec<ManifestMember> = p
        .order
        .iter()
        .map(|id| {
            let s = &p.graph.symbols[id];
            ManifestMember {
                symbol: id.clone(),
                display_name: s.display_name.clone(),
                kind: s.kind,
                version: p.members[id].clone(),
            }
        })
        .collect();
    let tests: Vec<String> = members
        .iter()
        .filter(|m| m.kind == Kind::Test)
        .map(|m| test_fn_name(req.language, &m.display_name))
        .collect();
    let mut sorted_roots = req.roots.clone();
    sorted_roots.sort();
    let manifest = RenderManifest {
        generator: GENERATOR.into(),
        language: req.language,
        roots: sorted_roots,
        closure_hash: p.closure_hash.clone(),
        include_tests: req.include_tests,
        harness: req.harness.as_ref().map(|h| h.symbol.clone()),
        members,
        tests,
    };
    files.insert(
        "render-manifest.json".into(),
        serde_json::to_string_pretty(&manifest).expect("manifest serialises") + "\n",
    );
    Ok(RenderedProject {
        files,
        ext,
        order: p
            .order
            .iter()
            .map(|id| (id.clone(), p.members[id].clone()))
            .collect(),
        manifest,
    })
}

/// Rust symbols bound (`binds_to`) by Python members of a render, sorted.
pub fn bound_rust_symbols(g: &CodeGraph, members: &BTreeMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = g
        .edges
        .iter()
        .filter(|e| {
            e.kind == kodehub_core::model::CodeEdgeKind::BindsTo
                && members.contains_key(&e.from)
                && g.symbols
                    .get(&e.to)
                    .map(|s| s.language == Language::Rust)
                    .unwrap_or(false)
        })
        .map(|e| e.to.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

fn render_ext_for(
    p: &Prepared,
    req: &RenderRequest,
) -> Result<Option<Box<RenderedProject>>, RenderError> {
    let bound = bound_rust_symbols(p.graph, &p.members);
    if bound.is_empty() {
        return Ok(None);
    }
    // only plain functions with JSON-able parameters can be bound (final review #3)
    for b in &bound {
        runnable(p.graph, b)
            .map_err(|e| RenderError::Invalid(format!("binds_to target `{b}`: {e}")))?;
        let name = &p.graph.symbols[b].display_name;
        if p.members
            .keys()
            .any(|m| p.graph.symbols[m].display_name == *name)
        {
            return Err(RenderError::Invalid(format!(
                "Python symbol `{name}` would shadow the bound Rust function of the same name"
            )));
        }
    }
    let rreq = RenderRequest {
        graph: req.graph,
        roots: bound.clone(),
        language: Language::Rust,
        include_tests: false,
        namespaces: vec![],
        harness: None,
    };
    let rp = prepare(&rreq)?;
    let files = rust::render_ext(&rp, &rreq, &bound)?;
    let mut sorted_roots = bound.clone();
    sorted_roots.sort();
    Ok(Some(Box::new(RenderedProject {
        manifest: RenderManifest {
            generator: GENERATOR.into(),
            language: Language::Rust,
            roots: sorted_roots,
            closure_hash: rp.closure_hash.clone(),
            include_tests: false,
            harness: None,
            members: vec![],
            tests: vec![],
        },
        order: vec![],
        files,
        ext: None,
    })))
}

/// Test function name in generated code (Python tests must start with `test_`).
pub fn test_fn_name(language: Language, display_name: &str) -> String {
    let n = kodehub_core::graph::short_name(display_name);
    match language {
        Language::Python if !n.starts_with("test_") => format!("test_{n}"),
        _ => n,
    }
}

/// Check whether a function can be run with a generated form/harness; explains why not.
pub fn runnable(g: &CodeGraph, symbol: &str) -> Result<ParsedSignature, RenderError> {
    use kodehub_core::sig::{parse_signature, TypeSpec};
    let s = g.symbol(symbol)?;
    if s.kind != Kind::Function {
        return Err(RenderError::NotRunnable(format!(
            "`{}` is a {}, only functions can be run",
            s.display_name,
            s.kind.as_str()
        )));
    }
    if g.class_of(symbol).is_some() {
        let sig = parse_signature(s.language, &g.version(&s.version)?.signature);
        if sig.has_self {
            return Err(RenderError::NotRunnable(format!(
                "`{}` is a method that needs an instance; run a function that calls it",
                s.display_name
            )));
        }
        return Err(RenderError::NotRunnable(format!(
            "`{}` is an associated function; run-now supports free functions in the pilot",
            s.display_name
        )));
    }
    let v = g.version(&s.version)?;
    let sig = parse_signature(s.language, &v.signature);
    if sig.generics {
        return Err(RenderError::NotRunnable(
            "generic functions are not supported".into(),
        ));
    }
    fn check(g: &CodeGraph, lang: Language, t: &TypeSpec, depth: usize) -> Result<(), String> {
        if depth > 6 {
            return Err("type nesting too deep".into());
        }
        match t {
            TypeSpec::Int | TypeSpec::Float | TypeSpec::Bool | TypeSpec::Str => Ok(()),
            TypeSpec::Option { inner } | TypeSpec::List { inner } => {
                check(g, lang, inner, depth + 1)
            }
            TypeSpec::Map { value } => check(g, lang, value, depth + 1),
            TypeSpec::Named { name } => {
                let Some(sym) = g.find_by_name(lang, name) else {
                    return Err(format!("unknown type `{name}`"));
                };
                let ver = g.version(&sym.version).map_err(|e| e.to_string())?;
                match sym.kind {
                    Kind::Class => {
                        for f in kodehub_core::sig::parse_fields(lang, &ver.body) {
                            check(g, lang, &f.ty, depth + 1)
                                .map_err(|e| format!("field `{name}.{}`: {e}", f.name))?;
                        }
                        Ok(())
                    }
                    Kind::Enum => Ok(()),
                    _ => Err(format!("`{name}` is a {}", sym.kind.as_str())),
                }
            }
            TypeSpec::Unsupported { raw } => Err(format!("unsupported type `{raw}`")),
        }
    }
    for p in &sig.params {
        check(g, s.language, &p.ty, 0)
            .map_err(|e| RenderError::NotRunnable(format!("parameter `{}`: {e}", p.name)))?;
    }
    Ok(sig)
}

#[cfg(test)]
mod tests;

/// JSON form spec for a run-now parameter form: primitive types plus nested structs / enums
/// resolved against the graph.
pub fn form_spec(
    g: &CodeGraph,
    lang: Language,
    t: &kodehub_core::sig::TypeSpec,
    depth: usize,
) -> serde_json::Value {
    use kodehub_core::sig::TypeSpec;
    use serde_json::json;
    if depth > 6 {
        return json!({"type": "json"});
    }
    match t {
        TypeSpec::Int => json!({"type": "int"}),
        TypeSpec::Float => json!({"type": "float"}),
        TypeSpec::Bool => json!({"type": "bool"}),
        TypeSpec::Str => json!({"type": "string"}),
        TypeSpec::Option { inner } => {
            json!({"type": "option", "inner": form_spec(g, lang, inner, depth + 1)})
        }
        TypeSpec::List { inner } => {
            json!({"type": "list", "inner": form_spec(g, lang, inner, depth + 1)})
        }
        TypeSpec::Map { value } => {
            json!({"type": "map", "value": form_spec(g, lang, value, depth + 1)})
        }
        TypeSpec::Named { name } => {
            let Some(sym) = g.find_by_name(lang, name) else {
                return json!({"type": "unsupported", "reason": format!("unknown type {name}")});
            };
            let Ok(v) = g.version(&sym.version) else {
                return json!({"type": "unsupported", "reason": "missing version"});
            };
            match sym.kind {
                Kind::Class => {
                    let fields: Vec<serde_json::Value> = kodehub_core::sig::parse_fields(lang, &v.body)
                        .iter()
                        .map(|f| json!({"name": f.name, "raw": f.raw, "spec": form_spec(g, lang, &f.ty, depth + 1)}))
                        .collect();
                    json!({"type": "struct", "name": name, "fields": fields})
                }
                Kind::Enum => {
                    let snake = v
                        .attributes
                        .get("item_attrs")
                        .map(|a| a.to_string().contains("snake_case"))
                        .unwrap_or(false);
                    let variants: Vec<String> = kodehub_core::sig::parse_enum_variants(&v.body)
                        .into_iter()
                        .map(|n| if snake { to_snake(&n) } else { n })
                        .collect();
                    json!({"type": "enum", "name": name, "variants": variants})
                }
                _ => {
                    json!({"type": "unsupported", "reason": format!("{name} is a {}", sym.kind.as_str())})
                }
            }
        }
        TypeSpec::Unsupported { raw } => {
            json!({"type": "unsupported", "reason": format!("unsupported type {raw}")})
        }
    }
}

fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}
