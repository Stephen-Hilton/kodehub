//! Test bundles (spec §5.3–5.4): build once, test many times.
//!
//! A build container renders a batch of same-language tests and compiles it **once** into a
//! bundle: for Rust the compiled test binary, for Python the rendered package with its
//! dependencies vendored. Every test gets a plain shell script, `tests/<fn>.sh`, which is the
//! runner contract: exit status 0 = pass. Test containers download the bundle by its key and
//! run the scripts; they never compile and never call an LLM.
//!
//! Tests whose version has `attributes.shell = true` are authored shell scripts: their body is
//! the script, run from the bundle directory (with the language artifact next to it).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kodehub_core::model::{Kind, Language};
use kodehub_core::CodeGraph;
use kodehub_render::{render, test_fn_name, RenderRequest};
use serde::{Deserialize, Serialize};

use crate::build::cargo_bin;
use crate::exec;
use crate::{Result, Runner, RunnerError};

/// One test in a bundle.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BundleTest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub closure_hash: String,
    /// `tests/<fn>.sh`
    pub script: String,
    /// An authored shell test (not a wrapped language test).
    #[serde(default)]
    pub shell: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleManifest {
    pub key: String,
    pub language: Language,
    pub tests: Vec<BundleTest>,
    pub toolchain: String,
    pub generator: String,
    /// Test container type able to run this bundle (`rust` → core layer only, `python`).
    pub container_type: String,
}

/// Outcome of running one script.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScriptResult {
    pub id: String,
    pub name: String,
    pub version: String,
    pub closure_hash: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub output: String,
    pub duration_ms: u64,
}

fn is_shell_test(g: &CodeGraph, id: &str) -> bool {
    g.symbols
        .get(id)
        .and_then(|s| g.versions.get(&s.version))
        .map(|v| v.attributes["shell"] == true)
        .unwrap_or(false)
}

fn script_name(lang: Language, display_name: &str) -> String {
    format!("tests/{}.sh", test_fn_name(lang, display_name))
}

fn header(t: &BundleTest, lang: Language) -> String {
    format!(
        "#!/bin/sh\n# kodehub test: {} ({}, version {}, closure {})\nset -u\ncd \"$(dirname \"$0\")/..\"\n",
        t.name.replace('\n', " "),
        lang.as_str(),
        &t.version[..12.min(t.version.len())],
        &t.closure_hash[..12.min(t.closure_hash.len())],
    )
}

impl Runner {
    /// Bundle key: language, every (test, version, closure) pair, the renderer and toolchain.
    pub fn bundle_key(&self, g: &CodeGraph, lang: Language, tests: &[String]) -> Result<String> {
        let mut parts: Vec<String> = Vec::new();
        for t in tests {
            let s = g.symbol(t)?;
            parts.push(format!("{t}|{}|{}", s.version, g.closure_hash(t)?));
        }
        parts.sort();
        let target = match lang {
            Language::Rust => "rust-test",
            Language::Python => "python-test",
        };
        let tag = self.toolchain_tag(target);
        Ok(
            blake3::hash(format!("{}|{tag}|{}", lang.as_str(), parts.join(";")).as_bytes())
                .to_hex()[..40]
                .to_string(),
        )
    }

