//! Per-project backups to the cold tier (spec §3.4): a first full snapshot, then incremental
//! bundles, so uploads stay small and one project can be restored alone.
//!
//! A project's objects are every object, in any bucket, whose key starts with
//! `projects/<project>/`. A bundle is a tar with `manifest.json` (project, seq, kind, the full
//! object → etag map after this bundle, and deletions) and `objects/<bucket>/<key>` for the
//! objects it carries. Bundles live upstream at `backups/<project>/<seq:06>-<full|incr>.tar`.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use crate::backend::{valid_bucket, LocalBackend, Upstream};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BackupState {
    pub seq: u64,
    /// `bucket/key` → etag, as of the last bundle.
    pub objects: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub project: String,
    pub seq: u64,
    pub kind: String,
    pub objects: BTreeMap<String, String>,
    pub carried: Vec<String>,
    pub deleted: Vec<String>,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupReport {
    pub project: String,
    pub bundle: Option<String>,
    pub kind: String,
    pub carried: usize,
    pub deleted: usize,
    pub bytes: usize,
}

fn state_path(local: &LocalBackend, project: &str) -> std::path::PathBuf {
    local
        .root
        .join("_state")
        .join("backup")
        .join(format!("{project}.json"))
}

pub fn valid_project(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 64
        && p.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Buckets present locally.
async fn buckets(local: &LocalBackend) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let mut rd = match tokio::fs::read_dir(&local.root).await {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    while let Some(e) = rd.next_entry().await? {
        let n = e.file_name().to_string_lossy().to_string();
        if e.file_type().await?.is_dir() && valid_bucket(&n) {
            out.push(n);
        }
    }
    out.sort();
    Ok(out)
}

/// Projects that have objects locally.
pub async fn projects(local: &LocalBackend) -> Result<Vec<String>> {
    let mut out = std::collections::BTreeSet::new();
    for b in buckets(local).await? {
        for o in local.list(&b, "projects/").await? {
            if let Some(p) = o.key.split('/').nth(1) {
                if valid_project(p) {
                    out.insert(p.to_string());
                }
            }
        }
    }
    Ok(out.into_iter().collect())
}

async fn project_objects(local: &LocalBackend, project: &str) -> Result<BTreeMap<String, String>> {
    let mut m = BTreeMap::new();
    let prefix = format!("projects/{project}/");
    for b in buckets(local).await? {
        for o in local.list(&b, &prefix).await? {
            m.insert(format!("{b}/{}", o.key), o.etag);
        }
    }
    Ok(m)
}

/// Back up one project: full on first run, incremental afterwards. No bundle when nothing
/// changed.
pub async fn backup_project(
    local: &LocalBackend,
    upstream: &Upstream,
    project: &str,
) -> Result<BackupReport> {
    if !valid_project(project) {
        bail!("invalid project id");
    }
    let sp = state_path(local, project);
    let state: BackupState = match tokio::fs::read(&sp).await {
        Ok(b) => serde_json::from_slice(&b).unwrap_or_default(),
        Err(_) => BackupState::default(),
    };
    let now = project_objects(local, project).await?;
    let full = state.seq == 0;
    let carried: Vec<String> = now
        .iter()
        .filter(|(k, e)| full || state.objects.get(*k) != Some(*e))
        .map(|(k, _)| k.clone())
        .collect();
    let deleted: Vec<String> = if full {
        vec![]
    } else {
        state
            .objects
            .keys()
            .filter(|k| !now.contains_key(*k))
            .cloned()
            .collect()
    };
    let kind = if full { "full" } else { "incr" }.to_string();
    if !full && carried.is_empty() && deleted.is_empty() {
        return Ok(BackupReport {
            project: project.into(),
            bundle: None,
            kind: "none".into(),
            carried: 0,
            deleted: 0,
            bytes: 0,
        });
    }
    let seq = state.seq + 1;
    let manifest = Manifest {
        project: project.into(),
        seq,
        kind: kind.clone(),
        objects: now.clone(),
        carried: carried.clone(),
        deleted: deleted.clone(),
        at: chrono::Utc::now().to_rfc3339(),
    };
    let mut tb = tar::Builder::new(Vec::new());
    let mj = serde_json::to_vec_pretty(&manifest)?;
    append(&mut tb, "manifest.json", &mj)?;
    for k in &carried {
        let (b, key) = k.split_once('/').context("bad object id")?;
        let data = local.get(b, key).await?.unwrap_or_default();
        append(&mut tb, &format!("objects/{k}"), &data)?;
    }
    let bytes = tb.into_inner()?;
    let size = bytes.len();
    let name = format!("backups/{project}/{seq:06}-{kind}.tar");
    upstream.put(&name, Bytes::from(bytes)).await?;
    if let Some(d) = sp.parent() {
        tokio::fs::create_dir_all(d).await?;
    }
    let new_state = BackupState { seq, objects: now };
    tokio::fs::write(&sp, serde_json::to_vec(&new_state)?).await?;
    Ok(BackupReport {
        project: project.into(),
        bundle: Some(name),
        kind,
        carried: carried.len(),
        deleted: deleted.len(),
        bytes: size,
    })
}

fn append(tb: &mut tar::Builder<Vec<u8>>, path: &str, data: &[u8]) -> Result<()> {
    let mut h = tar::Header::new_gnu();
    h.set_size(data.len() as u64);
    h.set_mode(0o644);
    h.set_mtime(0);
    h.set_cksum();
    tb.append_data(&mut h, path, data)?;
    Ok(())
}

/// Bundles of a project upstream, oldest first.
pub async fn bundles(upstream: &Upstream, project: &str) -> Result<Vec<String>> {
    let mut v: Vec<String> = upstream
        .list(&format!("backups/{project}/"))
        .await?
        .into_iter()
        .map(|o| o.key)
        .filter(|k| k.ends_with(".tar"))
        .collect();
    v.sort();
    Ok(v)
}

/// Restore one project from its bundles: from the last full snapshot forward. Objects of other
/// projects are untouched; objects not in the final manifest are removed.
pub async fn restore_project(
    local: &LocalBackend,
    upstream: &Upstream,
    project: &str,
) -> Result<serde_json::Value> {
    if !valid_project(project) {
        bail!("invalid project id");
    }
    let all = bundles(upstream, project).await?;
    let start = all
        .iter()
        .rposition(|b| b.ends_with("-full.tar"))
        .context("no full backup for this project")?;
    let mut applied = Vec::new();
    let mut last: Option<Manifest> = None;
    for name in &all[start..] {
        let data = upstream.get(name).await?.context("bundle vanished")?;
        let mut ar = tar::Archive::new(std::io::Cursor::new(data.to_vec()));
        let mut manifest: Option<Manifest> = None;
        let mut files: Vec<(String, Vec<u8>)> = Vec::new();
        for ent in ar.entries()? {
            let mut ent = ent?;
            let path = ent.path()?.to_string_lossy().to_string();
            let mut buf = Vec::new();
            ent.read_to_end(&mut buf)?;
            if path == "manifest.json" {
                manifest = Some(serde_json::from_slice(&buf)?);
            } else if let Some(k) = path.strip_prefix("objects/") {
                files.push((k.to_string(), buf));
            }
        }
        let m = manifest.context("bundle without manifest")?;
        for (k, buf) in files {
            let (b, key) = k.split_once('/').context("bad object id")?;
            if !key.starts_with(&format!("projects/{project}/")) {
                bail!("bundle for {project} carries a foreign object {k}");
            }
            local.put(b, key, Bytes::from(buf)).await?;
        }
        for k in &m.deleted {
            if let Some((b, key)) = k.split_once('/') {
                local.delete(b, key).await?;
            }
        }
        applied.push(name.clone());
        last = Some(m);
    }
    let m = last.context("nothing applied")?;
    // drop local objects of this project that the final state does not have
    for (k, _) in project_objects(local, project).await? {
        if !m.objects.contains_key(&k) {
            if let Some((b, key)) = k.split_once('/') {
                local.delete(b, key).await?;
            }
        }
    }
    let state = BackupState {
        seq: m.seq,
        objects: m.objects.clone(),
    };
    let sp = state_path(local, project);
    if let Some(d) = sp.parent() {
        tokio::fs::create_dir_all(d).await?;
    }
    tokio::fs::write(&sp, serde_json::to_vec(&state)?).await?;
    Ok(
        serde_json::json!({"project": project, "applied": applied, "objects": m.objects.len(), "seq": m.seq}),
    )
}
