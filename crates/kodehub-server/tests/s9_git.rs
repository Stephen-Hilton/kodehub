//! S9 acceptance: Git import creates functions (not files) as code nodes through an import agent's
//! mapping; re-import is refused; Git export round-trips the seed project through a repository;
//! a project's database backup restores alone.
mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use common::*;
use kodehub_agents::{AgentBackend, AgentOutput, AgentTask, EngineOptions};
use kodehub_runner::jobs::JobRuntime;
use kodehub_server::orchestrator::{AgentDispatcher, Orchestrator};
use serde_json::{json, Value};

fn sh(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

fn fixture_repo(dir: &Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("app")).unwrap();
    std::fs::create_dir_all(dir.join("web")).unwrap();
    std::fs::write(
        dir.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub struct Counter {\n    pub n: u32,\n}\n\nimpl Counter {\n    pub fn bump(&mut self) -> u32 {\n        self.n += 1;\n        self.n\n    }\n}\n\npub fn double(x: i32) -> i32 {\n    helper(x) * 2\n}\n\nfn helper(x: i32) -> i32 {\n    x\n}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("app/util.py"),
        "def greet(name):\n    return f\"hi {name}\"\n\nclass Cart:\n    def total(self, items):\n        return sum(items)\n",
    )
    .unwrap();
    std::fs::write(dir.join("web/index.js"), "export const x = 1;\n").unwrap();
    sh(dir, &["init", "-q", "-b", "main"]);
    sh(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "add",
            "-A",
        ],
    );
    sh(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    );
}

/// The import agent: map Rust units to one component, Python to another.
struct ImportAgent {
    seen: Mutex<Vec<String>>,
}

