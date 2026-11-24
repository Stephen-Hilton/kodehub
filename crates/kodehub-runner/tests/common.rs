#![allow(dead_code)]
use std::path::PathBuf;

use kodehub_runner::Runner;
use kodehub_store::seed::import_dir;
use kodehub_store::testutil::{seed_dir, temp_store};
use kodehub_store::{version_content_from_doc, Store};
use serde_json::json;

pub fn repo() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap()
}

/// A runner with a fresh home (cold cache) but the shared test cargo target and python venv.
pub fn runner() -> Runner {
    static BASE: std::sync::OnceLock<Runner> = std::sync::OnceLock::new();
    let base_dir = repo().join(".kode/test");
    let base = BASE.get_or_init(|| {
        let mut r = Runner::new(base_dir.clone());
        r.cargo_target = base_dir.join("cargo-target");
        r.pyenv = base_dir.join("pyenv");
        r
    });
    // clones share build/python locks, cargo target and venv, but get a cold cache
    let mut r = base.clone();
    r.home = base_dir.join(format!("home-{}", uuid()));
    std::fs::create_dir_all(&r.home).unwrap();
    r
}

pub fn uuid() -> String {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", t)
}

pub async fn seeded(prefix: &str) -> Store {
    let s = temp_store(prefix)
        .await
        .expect("arangodb reachable (run `make db`)");
    import_dir(&s, &seed_dir()).await.expect("seed imports");
    s
}

pub async fn edit_body(s: &Store, symbol: &str, f: impl Fn(&str) -> String) -> String {
    let doc = s
        .aql("FOR e IN @@has_version FILTER e._from == CONCAT(@pfx, 'symbols/', @k) AND e.state == 'stable' RETURN DOCUMENT(e._to)", json!({"k": symbol}))
        .await
        .unwrap();
    let mut c = version_content_from_doc(&doc[0]).unwrap();
    c.body = f(&c.body);
    s.write_version(symbol, &c, "test").await.unwrap().version
}
