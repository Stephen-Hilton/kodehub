//! Build cache keyed by (closure hash, build target). Hit → reuse; miss → render, compile, store.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kodehub_core::model::{now_iso, Language};
use kodehub_core::CodeGraph;
use kodehub_render::{render, HarnessSpec, RenderRequest, RenderedProject};
use kodehub_store::Store;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::exec;
use crate::pyenv::which;
use crate::{Result, Runner, RunnerError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildTarget {
    /// Library build of a set of roots (compile check), with namespace re-exports.
    Lib {
        language: Language,
        roots: Vec<String>,
        namespaces: Vec<kodehub_render::NamespaceSpec>,
    },
    /// Run-now harness for one function.
    Run { language: Language, symbol: String },
}

impl BuildTarget {
    pub fn name(&self) -> &'static str {
        match self {
            BuildTarget::Lib {
                language: Language::Rust,
                ..
            } => "rust-lib",
            BuildTarget::Lib {
                language: Language::Python,
                ..
            } => "python-pkg",
            BuildTarget::Run {
                language: Language::Rust,
                ..
            } => "rust-run",
            BuildTarget::Run {
                language: Language::Python,
                ..
            } => "python-run",
        }
    }
    pub fn language(&self) -> Language {
        match self {
            BuildTarget::Lib { language, .. } | BuildTarget::Run { language, .. } => *language,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildOutcome {
    pub target: String,
    pub closure_hash: String,
    pub cache_hit: bool,
    pub ok: bool,
    pub dir: PathBuf,
    pub duration_ms: u64,
    pub log: String,
    pub packages: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Marker {
    status: String,
    built_at: String,
    log: String,
    #[serde(default)]
    packages: Vec<String>,
}

/// Per-closure harness binary name (avoids every crate writing the same target/debug path).
pub fn bin_name(closure_hash: &str) -> String {
    format!("lr_{}", &closure_hash[..16])
}

pub fn cargo_bin() -> PathBuf {
    which("cargo").unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cargo/bin/cargo")
    })
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

impl Runner {
    pub fn render_target(&self, g: &CodeGraph, t: &BuildTarget) -> Result<RenderedProject> {
        let req = match t {
            BuildTarget::Lib {
                language,
                roots,
                namespaces,
            } => RenderRequest {
                graph: g,
                roots: roots.clone(),
                language: *language,
                include_tests: false,
                namespaces: namespaces.clone(),
                harness: None,
            },
            BuildTarget::Run { language, symbol } => RenderRequest {
                graph: g,
                roots: vec![symbol.clone()],
                language: *language,
                include_tests: false,
                namespaces: vec![],
                harness: Some(HarnessSpec {
                    symbol: symbol.clone(),
                }),
            },
        };
        Ok(render(&req)?)
    }

    fn read_marker(dir: &Path) -> Option<Marker> {
        std::fs::read_to_string(dir.join("artifact.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
    }

    /// Build (or reuse) a target. Never errors on compile failure — check `ok`/`log`.
    pub async fn build(&self, g: &CodeGraph, t: &BuildTarget) -> Result<BuildOutcome> {
        let start = Instant::now();
        let project = self.render_target(g, t)?;
        let closure = project.manifest.closure_hash.clone();
        let tname = if project.ext.is_some() {
            format!("{}+ext", t.name())
        } else {
            t.name().to_string()
        };
        let ns_tag = match t {
            // namespace re-exports change the rendered crate but not the closure (final review #6)
            BuildTarget::Lib { namespaces, .. } if !namespaces.is_empty() => {
                let s = serde_json::to_string(namespaces).unwrap_or_default();
                format!("-ns{}", &blake3::hash(s.as_bytes()).to_hex()[..8])
            }
            _ => String::new(),
        };
        let tname = format!("{tname}{ns_tag}");
        let dir = self.cache_dir(&closure, &tname);
        let hit = |m: Marker, dir: PathBuf| BuildOutcome {
            target: t.name().into(),
            closure_hash: closure.clone(),
            cache_hit: true,
            ok: m.status == "ok",
            dir,
            duration_ms: start.elapsed().as_millis() as u64,
            log: m.log,
            packages: m.packages,
        };
        if let Some(m) = Self::read_marker(&dir) {
            return Ok(hit(m, dir));
        }
        let _guard = self.build_lock.lock().await;
        if let Some(m) = Self::read_marker(&dir) {
            return Ok(hit(m, dir));
        }
        std::fs::create_dir_all(&dir)?;
        let packages: Vec<String> = project
            .files
            .get("requirements.txt")
            .map(|r| {
                r.lines()
                    .map(String::from)
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        let (ok, log, cacheable) = match t.language() {
            Language::Rust => self.build_rust(&project, t, &dir).await?,
            Language::Python => self.build_python(&project, &dir, &packages).await?,
        };
        let marker = Marker {
            status: if ok { "ok" } else { "failed" }.into(),
            built_at: now_iso(),
            log: tail(&log, 200),
            packages: packages.clone(),
        };
        if ok || cacheable {
            std::fs::write(
                dir.join("artifact.json"),
                serde_json::to_string_pretty(&marker).unwrap(),
            )?;
        }
        Ok(BuildOutcome {
            target: t.name().into(),
            closure_hash: closure,
            cache_hit: false,
            ok,
            dir,
            duration_ms: start.elapsed().as_millis() as u64,
            log: marker.log,
            packages,
        })
    }

    async fn build_rust(
        &self,
        project: &RenderedProject,
        t: &BuildTarget,
        dir: &Path,
    ) -> Result<(bool, String, bool)> {
        std::fs::create_dir_all(self.render_root())?;
        let tmp = tempfile::Builder::new()
            .prefix(&format!("{}-", &project.manifest.closure_hash[..12]))
            .tempdir_in(self.render_root())?;
        project.write_to(tmp.path())?;
        // 1. fetch dependencies (network, runs no third-party code) …
        let fetch = self.cargo_fetch(tmp.path()).await?;
        if !fetch.success() {
            return Ok((
                false,
                format!(
                    "dependency fetch failed (not cached):\n{}",
                    fetch.combined()
                ),
                false,
            ));
        }
        // 2. … then compile offline inside the sandbox (build scripts / proc-macros run here)
        let bin = crate::build::bin_name(&project.manifest.closure_hash);
        let args: Vec<&str> = match t {
            BuildTarget::Run { .. } => {
                vec!["build", "--offline", "--bin", &bin, "--color", "never"]
            }
            BuildTarget::Lib { .. } => vec!["build", "--offline", "--lib", "--color", "never"],
        };
        let mut c = exec::command(&cargo_bin(), &args, self.sandbox);
        c.current_dir(tmp.path())
            .env("CARGO_TARGET_DIR", self.cargo_target_dir())
            .env("CARGO_TERM_COLOR", "never");
        let out = exec::run(c, None, Duration::from_secs(900), None).await?;
        if out.success() {
            if let BuildTarget::Run { .. } = t {
                let built = self.cargo_target_dir().join("debug").join(&bin);
                std::fs::copy(&built, dir.join("kodehub_run"))?;
            }
        }
        // keep the rendered sources next to the artifact for inspection
        std::fs::copy(
            tmp.path().join("render-manifest.json"),
            dir.join("render-manifest.json"),
        )?;
        let _ = std::fs::create_dir_all(dir.join("src"));
        let _ = std::fs::copy(tmp.path().join("src/lib.rs"), dir.join("src/lib.rs"));
        // Only genuine compile errors are cached; timeouts / environment failures are retried.
        let deterministic = out.success()
            || out.combined().contains("error[E")
            || out.combined().contains("could not compile");
        Ok((
            out.success(),
            out.combined(),
            deterministic && !out.timed_out,
        ))
    }

    /// `cargo fetch` (network allowed, no third-party code executed).
    pub(crate) async fn cargo_fetch(&self, dir: &Path) -> Result<crate::exec::ExecOutput> {
        let mut c = exec::command(&cargo_bin(), &["fetch", "--color", "never"], false);
        c.current_dir(dir)
            .env("CARGO_TARGET_DIR", self.cargo_target_dir());
        Ok(exec::run(c, None, Duration::from_secs(600), None).await?)
    }

    async fn build_python(
        &self,
        project: &RenderedProject,
        dir: &Path,
        packages: &[String],
    ) -> Result<(bool, String, bool)> {
        project.write_to(dir)?;
        if let Some(ext) = &project.ext {
            let (ok, log, det) = self.build_ext(ext, &dir.join("kodehub_gen"), true).await?;
            if !ok {
                return Ok((false, format!("PyO3 extension build failed:\n{log}"), det));
            }
        }
        let py = match self.ensure_python(packages).await {
            Ok(p) => p,
            Err(RunnerError::Build(log)) => return Ok((false, log, false)),
            Err(e) => return Err(e),
        };
        let mut c = exec::command(&py, &["-c", "import kodehub_gen"], self.sandbox);
        c.current_dir(dir).env("PYTHONDONTWRITEBYTECODE", "1");
        let out = exec::run(c, None, Duration::from_secs(60), None).await?;
        Ok((out.success(), out.combined(), !out.timed_out))
    }

    /// Record an artifact in the store (`artifacts` collection).
    pub async fn record_artifact(&self, store: &Store, b: &BuildOutcome) -> Result<()> {
        let key = blake3::hash(format!("{}|{}", b.closure_hash, b.target).as_bytes()).to_hex()
            [..24]
            .to_string();
        store
            .db
            .upsert(
                "artifacts",
                &json!({
                    "_key": key,
                    "closure_hash": b.closure_hash,
                    "build_target": b.target,
                    "path": b.dir.to_string_lossy(),
                    "built_at": now_iso(),
                    "status": if b.ok { "ok" } else { "failed" },
                }),
                None,
            )
            .await?;
        Ok(())
    }
}

impl Runner {
    /// Build a PyO3 extension crate and copy it into `pkg_dir` as `kodehub_ext.so` (M8).
    /// `locked`: the caller already holds the build lock (tokio mutexes are not reentrant).
    pub async fn build_ext(
        &self,
        ext: &RenderedProject,
        pkg_dir: &Path,
        locked: bool,
    ) -> Result<(bool, String, bool)> {
        let py = match self.ensure_python(&[]).await {
            Ok(p) => p,
            Err(RunnerError::Build(log)) => return Ok((false, log, false)),
            Err(e) => return Err(e),
        };
        // every extension gets its own target dir: all extension crates must be named
        // `kodehub_ext` (PyInit symbol), so a shared output path could be overwritten by another
        // closure's build between `cargo build` and the copy (final review #1)
        let target = self
            .cargo_target_dir()
            .join("ext")
            .join(&ext.manifest.closure_hash[..16]);
        std::fs::create_dir_all(self.render_root())?;
        let tmp = tempfile::Builder::new()
            .prefix(&format!("ext-{}-", &ext.manifest.closure_hash[..12]))
            .tempdir_in(self.render_root())?;
        ext.write_to(tmp.path())?;
        let _guard = if locked {
            None
        } else {
            Some(self.build_lock.lock().await)
        };
        let fetch = self.cargo_fetch(tmp.path()).await?;
        if !fetch.success() {
            return Ok((
                false,
                format!("dependency fetch failed:\n{}", fetch.combined()),
                false,
            ));
        }
        let mut c = exec::command(
            &cargo_bin(),
            &["build", "--offline", "--lib", "--color", "never"],
            self.sandbox,
        );
        c.current_dir(tmp.path())
            .env("CARGO_TARGET_DIR", &target)
            .env("CARGO_TERM_COLOR", "never")
            .env("PYO3_PYTHON", &py);
        let out = exec::run(c, None, Duration::from_secs(900), None).await?;
        if !out.success() {
            // only genuine compile errors are deterministic enough to cache (final review #2)
            let log = out.combined();
            let det =
                !out.timed_out && (log.contains("error[E") || log.contains("could not compile"));
            return Ok((false, log, det));
        }
        let dbg = target.join("debug");
        let lib = [
            "libkodehub_ext.dylib",
            "libkodehub_ext.so",
            "kodehub_ext.dll",
        ]
        .iter()
        .map(|n| dbg.join(n))
        .find(|p| p.exists())
        .ok_or_else(|| RunnerError::Build("extension library not found after build".into()))?;
        std::fs::create_dir_all(pkg_dir)?;
        std::fs::copy(lib, pkg_dir.join("kodehub_ext.so"))?;
        Ok((true, out.combined(), true))
    }
}
