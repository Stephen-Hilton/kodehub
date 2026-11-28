#![allow(clippy::too_many_arguments)]
use super::*;
use kodehub_core::model::{CodeEdgeKind, Language, VersionContent, Visibility};
use kodehub_core::{CodeEdge, CodeSymbol};
use serde_json::json;

fn add(
    g: &mut CodeGraph,
    id: &str,
    name: &str,
    kind: Kind,
    lang: Language,
    sig: &str,
    body: &str,
    attrs: serde_json::Value,
) {
    let v = VersionContent {
        kind,
        language: lang,
        signature: sig.into(),
        body: body.into(),
        attributes: attrs,
    };
    let h = v.hash();
    g.versions.insert(h.clone(), v);
    g.symbols.insert(
        id.into(),
        CodeSymbol {
            id: id.into(),
            display_name: name.into(),
            kind,
            language: lang,
            visibility: Visibility::Public,
            tags: vec![],
            version: h,
        },
    );
}

fn edge(g: &mut CodeGraph, f: &str, t: &str, k: CodeEdgeKind) {
    g.edges.push(CodeEdge {
        from: f.into(),
        to: t.into(),
        kind: k,
        pinned_version: None,
    });
    g.edges.sort();
}

fn sample() -> CodeGraph {
    let mut g = CodeGraph::default();
    add(
        &mut g,
        "Counter",
        "Counter",
        Kind::Class,
        Language::Rust,
        "",
        "pub n: i64,",
        json!({"derives": ["Debug", "Default"]}),
    );
    add(
        &mut g,
        "Counter-bump",
        "Counter::bump",
        Kind::Function,
        Language::Rust,
        "(&mut self) -> i64",
        "self.n += 1;\nself.n",
        json!({}),
    );
    add(
        &mut g,
        "double",
        "double",
        Kind::Function,
        Language::Rust,
        "(x: i64) -> i64",
        "x * 2",
        json!({}),
    );
    add(
        &mut g,
        "quad",
        "quad",
        Kind::Function,
        Language::Rust,
        "(x: i64, label: &str) -> String",
        "format!(\"{label}:{}\", double(double(x)))",
        json!({"uses": ["std::fmt::Write"]}),
    );
    add(
        &mut g,
        "test_quad",
        "test_quad",
        Kind::Test,
        Language::Rust,
        "",
        "assert_eq!(quad(1, \"a\"), \"a:4\");",
        json!({}),
    );
    add(
        &mut g,
        "py-greet",
        "greet",
        Kind::Function,
        Language::Python,
        "(name: str) -> str",
        "return f\"hi {name}\"",
        json!({"imports": ["import json"]}),
    );
    add(
        &mut g,
        "py-test_greet",
        "greet_works",
        Kind::Test,
        Language::Python,
        "",
        "assert greet(\"a\") == \"hi a\"",
        json!({}),
    );
    edge(&mut g, "Counter-bump", "Counter", CodeEdgeKind::MemberOf);
    edge(&mut g, "quad", "double", CodeEdgeKind::Calls);
    edge(&mut g, "test_quad", "quad", CodeEdgeKind::Calls);
    edge(&mut g, "quad", "test_quad", CodeEdgeKind::TestedWith);
    edge(&mut g, "py-test_greet", "py-greet", CodeEdgeKind::Calls);
    g
}

fn req<'a>(g: &'a CodeGraph, roots: &[&str], lang: Language) -> RenderRequest<'a> {
    RenderRequest {
        graph: g,
        roots: roots.iter().map(|s| s.to_string()).collect(),
        language: lang,
        include_tests: false,
        namespaces: vec![],
        harness: None,
    }
}

#[test]
fn rendering_is_byte_identical() {
    let g = sample();
    let a = render(&req(
        &g,
        &["quad", "Counter-bump", "test_quad"],
        Language::Rust,
    ))
    .unwrap();
    let b = render(&req(
        &g,
        &["test_quad", "Counter-bump", "quad"],
        Language::Rust,
    ))
    .unwrap();
    assert_eq!(a.files, b.files);
    let g2 = g.clone(); // independent clone renders the same
    let c = render(&req(
        &g2,
        &["quad", "Counter-bump", "test_quad"],
        Language::Rust,
    ))
    .unwrap();
    assert_eq!(a.files, c.files);
}