#[async_trait::async_trait]
impl AgentBackend for ImportAgent {
    fn name(&self) -> &str {
        "scripted"
    }
    async fn run(&self, task: &AgentTask) -> kodehub_agents::Result<AgentOutput> {
        self.seen.lock().unwrap().push(task.label.clone());
        let import = regex::Regex::new(r#""import":"(wi-[0-9a-f]+)""#)
            .unwrap()
            .captures(&task.input.replace([' ', '\n'], ""))
            .map(|c| c[1].to_string())
            .unwrap_or_default();
        let json = json!({"change_log": "one context, one container, two components", "actions": [{
        "type": "import_mapping", "import": import,
        "contexts": [{"name": "Demo"}],
        "containers": [{"name": "demo-lib", "parent": "Demo"}],
        "components": [
            {"name": "math", "parent": "demo-lib", "units": ["demo::"]},
            {"name": "py-util", "parent": "demo-lib", "units": ["app.util"]}
        ]}]});
        Ok(AgentOutput {
            text: json.to_string(),
            json,
            transcript: vec![],
            backend: "scripted".into(),
            model: "m".into(),
            duration_ms: 0,
            usage: json!({}),
        })
    }
}

fn orchestrator(t: &TestServer, objects: &Path) -> Orchestrator {
    let r = t.state.runner.clone();
    let storage = format!("file://{}", objects.display());
    Orchestrator {
        store: t.store.clone(),
        runner: r.clone(),
        storage: kodehub_storage::StorageClient::new(&storage, None),
        jobs: JobRuntime::Local {
            worker: repo().join("target/debug/kodehub-worker"),
            env: vec![
                ("KODEHUB_HOME".into(), r.home.display().to_string()),
                (
                    "KODEHUB_CARGO_TARGET".into(),
                    r.cargo_target.display().to_string(),
                ),
                ("KODEHUB_PYENV".into(), r.pyenv.display().to_string()),
                ("KODEHUB_STORAGE_URL".into(), storage),
            ],
        },
        owner: "orch".into(),
        events: t.state.events.clone(),
    }
}

#[tokio::test]
async fn import_maps_functions_export_round_trips_backups_restore() {
    let agent = Arc::new(ImportAgent {
        seen: Mutex::new(vec![]),
    });
    let a2 = agent.clone();
    let t = TestServer::start_with("s9", move |st, _| {
        st.dispatch = false;
        kodehub_server::install_agents(st, "http://127.0.0.1:1", EngineOptions::default());
        let e = Arc::try_unwrap(st.engine.take().unwrap()).ok().unwrap();
        st.engine = Some(Arc::new(e.with_backend(a2)));
    })
    .await;
    let objects = tempfile::tempdir().unwrap();
    let o = orchestrator(&t, objects.path());
    let d = AgentDispatcher {
        store: t.store.clone(),
        engine: t.state.engine.clone().unwrap(),
        owner: "agents".into(),
        events: t.state.events.clone(),
    };
    let pid = t.store.project_id().unwrap();
    let src = tempfile::tempdir().unwrap();
    fixture_repo(src.path());
    let url = format!("file://{}", src.path().display());

    // import: the container parses, the import agent maps, the changeset promotes
    t.ok("POST", "/api/git/import", Some(json!({"repo_url": url})))
        .await;
    assert!(o.tick().await.unwrap());
    let items = t.store.list_work_items(Some(&pid), None, 50).await.unwrap();
    let gi = items
        .iter()
        .find(|w| w["agent_type"] == "git_import")
        .unwrap();
    assert_eq!(gi["state"], "done", "{gi}");
    assert!(
        gi["result"]["staged"].as_u64().unwrap() >= 7,
        "{}",
        gi["result"]
    );
    assert!(d.tick().await.unwrap(), "the import agent ran");
    let ia = t
        .store
        .list_work_items(Some(&pid), None, 50)
        .await
        .unwrap()
        .into_iter()
        .find(|w| w["agent_type"] == "import")
        .unwrap();
    assert_eq!(ia["state"], "done", "{ia}");
    // functions, methods and classes are code nodes — files are not
    let syms: Vec<String> = t
        .store
        .aql(
            "FOR s IN @@symbols SORT s.display_name RETURN s.display_name",
            json!({}),
        )
        .await
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    for want in [
        "double",
        "helper",
        "Counter",
        "Counter::bump",
        "app.util.greet",
        "app.util.Cart",
        "app.util.Cart.total",
    ] {
        assert!(
            syms.contains(&want.to_string()),
            "missing {want} in {syms:?}"
        );
    }
    assert!(
        !syms
            .iter()
            .any(|s| s.ends_with(".rs") || s.ends_with(".py")),
        "{syms:?}"
    );
    let d_body = t
        .store
        .aql("FOR s IN @@symbols FILTER s.display_name == 'double' FOR e IN @@has_version FILTER e._from == s._id AND e.state == 'stable' RETURN DOCUMENT(e._to).body", json!({}))
        .await
        .unwrap();
    assert_eq!(d_body[0], "helper(x) * 2");
    // the C4 mapping, a generated requirement per component, no orphans, opaque JS kept aside
    for n in [
        "contexts/demo",
        "containers/demo-lib",
        "components/math",
        "components/py-util",
    ] {
        assert!(t.store.get_node(n).await.is_ok(), "{n}");
    }
    assert!(t.store.orphans().await.unwrap().is_empty());
    let opaque = t
        .store
        .aql(
            "FOR u IN @@import_units FILTER u.state == 'opaque' RETURN u.file",
            json!({}),
        )
        .await
        .unwrap();
    assert_eq!(opaque, vec![json!("web/index.js")]);
    assert!(t.ok("GET", "/api/invariants", None).await["ok"] == true);
    // importing the same repository again is a reverse sync: refused
    let (s, _) = t
        .req("POST", "/api/git/import", Some(json!({"repo_url": url})))
        .await;
    assert_eq!(s, 409);

    // export round trip: the seed project, in a second project, to a bare repository
    let p2 = t
        .uok("POST", "/api/projects", Some(json!({"name": "Seeded"})))
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    let q = |path: &str| format!("{path}?project={p2}");
    t.ok("POST", &q("/api/seed"), Some(json!({"path": seed_path()})))
        .await;
    let bare = tempfile::tempdir().unwrap();
    sh(bare.path(), &["init", "-q", "--bare", "-b", "main"]);
    let burl = format!("file://{}", bare.path().display());
    t.ok(
        "POST",
        &q("/api/git/export"),
        Some(json!({"repo_url": burl})),
    )
    .await;
    assert!(o.tick().await.unwrap());
    let ex = t
        .store
        .list_work_items(Some(&p2), None, 50)
        .await
        .unwrap()
        .into_iter()
        .find(|w| w["agent_type"] == "git_export")
        .unwrap();
    assert_eq!(ex["state"], "done", "{ex}");
    assert_eq!(ex["result"]["pushed"], true);
    let clone = tempfile::tempdir().unwrap();
    sh(clone.path(), &["clone", "-q", &burl, "out"]);
    let out = clone.path().join("out");
    let lib = std::fs::read_to_string(out.join("rust/src/lib.rs")).unwrap();
    assert!(lib.contains("fn verify_credentials"), "{lib}");
    assert!(out.join("KODEHUB.md").exists());
    assert!(std::fs::read_to_string(out.join("KODEHUB.md"))
        .unwrap()
        .contains("source of truth"));
    let py: Vec<_> = walkdir(out.join("python"));
    assert!(py.iter().any(|f| f.ends_with(".py")), "{py:?}");
    // a second export with nothing changed pushes nothing
    t.ok(
        "POST",
        &q("/api/git/export"),
        Some(json!({"repo_url": burl})),
    )
    .await;
    assert!(o.tick().await.unwrap());
    let ex2: Vec<Value> = t
        .store
        .list_work_items(Some(&p2), None, 50)
        .await
        .unwrap()
        .into_iter()
        .filter(|w| w["agent_type"] == "git_export")
        .collect();
    assert!(
        ex2.iter().any(|w| w["result"]["pushed"] == false),
        "{ex2:?}"
    );
    // …and the exported repository imports back: the seed's functions are found again
    let p3 = t
        .uok("POST", "/api/projects", Some(json!({"name": "Round trip"})))
        .await["id"]
        .as_str()
        .unwrap()
        .to_string();
    t.ok(
        "POST",
        &format!("/api/git/import?project={p3}"),
        Some(json!({"repo_url": burl})),
    )
    .await;
    assert!(o.tick().await.unwrap());
    let staged = t
        .store
        .for_project(&p3)
        .aql("FOR u IN @@import_units RETURN u.name", json!({}))
        .await
        .unwrap();
    assert!(
        staged.iter().any(|n| n == "verify_credentials"),
        "{staged:?}"
    );
    assert!(
        staged
            .iter()
            .any(|n| n.as_str().unwrap().ends_with(".login")),
        "{staged:?}"
    );

    // a project's database backup restores alone, as a new project
    let b = t.uok("POST", &q("/api/backups"), None).await;
    let key = b["key"].as_str().unwrap().to_string();
    assert!(key.starts_with(&format!("projects/{p2}/backups/")));
    std::env::set_var("KODEHUB_STORAGE_URL", "");
    let r = t
        .uok(
            "POST",
            "/api/projects/restore",
            Some(json!({"key": key, "name": "Restored"})),
        )
        .await;
    let p4 = r["project"].as_str().unwrap().to_string();
    let count = |p: &str| {
        let s = t.store.for_project(p);
        async move { s.aql("RETURN LENGTH(@@symbols)", json!({})).await.unwrap()[0].clone() }
    };
    assert_eq!(count(&p4).await, count(&p2).await);
    assert!(t
        .store
        .for_project(&p4)
        .check_invariants()
        .await
        .unwrap()
        .is_empty());
    t.stop().await;
}

fn walkdir(d: std::path::PathBuf) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&d) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(walkdir(p));
            } else {
                out.push(p.display().to_string());
            }
        }
    }
    out
}
