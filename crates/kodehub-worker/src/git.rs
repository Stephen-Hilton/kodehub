//! Git import and export containers (spec §13). Deterministic, no LLM, no graph writes.
//!
//! **Import** clones a repository and parses it into code units — functions, classes (structs,
//! enums, traits), methods and modules, not files — with the references between them. Rust is
//! parsed with `syn` (bodies sliced from the original source by span), Python with the standard
//! `ast` module; other languages become one opaque module unit per file, marked not decomposed.
//! The orchestrator stages the units; the import agent maps them onto the C4 graph.
//!
//! **Export** writes a project's rendered live versions as ordinary files and pushes them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use syn::spanned::Spanned;

fn run(cmd: &mut std::process::Command) -> Result<String> {
    let out = cmd
        .output()
        .with_context(|| format!("cannot run {cmd:?}"))?;
    if !out.status.success() {
        bail!(
            "{:?} failed: {}",
            cmd.get_program(),
            String::from_utf8_lossy(&out.stderr).replace(
                &std::env::var("GIT_TOKEN").unwrap_or_else(|_| "\u{0}".into()),
                "***"
            )
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// `https://host/path` + token → `https://x-access-token:<token>@host/path` (never logged).
fn authed_url(url: &str) -> String {
    match (
        std::env::var("GIT_TOKEN").ok().filter(|t| !t.is_empty()),
        url.strip_prefix("https://"),
    ) {
        (Some(t), Some(rest)) => format!("https://x-access-token:{t}@{rest}"),
        _ => url.to_string(),
    }
}

fn git(dir: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    c.current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_CONFIG_NOSYSTEM", "1");
    c
}

// ---------------------------------------------------------------------- import

pub async fn import(input: &Value) -> Result<Value> {
    let url = input["repo_url"].as_str().context("repo_url is required")?;
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().join("repo");
    let mut c = git(tmp.path());
    c.args(["clone", "--depth", "1", "--quiet"]);
    if let Some(r) = input["ref"].as_str().filter(|r| !r.is_empty()) {
        c.args(["--branch", r]);
    }
    c.arg(authed_url(url)).arg(&dir);
    run(&mut c)?;
    let commit = run(git(&dir).args(["rev-parse", "HEAD"]))?
        .trim()
        .to_string();
    let units = parse_tree(&dir)?;
    Ok(
        json!({"ok": true, "repo_url": url, "commit": commit, "units": units,
              "summary": summarize(&units)}),
    )
}

fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    let mut ents: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
    ents.sort_by_key(|e| e.file_name());
    for e in ents {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.')
            || matches!(
                name.as_str(),
                "target" | "node_modules" | "__pycache__" | "venv" | ".venv" | "dist" | "build"
            )
        {
            continue;
        }
        let p = e.path();
        if e.file_type()?.is_dir() {
            walk(&p, base, out)?;
        } else {
            out.push(p.strip_prefix(base)?.to_path_buf());
        }
    }
    Ok(())
}

/// Parse a checked-out tree into code units.
pub fn parse_tree(root: &Path) -> Result<Vec<Value>> {
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    let mut units = Vec::new();
    let mut py_files = Vec::new();
    for f in &files {
        let ext = f.extension().and_then(|x| x.to_str()).unwrap_or("");
        let rel = f.to_string_lossy().to_string();
        match ext {
            "rs" => {
                let src = std::fs::read_to_string(root.join(f))?;
                match parse_rust(&rel, &src, &rust_package(root, f)) {
                    Ok(mut u) => units.append(&mut u),
                    Err(_) => units.push(opaque(&rel, "rust", &src)),
                }
            }
            "py" => py_files.push(rel),
            "toml" | "lock" | "md" | "txt" | "json" | "yaml" | "yml" | "cfg" | "ini" => {}
            _ => {
                if let Ok(src) = std::fs::read_to_string(root.join(f)) {
                    if src.len() < 512 * 1024 && !src.is_empty() {
                        units.push(opaque(&rel, ext, &src));
                    }
                }
            }
        }
    }
    if !py_files.is_empty() {
        units.extend(parse_python(root, &py_files)?);
    }
    Ok(units)
}

fn opaque(rel: &str, lang: &str, src: &str) -> Value {
    json!({"id": rel.to_string(), "language": lang, "kind": "module", "name": rel,
           "file": rel, "decomposed": false, "body": src, "signature": "", "references": []})
}

/// The Cargo package a Rust file belongs to (nearest Cargo.toml `name`).
fn rust_package(root: &Path, f: &Path) -> String {
    let mut d = root.join(f);
    while d.pop() {
        let ct = d.join("Cargo.toml");
        if let Ok(t) = std::fs::read_to_string(&ct) {
            if let Some(n) = t.lines().find_map(|l| {
                l.trim().strip_prefix("name").map(|x| {
                    x.trim_start_matches([' ', '='])
                        .trim_matches('"')
                        .to_string()
                })
            }) {
                return n;
            }
        }
        if d == root {
            break;
        }
    }
    "crate".into()
}

/// Rust module path of a file (`src/a/b.rs` → `a::b`; `lib.rs`/`main.rs`/`mod.rs` collapse).
fn rust_module(rel: &str) -> String {
    let p = rel
        .split("src/")
        .last()
        .unwrap_or(rel)
        .trim_end_matches(".rs");
    p.split('/')
        .filter(|s| !matches!(*s, "lib" | "main" | "mod"))
        .collect::<Vec<_>>()
        .join("::")
}

fn slice(src: &str, span: proc_macro2::Span) -> String {
    let (s, e) = (span.start(), span.end());
    let lines: Vec<&str> = src.lines().collect();
    if s.line == 0 || e.line == 0 || s.line > lines.len() {
        return String::new();
    }
    let mut out = String::new();
    for (i, l) in lines.iter().enumerate().take(e.line).skip(s.line - 1) {
        let ln = i + 1;
        let from = if ln == s.line {
            s.column.min(l.len())
        } else {
            0
        };
        let to = if ln == e.line {
            e.column.min(l.len())
        } else {
            l.len()
        };
        out.push_str(&l[from..to.max(from)]);
        if ln != e.line {
            out.push('\n');
        }
    }
    out
}

/// Strip one level of braces and dedent: the body format Kodehub versions store.
fn inner_block(block: &str) -> String {
    let t = block.trim();
    let t = t.strip_prefix('{').unwrap_or(t);
    let t = t.strip_suffix('}').unwrap_or(t);
    let lines: Vec<&str> = t.lines().collect();
    let indent = lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| l.len() - l.trim_start().len())
        .min()
        .unwrap_or(0);
    lines
        .iter()
        .map(|l| {
            if l.len() >= indent {
                &l[indent..]
            } else {
                l.trim_start()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn idents(tokens: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for c in tokens.chars() {
        if c.is_alphanumeric() || c == '_' {
            cur.push(c);
        } else if !cur.is_empty() {
            if !cur.chars().next().unwrap().is_ascii_digit() && !out.contains(&cur) {
                out.push(cur.clone());
            }
            cur.clear();
        }
    }
    out
}

pub fn parse_rust(rel: &str, src: &str, package: &str) -> Result<Vec<Value>> {
    let file = syn::parse_file(src)?;
    let module = rust_module(rel);
    let qual = |n: &str| {
        if module.is_empty() {
            n.to_string()
        } else {
            format!("{module}::{n}")
        }
    };
    let mut out = Vec::new();
    let mut push = |kind: &str,
                    name: String,
                    sig: String,
                    body: String,
                    member_of: Option<String>,
                    span_text: &str| {
        out.push(json!({
            "id": format!("{package}::{name}"), "language": "rust", "kind": kind, "name": name,
            "package": package, "module": module, "file": rel, "decomposed": true,
            "signature": sig, "body": body, "member_of": member_of,
            "references": idents(span_text),
        }));
    };
    for item in &file.items {
        match item {
            syn::Item::Fn(f) => {
                let sig = slice(src, f.sig.inputs.span());
                let ret = slice(src, f.sig.output.span());
                let sig = format!("({sig}) {ret}").trim().to_string();
                let body = inner_block(&slice(src, f.block.span()));
                push(
                    "function",
                    qual(&f.sig.ident.to_string()),
                    sig,
                    body.clone(),
                    None,
                    &body,
                );
            }
            syn::Item::Struct(s) => {
                let text = slice(src, s.span());
                let body = text
                    .split_once('{')
                    .map(|x| format!("{{{}", x.1))
                    .unwrap_or_default();
                push(
                    "class",
                    qual(&s.ident.to_string()),
                    String::new(),
                    inner_block(&body),
                    None,
                    &text,
                );
            }
            syn::Item::Enum(e) => {
                let text = slice(src, e.span());
                let body = text
                    .split_once('{')
                    .map(|x| format!("{{{}", x.1))
                    .unwrap_or_default();
                push(
                    "enum",
                    qual(&e.ident.to_string()),
                    String::new(),
                    inner_block(&body),
                    None,
                    &text,
                );
            }
            syn::Item::Trait(t) => {
                let text = slice(src, t.span());
                let body = text
                    .split_once('{')
                    .map(|x| format!("{{{}", x.1))
                    .unwrap_or_default();
                push(
                    "trait",
                    qual(&t.ident.to_string()),
                    String::new(),
                    inner_block(&body),
                    None,
                    &text,
                );
            }
            syn::Item::Impl(im) => {
                let ty = slice(src, im.self_ty.span());
                for it in &im.items {
                    if let syn::ImplItem::Fn(m) = it {
                        let sig = format!(
                            "({}) {}",
                            slice(src, m.sig.inputs.span()),
                            slice(src, m.sig.output.span())
                        )
                        .trim()
                        .to_string();
                        let body = inner_block(&slice(src, m.block.span()));
                        push(
                            "function",
                            qual(&format!("{ty}::{}", m.sig.ident)),
                            sig,
                            body.clone(),
                            Some(format!("{package}::{}", qual(&ty))),
                            &body,
                        );
                    }
                }
            }
            _ => {}
        }
    }
    Ok(out)
}

const PY_PARSER: &str = r#"
import ast, json, sys
root = sys.argv[1]
out = []
for rel in sys.argv[2:]:
    src = open(f"{root}/{rel}", encoding="utf-8", errors="replace").read()
    mod = rel[:-3].replace("/", ".")
    if mod.endswith(".__init__"):
        mod = mod[: -len(".__init__")]
    try:
        tree = ast.parse(src)
    except SyntaxError:
        out.append({"id": rel, "language": "python", "kind": "module", "name": rel, "file": rel,
                    "decomposed": False, "body": src, "signature": "", "references": []})
        continue
    def refs(node):
        return sorted({n.id for n in ast.walk(node) if isinstance(n, ast.Name)} |
                      {n.attr for n in ast.walk(node) if isinstance(n, ast.Attribute)})
    def body_of(fn):
        seg = ast.get_source_segment(src, fn) or ""
        lines = seg.split("\n")[fn.body[0].lineno - fn.lineno:] if fn.body else []
        import textwrap
        return textwrap.dedent("\n".join(lines)).strip()
    for node in tree.body:
        if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)):
            args = ast.get_source_segment(src, node.args) or ""
            ret = (" -> " + ast.get_source_segment(src, node.returns)) if node.returns else ""
            out.append({"id": f"{mod}.{node.name}", "language": "python", "kind": "function",
                        "name": f"{mod}.{node.name}", "module": mod, "file": rel, "decomposed": True,
                        "signature": f"({args}){ret}", "body": body_of(node), "references": refs(node)})
        elif isinstance(node, ast.ClassDef):
            out.append({"id": f"{mod}.{node.name}", "language": "python", "kind": "class",
                        "name": f"{mod}.{node.name}", "module": mod, "file": rel, "decomposed": True,
                        "signature": "", "body": "", "references": refs(node)})
            for m in node.body:
                if isinstance(m, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    args = ast.get_source_segment(src, m.args) or ""
                    out.append({"id": f"{mod}.{node.name}.{m.name}", "language": "python", "kind": "function",
                                "name": f"{mod}.{node.name}.{m.name}", "module": mod, "file": rel,
                                "decomposed": True, "signature": f"({args})", "body": body_of(m),
                                "member_of": f"{mod}.{node.name}", "references": refs(m)})
json.dump(out, sys.stdout)
"#;

fn parse_python(root: &Path, files: &[String]) -> Result<Vec<Value>> {
    let py = std::env::var("KODEHUB_PYTHON").unwrap_or_else(|_| "python3".into());
    let out = run(std::process::Command::new(py)
        .arg("-I")
        .arg("-c")
        .arg(PY_PARSER)
        .arg(root)
        .args(files))?;
    Ok(serde_json::from_str(&out)?)
}

fn summarize(units: &[Value]) -> Value {
    let mut by: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for u in units {
        let group = u["package"]
            .as_str()
            .map(|p| format!("{p}::{}", u["module"].as_str().unwrap_or("")))
            .or_else(|| u["module"].as_str().map(str::to_string))
            .unwrap_or_else(|| u["file"].as_str().unwrap_or("").to_string());
        *by.entry(group)
            .or_default()
            .entry(u["kind"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
    }
    json!({"units": units.len(), "groups": by})
}

// ---------------------------------------------------------------------- export

/// Write the rendered files and push them (Kodehub is the source of truth; Git is a mirror).
pub async fn export(input: &Value) -> Result<Value> {
    let url = input["repo_url"].as_str().context("repo_url is required")?;
    let branch = input["branch"].as_str().unwrap_or("main");
    let files = input["files"].as_object().context("files are required")?;
    let tmp = tempfile::tempdir()?;
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(&dir)?;
    run(git(&dir).args(["init", "--quiet", "-b", branch]))?;
    run(git(&dir).args(["config", "user.name", "Kodehub"]))?;
    run(git(&dir).args(["config", "user.email", "export@kodehub.invalid"]))?;
    // keep history: fetch the branch if the remote has it
    let remote = authed_url(url);
    let have = git(&dir)
        .args(["fetch", "--quiet", "--depth", "1", &remote, branch])
        .output()?
        .status
        .success();
    if have {
        run(git(&dir).args(["reset", "--quiet", "--soft", "FETCH_HEAD"]))?;
    }
    // the mirror is exactly the project: clear tracked files, write the render
    let _ = git(&dir).args(["rm", "-r", "-q", "--cached", "."]).output();
    for (path, content) in files {
        let p = Path::new(path);
        if p.is_absolute()
            || p.components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            bail!("unsafe export path {path}");
        }
        let full = dir.join(p);
        if let Some(d) = full.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&full, content.as_str().unwrap_or(""))?;
    }
    run(git(&dir).args(["add", "-A"]))?;
    let msg = input["message"].as_str().unwrap_or("Kodehub export");
    let changed = !git(&dir)
        .args(["diff", "--cached", "--quiet"])
        .status()?
        .success()
        || !have;
    if changed {
        run(git(&dir).args(["commit", "--quiet", "--allow-empty", "-m", msg]))?;
        run(git(&dir).args([
            "push",
            "--quiet",
            &remote,
            &format!("HEAD:refs/heads/{branch}"),
        ]))?;
    }
    let commit = run(git(&dir).args(["rev-parse", "HEAD"]))?
        .trim()
        .to_string();
    Ok(
        json!({"ok": true, "commit": commit, "pushed": changed, "files": files.len(), "branch": branch}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_units_are_functions_not_files() {
        let src = "use std::fmt;\n\npub struct Counter {\n    n: u32,\n}\n\nimpl Counter {\n    pub fn bump(&mut self) -> u32 {\n        self.n += 1;\n        self.n\n    }\n}\n\npub fn double(x: i32) -> i32 {\n    helper(x) * 2\n}\n\nfn helper(x: i32) -> i32 { x }\n";
        let u = parse_rust("src/lib.rs", src, "demo").unwrap();
        let names: Vec<&str> = u.iter().map(|x| x["name"].as_str().unwrap()).collect();
        assert_eq!(names, vec!["Counter", "Counter::bump", "double", "helper"]);
        let d = u.iter().find(|x| x["name"] == "double").unwrap();
        assert_eq!(d["signature"], "(x: i32) -> i32");
        assert_eq!(d["body"], "helper(x) * 2");
        assert!(d["references"]
            .as_array()
            .unwrap()
            .contains(&json!("helper")));
        let m = u.iter().find(|x| x["name"] == "Counter::bump").unwrap();
        assert_eq!(m["member_of"], "demo::Counter");
        assert_eq!(m["body"], "self.n += 1;\nself.n");
        assert_eq!(rust_module("crates/x/src/a/b.rs"), "a::b");
        assert_eq!(rust_module("src/lib.rs"), "");
    }
}