#[test]
fn rust_layout() {
    let g = sample();
    let mut r = req(&g, &["test_quad", "Counter-bump"], Language::Rust);
    r.namespaces = vec![NamespaceSpec {
        name: "sdk".into(),
        exposes: vec![("quad".into(), "quadruple".into())],
    }];
    r.harness = Some(HarnessSpec {
        symbol: "quad".into(),
    });
    let p = render(&r).unwrap();
    let lib = &p.files["src/lib.rs"];
    assert!(
        lib.find("pub fn double").unwrap() < lib.find("pub fn quad").unwrap(),
        "{lib}"
    );
    assert!(lib.contains("impl Counter {\n    // kodehub: Counter-bump"));
    assert!(lib.contains("    pub fn bump(&mut self) -> i64 {\n        self.n += 1;"));
    assert!(lib.contains("#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]"));
    assert!(lib.contains("mod kodehub_tests"));
    assert!(lib.contains("pub use super::quad as quadruple;"));
    assert!(lib.contains("use std::fmt::Write;"));
    let h = &p.files["src/bin/kodehub_run.rs"];
    assert!(h.contains("let a1: String = arg(&args, \"label\");"));
    assert!(h.contains("quad(a0, &a1)"));
    assert!(p.files["Cargo.toml"].contains("[workspace]"));
    assert_eq!(p.manifest.tests, vec!["test_quad"]);
    assert!(p.files.contains_key("render-manifest.json"));
}

#[test]
fn python_layout() {
    let g = sample();
    let mut r = req(&g, &["py-test_greet"], Language::Python);
    r.harness = Some(HarnessSpec {
        symbol: "py-greet".into(),
    });
    let p = render(&r).unwrap();
    let init = &p.files["kodehub_gen/__init__.py"];
    assert!(init.contains("def greet(name: str) -> str:\n    return f\"hi {name}\""));
    assert!(init.contains("import json"));
    let t = &p.files["tests/test_kodehub.py"];
    assert!(t.contains("def test_greet_works():"));
    assert!(p.files["kodehub_run.py"].contains("from kodehub_gen import greet"));
}

#[test]
fn runnable_checks() {
    let g = sample();
    assert!(runnable(&g, "quad").is_ok());
    assert!(matches!(
        runnable(&g, "Counter-bump"),
        Err(RenderError::NotRunnable(_))
    ));
    assert!(matches!(
        runnable(&g, "Counter"),
        Err(RenderError::NotRunnable(_))
    ));
}

#[test]
fn crate_conflicts_are_reported() {
    let mut g = sample();
    add(
        &mut g,
        "a",
        "a",
        Kind::Function,
        Language::Rust,
        "()",
        "",
        json!({"crates": {"rand": "0.8"}}),
    );
    add(
        &mut g,
        "b",
        "b",
        Kind::Function,
        Language::Rust,
        "()",
        "",
        json!({"crates": {"rand": "0.9"}}),
    );
    assert!(matches!(
        render(&req(&g, &["a", "b"], Language::Rust)),
        Err(RenderError::DependencyConflict { .. })
    ));
}

/// Guard for the build cache: rendered output is part of every artifact, so any change to it must
/// bump GENERATOR (which is in the cache key). Regenerate with UPDATE_GOLDEN=1 after bumping.
#[test]
fn renderer_changes_bump_the_generator_version() {
    // binds_to (PyO3 extension + shim) and a namespace, so every kind of rendered output is
    // covered (final review #14)
    let mut g = sample();
    edge(&mut g, "py-greet", "quad", CodeEdgeKind::BindsTo);
    let mut r = req(
        &g,
        &["test_quad", "Counter-bump", "py-test_greet"],
        Language::Rust,
    );
    r.harness = Some(HarnessSpec {
        symbol: "quad".into(),
    });
    r.namespaces = vec![NamespaceSpec {
        name: "sdk".into(),
        exposes: vec![
            ("quad".into(), "quadruple".into()),
            ("py-greet".into(), "hello".into()),
        ],
    }];
    let rust = render(&r).unwrap();
    let py = render(&RenderRequest {
        language: Language::Python,
        roots: vec!["py-test_greet".into()],
        harness: Some(HarnessSpec {
            symbol: "py-greet".into(),
        }),
        ..r.clone()
    })
    .unwrap();
    let ext = py.ext.as_ref().expect("binds_to renders an extension");
    assert!(ext.files["src/lib.rs"].contains("#[pyo3::pymodule]"));
    assert!(py.files["kodehub_gen/__init__.py"].contains("from . import kodehub_ext"));
    let mut h = blake3::Hasher::new();
    for f in [&rust.files, &py.files, &ext.files] {
        for (k, v) in f {
            h.update(k.as_bytes());
            h.update(v.as_bytes());
        }
    }
    let line = format!("{} {}\n", GENERATOR, h.finalize().to_hex());
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden_render.txt");
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::write(path, &line).unwrap();
    }
    let golden = std::fs::read_to_string(path).unwrap_or_default();
    let (g_ver, g_hash) = golden.trim().split_once(' ').unwrap_or(("", ""));
    let (ver, hash) = line.trim().split_once(' ').unwrap();
    if hash != g_hash {
        assert_ne!(ver, g_ver, "rendered output changed but GENERATOR is still {ver}: bump it, then run with UPDATE_GOLDEN=1");
        panic!("rendered output changed (GENERATOR bumped to {ver}): update the golden with UPDATE_GOLDEN=1");
    }
}
