//! Test rendering, execution and result storage (doc 03 "Test"). Results are stored per test
//! version and closure hash; a cached pass for the same closure is not re-run.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use kodehub_core::model::{now_iso, Kind, Language};
use kodehub_core::CodeGraph;
use kodehub_render::{render, test_fn_name, RenderRequest};
use kodehub_store::Store;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedSender;

use crate::build::cargo_bin;
use crate::exec::{self, OutputLine};
use crate::{Result, Runner, RunnerError};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum TestScope {
    All,
    Requirement(String),
    Symbol(String),
    Tests(Vec<String>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestOutcome {
    pub test: String,
    pub name: String,
    pub language: Language,
    pub version: String,
    pub closure_hash: String,
    /// pass | fail
    pub status: String,
    pub output: String,
    pub duration_ms: u64,
    pub cached: bool,
}

/// Tests whose closure includes `symbol`, plus tests attached via `tested_by`.
pub fn tests_for_symbol(g: &CodeGraph, symbol: &str) -> BTreeSet<String> {
    g.tests_reaching(symbol)
}

impl Runner {
    pub async fn resolve_scope(
        &self,
        store: &Store,
        g: &CodeGraph,
        scope: &TestScope,
    ) -> Result<Vec<String>> {
        let ids: BTreeSet<String> = match scope {
            TestScope::All => g
                .symbols
                .values()
                .filter(|s| s.kind == Kind::Test)
                .map(|s| s.id.clone())
                .collect(),
            TestScope::Symbol(s) => tests_for_symbol(g, kodehub_store::key_of(s)),
            TestScope::Tests(t) => t
                .iter()
                .map(|x| kodehub_store::key_of(x).to_string())
                .collect(),
            TestScope::Requirement(r) => {
                let cov = store.coverage(Some(kodehub_store::key_of(r))).await?;
                cov.requirements
                    .iter()
                    .flat_map(|r| {
                        r.tests
                            .iter()
                            .map(|t| kodehub_store::key_of(&t.test).to_string())
                    })
                    .collect()
            }
        };
        for id in &ids {
            g.symbol(id)?;
        }
        Ok(ids.into_iter().collect())
    }

    /// Run tests (by symbol id) against graph `g`; store results. `changeset` tags the runs.
    pub async fn run_tests(
        &self,
        store: &Store,
        g: &CodeGraph,
        tests: &[String],
        force: bool,
        changeset: Option<&str>,
        stream: Option<UnboundedSender<OutputLine>>,
    ) -> Result<Vec<TestOutcome>> {
        let mut outcomes = Vec::new();
        let mut todo: BTreeMap<Language, Vec<String>> = BTreeMap::new();
        for t in tests {
            let s = g.symbol(t)?;
            if s.kind != Kind::Test {
                return Err(RunnerError::Invalid(format!(
                    "`{}` is not a test",
                    s.display_name
                )));
            }
            let closure = g.closure_hash(t)?;
            if !force {
                let cached = store
                    .aql(
                        "FOR r IN @@test_runs FILTER r.test_version == @v AND r.closure_hash == @c AND r.status == 'pass'
                         SORT r.ran_at DESC LIMIT 1 RETURN r",
                        json!({"v": s.version, "c": closure}),
                    )
                    .await?;
                if let Some(r) = cached.first() {
                    outcomes.push(TestOutcome {
                        test: t.clone(),
                        name: s.display_name.clone(),
                        language: s.language,
                        version: s.version.clone(),
                        closure_hash: closure,
                        status: "pass".into(),
                        output: r["output"].as_str().unwrap_or("").into(),
                        duration_ms: r["duration_ms"].as_u64().unwrap_or(0),
                        cached: true,
                    });
                    continue;
                }
            }
            todo.entry(s.language).or_default().push(t.clone());
        }
        for (lang, ids) in todo {
            let results = self.run_batch(g, lang, &ids, stream.clone()).await?;
            for o in results {
                store
                    .db
                    .insert(
                        "test_runs",
                        &json!({
                            "test_symbol": o.test,
                            "test_name": o.name,
                            "test_version": o.version,
                            "closure_hash": o.closure_hash,
                            "status": o.status,
                            "output": o.output,
                            "duration_ms": o.duration_ms,
                            "ran_at": now_iso(),
                            "changeset": changeset,
                        }),
                        None,
                    )
                    .await?;
                outcomes.push(o);
            }
        }
        outcomes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(outcomes)
    }

    /// Run a batch of same-language tests in one rendered project. If the batch fails to build,
    /// each test is retried alone so one broken closure doesn't fail unrelated tests.
    async fn run_batch(
        &self,
        g: &CodeGraph,
        lang: Language,
        ids: &[String],
        stream: Option<UnboundedSender<OutputLine>>,
    ) -> Result<Vec<TestOutcome>> {
        let (results, build_failed, log, ms) =
            self.exec_batch(g, lang, ids, stream.clone()).await?;
        let mk = |id: &String, status: &str, output: String, ms: u64| -> Result<TestOutcome> {
            let s = g.symbol(id)?;
            Ok(TestOutcome {
                test: id.clone(),
                name: s.display_name.clone(),
                language: lang,
                version: s.version.clone(),
                closure_hash: g.closure_hash(id)?,
                status: status.into(),
                output,
                duration_ms: ms,
                cached: false,
            })
        };
        if build_failed {
            if ids.len() > 1 {
                let mut all = Vec::new();
                for id in ids {
                    all.extend(
                        Box::pin(self.run_batch(g, lang, std::slice::from_ref(id), stream.clone()))
                            .await?,
                    );
                }
                return Ok(all);
            }
            return Ok(vec![mk(
                &ids[0],
                "fail",
                format!("build failed:\n{log}"),
                ms,
            )?]);
        }
        let per = ms / ids.len().max(1) as u64;
        ids.iter()
            .map(|id| {
                let s = g.symbol(id)?;
                let fname = test_fn_name(lang, &s.display_name);
                match results.get(&fname) {
                    Some((true, out)) => mk(id, "pass", out.clone(), per),
                    Some((false, out)) => mk(id, "fail", out.clone(), per),
                    None => mk(id, "fail", format!("test did not run:\n{log}"), per),
                }
            })
            .collect()
    }

    /// Returns (fn name → (passed, output)), build_failed, full log, duration.
    async fn exec_batch(
        &self,
        g: &CodeGraph,
        lang: Language,
        ids: &[String],
        stream: Option<UnboundedSender<OutputLine>>,
    ) -> Result<(BTreeMap<String, (bool, String)>, bool, String, u64)> {
        let start = Instant::now();
        let project = render(&RenderRequest {
            graph: g,
            roots: ids.to_vec(),
            language: lang,
            include_tests: false,
            namespaces: vec![],
            harness: None,
        })?;
        std::fs::create_dir_all(self.render_root())?;
        let tmp = tempfile::Builder::new()
            .prefix(&format!("test-{}-", &project.manifest.closure_hash[..12]))
            .tempdir_in(self.render_root())?;
        project.write_to(tmp.path())?;
        let mut results = BTreeMap::new();
        match lang {
            Language::Rust => {
                let _g = self.build_lock.lock().await;
                // fetch with network, then compile + run tests offline in the sandbox
                let fetch = self.cargo_fetch(tmp.path()).await?;
                if !fetch.success() {
                    let log = format!("dependency fetch failed:\n{}", fetch.combined());
                    return Ok((results, true, log, 0));
                }
                let mut c = exec::command(
                    &cargo_bin(),
                    &[
                        "test",
                        "--offline",
                        "--lib",
                        "--color",
                        "never",
                        "--",
                        "--test-threads=8",
                    ],
                    self.sandbox,
                );
                c.current_dir(tmp.path())
                    .env("CARGO_TARGET_DIR", self.cargo_target_dir())
                    .env("CARGO_TERM_COLOR", "never");
                let out = exec::run(c, None, self.test_timeout, stream).await?;
                let log = out.combined();
                let line_re =
                    Regex::new(r"^test kodehub_tests::(\w+) \.\.\. (ok|FAILED|ignored)$").unwrap();
                let total_re =
                    Regex::new(r"^test result: \w+\. (\d+) passed; (\d+) failed; (\d+) ignored")
                        .unwrap();
                // Only result lines before the failure details count, every name at most once,
                // and the totals must match libtest's summary — so a test's own output can't
                // forge results (review finding).
                let mut dup = false;
                for l in out.stdout.lines() {
                    if l.starts_with("failures:") || l.starts_with("---- ") || total_re.is_match(l)
                    {
                        break;
                    }
                    if let Some(c) = line_re.captures(l) {
                        if results
                            .insert(c[1].to_string(), (&c[2] == "ok", String::new()))
                            .is_some()
                        {
                            dup = true;
                        }
                    }
                }
                let totals = out
                    .stdout
                    .lines()
                    .filter_map(|l| total_re.captures(l))
                    .next_back()
                    .map(|c| -> (usize, usize, usize) {
                        (
                            c[1].parse().unwrap_or(0),
                            c[2].parse().unwrap_or(0),
                            c[3].parse().unwrap_or(0),
                        )
                    });
                let passed = results.values().filter(|r| r.0).count();
                let consistent = match totals {
                    Some((p, f, ig)) => p == passed && p + f + ig == results.len(),
                    None => results.is_empty(),
                };
                if dup || !consistent {
                    for r in results.values_mut() {
                        *r = (
                            false,
                            "test output inconsistent with the libtest summary; result not trusted"
                                .into(),
                        );
                    }
                }
                // failure details
                let mut current: Option<String> = None;
                let mut buf = String::new();
                let head_re = Regex::new(r"^---- kodehub_tests::(\w+) stdout ----").unwrap();
                for l in out.stdout.lines() {
                    if let Some(c) = head_re.captures(l) {
                        if let Some(n) = current.take() {
                            if let Some(r) = results.get_mut(&n) {
                                r.1 = buf.trim().to_string();
                            }
                        }
                        current = Some(c[1].to_string());
                        buf.clear();
                    } else if current.is_some() {
                        if l.starts_with("failures:") || l.starts_with("test result:") {
                            if let Some(n) = current.take() {
                                if let Some(r) = results.get_mut(&n) {
                                    r.1 = buf.trim().to_string();
                                }
                            }
                        } else {
                            buf.push_str(l);
                            buf.push('\n');
                        }
                    }
                }
                let build_failed = results.is_empty() && !out.success();
                Ok((
                    results,
                    build_failed,
                    tail(&log, 120),
                    start.elapsed().as_millis() as u64,
                ))
            }
            Language::Python => {
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
                let py = match self.ensure_python(&packages).await {
                    Ok(p) => p,
                    Err(RunnerError::Build(log)) => return Ok((results, true, log, 0)),
                    Err(e) => return Err(e),
                };
                if let Some(ext) = &project.ext {
                    let (ok, log, _) = self
                        .build_ext(ext, &tmp.path().join("kodehub_gen"), false)
                        .await?;
                    if !ok {
                        return Ok((
                            results,
                            true,
                            format!("PyO3 extension build failed:\n{log}"),
                            0,
                        ));
                    }
                }
                let mut c = exec::command(
                    &py,
                    &[
                        "-m",
                        "pytest",
                        "-rA",
                        "-q",
                        "-p",
                        "no:cacheprovider",
                        "--junitxml=.kode-junit.xml",
                        "tests/test_kodehub.py",
                    ],
                    self.sandbox,
                );
                c.current_dir(tmp.path())
                    .env("PYTHONDONTWRITEBYTECODE", "1")
                    .env("PYTHONPATH", tmp.path());
                let out = exec::run(
                    c,
                    None,
                    Duration::from_secs(300).min(self.test_timeout),
                    stream,
                )
                .await?;
                let log = out.combined();
                // Results come from pytest's JUnit XML (written after the session ends), never
                // from stdout, so printed text can't forge a pass.
                let xml =
                    std::fs::read_to_string(tmp.path().join(".kode-junit.xml")).unwrap_or_default();
                let case_re =
                    Regex::new(r#"(?s)<testcase\b([^>]*?)(/>|>(.*?)</testcase>)"#).unwrap();
                let name_re = Regex::new(r#"\bname="([^"]+)""#).unwrap();
                for c in case_re.captures_iter(&xml) {
                    let Some(n) = name_re.captures(&c[1]) else {
                        continue;
                    };
                    let inner = c.get(3).map(|m| m.as_str()).unwrap_or("");
                    let failed = inner.contains("<failure")
                        || inner.contains("<error")
                        || inner.contains("<skipped");
                    let detail = if failed {
                        xml_text(inner)
                    } else {
                        String::new()
                    };
                    results.insert(
                        n[1].to_string(),
                        (!failed, detail.chars().take(4000).collect()),
                    );
                }
                let build_failed = results.is_empty() && !out.success();
                Ok((
                    results,
                    build_failed,
                    tail(&log, 120),
                    start.elapsed().as_millis() as u64,
                ))
            }
        }
    }

    /// Latest stored results for display.
    pub async fn latest_results(store: &Store, limit: usize) -> Result<Vec<Value>> {
        Ok(store
            .aql(
                "FOR r IN @@test_runs SORT r.ran_at DESC LIMIT @n RETURN r",
                json!({"n": limit}),
            )
            .await?)
    }
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Text content of a JUnit XML fragment (failure message + traceback).
fn xml_text(s: &str) -> String {
    let tags = Regex::new(r"<[^>]+>").unwrap();
    let msg = Regex::new(r#"message="([^"]*)""#)
        .unwrap()
        .captures(s)
        .map(|c| c[1].to_string())
        .unwrap_or_default();
    let body = tags.replace_all(s, "").to_string();
    format!("{msg}\n{body}")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#10;", "\n")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
        .trim()
        .to_string()
}