    /// Build a test bundle (the build container's job). `Ok(Err(log))` on a compile failure.
    pub async fn build_test_bundle(
        &self,
        g: &CodeGraph,
        lang: Language,
        tests: &[String],
    ) -> Result<std::result::Result<(Vec<u8>, BundleManifest), String>> {
        if tests.is_empty() {
            return Err(RunnerError::Invalid(
                "a bundle needs at least one test".into(),
            ));
        }
        for t in tests {
            let s = g.symbol(t)?;
            if s.kind != Kind::Test || s.language != lang {
                return Err(RunnerError::Invalid(format!(
                    "`{}` is not a {} test",
                    s.display_name,
                    lang.as_str()
                )));
            }
        }
        let key = self.bundle_key(g, lang, tests)?;
        let shell: Vec<String> = tests
            .iter()
            .filter(|t| is_shell_test(g, t))
            .cloned()
            .collect();
        let wrapped: Vec<String> = tests
            .iter()
            .filter(|t| !is_shell_test(g, t))
            .cloned()
            .collect();
        // roots: the wrapped tests, plus whatever authored shell tests call into
        let mut roots = wrapped.clone();
        for t in &shell {
            for e in g
                .edges
                .iter()
                .filter(|e| &e.from == t && e.kind.is_build_dependency())
            {
                if g.symbols.get(&e.to).is_some_and(|s| s.language == lang) {
                    roots.push(e.to.clone());
                }
            }
        }
        roots.sort();
        roots.dedup();
        std::fs::create_dir_all(self.render_root())?;
        let stage = tempfile::Builder::new()
            .prefix(&format!("bundle-{}-", &key[..12]))
            .tempdir_in(self.render_root())?;
        let out = stage.path();
        std::fs::create_dir_all(out.join("tests"))?;
        let mut manifest_tests = Vec::new();
        for t in tests {
            let s = g.symbol(t)?;
            manifest_tests.push(BundleTest {
                id: t.clone(),
                name: s.display_name.clone(),
                version: s.version.clone(),
                closure_hash: g.closure_hash(t)?,
                script: script_name(lang, &s.display_name),
                shell: shell.contains(t),
            });
        }
        if !roots.is_empty() {
            let project = render(&RenderRequest {
                graph: g,
                roots: roots.clone(),
                language: lang,
                include_tests: false,
                namespaces: vec![],
                harness: None,
            })?;
            if project.ext.is_some() {
                return Err(RunnerError::Invalid(
                    "PyO3 extension tests run in the build container itself (not bundled)".into(),
                ));
            }
            let work = tempfile::Builder::new()
                .prefix(&format!("bsrc-{}-", &key[..12]))
                .tempdir_in(self.render_root())?;
            project.write_to(work.path())?;
            match lang {
                Language::Rust => {
                    if let Err(log) = self
                        .compile_rust_tests(work.path(), out, &manifest_tests)
                        .await?
                    {
                        return Ok(Err(log));
                    }
                }
                Language::Python => {
                    if let Err(log) = self.vendor_python(&project, work.path(), out).await? {
                        return Ok(Err(log));
                    }
                }
            }
        }
        for t in &manifest_tests {
            let body = if t.shell {
                let v = g.version(&g.symbol(&t.id)?.version)?;
                format!("{}{}\n", header(t, lang), v.body)
            } else {
                let f = test_fn_name(lang, &t.name);
                match lang {
                    Language::Rust => format!(
                        "{}exec ./bin/kodehub-tests --exact kodehub_tests::{f} --test-threads=1 -q\n",
                        header(t, lang)
                    ),
                    Language::Python => format!(
                        "{}export PYTHONPATH=\"$PWD/pkg:$PWD/vendor\" PYTHONDONTWRITEBYTECODE=1\ncd pkg\nexec \"${{KODEHUB_PYTHON:-python3}}\" -m pytest -q -p no:cacheprovider \"tests/test_kodehub.py::{f}\"\n",
                        header(t, lang)
                    ),
                }
            };
            std::fs::write(out.join(&t.script), body)?;
        }
        let manifest = BundleManifest {
            key: key.clone(),
            language: lang,
            tests: manifest_tests,
            toolchain: self.toolchain_tag(match lang {
                Language::Rust => "rust-test",
                Language::Python => "python-test",
            }),
            generator: kodehub_render::GENERATOR.to_string(),
            container_type: lang.as_str().to_string(),
        };
        std::fs::write(
            out.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap_or_default(),
        )?;
        Ok(Ok((tar_dir(out)?, manifest)))
    }

    /// Compile the rendered crate's tests once (`cargo test --no-run`) and keep the binary.
    async fn compile_rust_tests(
        &self,
        src: &Path,
        out: &Path,
        tests: &[BundleTest],
    ) -> Result<std::result::Result<(), String>> {
        let _g = self.build_lock.lock().await;
        let fetch = self.cargo_fetch(src).await?;
        if !fetch.success() {
            return Ok(Err(format!(
                "dependency fetch failed:\n{}",
                fetch.combined()
            )));
        }
        let mut c = exec::command(
            &cargo_bin(),
            &[
                "test",
                "--no-run",
                "--offline",
                "--lib",
                "--message-format=json",
                "--color",
                "never",
            ],
            self.sandbox,
        );
        c.current_dir(src)
            .env("CARGO_TARGET_DIR", self.cargo_target_dir())
            .env("CARGO_TERM_COLOR", "never");
        let res = exec::run(c, None, Duration::from_secs(900), None).await?;
        if !res.success() {
            return Ok(Err(crate_tail(&res.combined())));
        }
        let exe = res
            .stdout
            .lines()
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v["reason"] == "compiler-artifact" && v["profile"]["test"] == true)
            .filter_map(|v| v["executable"].as_str().map(PathBuf::from))
            .next_back()
            .ok_or_else(|| RunnerError::Build("cargo produced no test executable".into()))?;
        std::fs::create_dir_all(out.join("bin"))?;
        let dest = out.join("bin/kodehub-tests");
        std::fs::copy(&exe, &dest)?;
        // every expected test must exist in the binary, so a missing test can never "pass"
        let list = exec::run(
            exec::command(&dest, &["--list", "--format", "terse"], false),
            None,
            Duration::from_secs(60),
            None,
        )
        .await?;
        let listed: std::collections::BTreeSet<String> = list
            .stdout
            .lines()
            .filter_map(|l| l.strip_suffix(": test"))
            .map(str::to_string)
            .collect();
        for t in tests.iter().filter(|t| !t.shell) {
            let want = format!("kodehub_tests::{}", test_fn_name(Language::Rust, &t.name));
            if !listed.contains(&want) {
                return Ok(Err(format!("test `{want}` is not in the compiled binary")));
            }
        }
        Ok(Ok(()))
    }

    /// Copy the rendered package and vendor its dependencies (and pytest) into the bundle.
    async fn vendor_python(
        &self,
        project: &kodehub_render::RenderedProject,
        src: &Path,
        out: &Path,
    ) -> Result<std::result::Result<(), String>> {
        let pkg = out.join("pkg");
        copy_dir(src, &pkg)?;
        let mut packages: Vec<String> = project
            .files
            .get("requirements.txt")
            .map(|r| {
                r.lines()
                    .map(String::from)
                    .filter(|l| !l.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        packages.push("pytest".into());
        let py = match self.ensure_python(&[]).await {
            Ok(p) => p,
            Err(RunnerError::Build(log)) => return Ok(Err(log)),
            Err(e) => return Err(e),
        };
        let vendor = out.join("vendor");
        let mut args: Vec<String> = Vec::new();
        let program = match crate::pyenv::which("uv") {
            Some(uv) => {
                args.extend(["pip", "install", "--quiet", "--python"].map(String::from));
                args.push(py.to_string_lossy().into());
                args.push("--target".into());
                args.push(vendor.to_string_lossy().into());
                uv
            }
            None => {
                args.extend(["-m", "pip", "install", "--quiet", "--target"].map(String::from));
                args.push(vendor.to_string_lossy().into());
                py.clone()
            }
        };
        args.extend(packages);
        let argv: Vec<&str> = args.iter().map(String::as_str).collect();
        let res = exec::run(
            exec::command(&program, &argv, false),
            None,
            Duration::from_secs(600),
            None,
        )
        .await?;
        if !res.success() {
            return Ok(Err(format!(
                "vendoring python packages failed:\n{}",
                res.combined()
            )));
        }
        Ok(Ok(()))
    }

    /// Run a bundle's scripts (the test container's job). `only` limits the tests.
    pub async fn run_test_bundle(
        &self,
        bundle: &[u8],
        only: Option<&[String]>,
    ) -> Result<Vec<ScriptResult>> {
        std::fs::create_dir_all(self.render_root())?;
        let dir = tempfile::Builder::new()
            .prefix("run-")
            .tempdir_in(self.render_root())?;
        untar(bundle, dir.path())?;
        let manifest: BundleManifest =
            serde_json::from_slice(&std::fs::read(dir.path().join("manifest.json"))?)
                .map_err(|e| RunnerError::Invalid(format!("bad bundle manifest: {e}")))?;
        let py = self.python();
        let mut out = Vec::new();
        for t in &manifest.tests {
            if let Some(o) = only {
                if !o.contains(&t.id) {
                    continue;
                }
            }
            let start = Instant::now();
            let script = dir.path().join(&t.script);
            let mut c = exec::command(
                Path::new("/bin/sh"),
                &[script.to_str().unwrap_or_default()],
                self.sandbox,
            );
            c.current_dir(dir.path());
            if py.exists() {
                c.env("KODEHUB_PYTHON", &py);
            }
            let r = exec::run(
                c,
                None,
                self.test_timeout.min(Duration::from_secs(300)),
                None,
            )
            .await?;
            out.push(ScriptResult {
                id: t.id.clone(),
                name: t.name.clone(),
                version: t.version.clone(),
                closure_hash: t.closure_hash.clone(),
                status: if r.success() { "pass" } else { "fail" }.into(),
                exit_code: r.exit_code,
                output: crate_tail(&r.combined()),
                duration_ms: start.elapsed().as_millis() as u64,
            });
        }
        Ok(out)
    }
}

fn crate_tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(120)..].join("\n")
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        let p = e.path();
        let dest = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_dir(&p, &dest)?;
        } else {
            std::fs::copy(&p, &dest)?;
        }
    }
    Ok(())
}

/// Deterministic tar of a directory (sorted, zero mtimes).
pub fn tar_dir(dir: &Path) -> Result<Vec<u8>> {
    fn walk(base: &Path, d: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
        let mut ents: Vec<_> = std::fs::read_dir(d)?.collect::<std::io::Result<_>>()?;
        ents.sort_by_key(|e| e.file_name());
        for e in ents {
            let p = e.path();
            if e.file_type()?.is_dir() {
                walk(base, &p, out)?;
            } else {
                out.push(p.strip_prefix(base).unwrap().to_path_buf());
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    let mut tb = tar::Builder::new(Vec::new());
    for f in files {
        let data = std::fs::read(dir.join(&f))?;
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        let exec = f.starts_with("bin") || f.extension().is_some_and(|x| x == "sh");
        h.set_mode(if exec { 0o755 } else { 0o644 });
        h.set_mtime(0);
        h.set_cksum();
        tb.append_data(&mut h, &f, data.as_slice())?;
    }
    Ok(tb.into_inner()?)
}

pub fn untar(data: &[u8], dir: &Path) -> Result<()> {
    let mut ar = tar::Archive::new(std::io::Cursor::new(data));
    ar.set_preserve_permissions(true);
    for e in ar.entries()? {
        let mut e = e?;
        let p = e.path()?.to_path_buf();
        if p.components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(RunnerError::Invalid(format!(
                "unsafe path in bundle: {}",
                p.display()
            )));
        }
        e.unpack_in(dir)?;
    }
    Ok(())
}
