//! Releases and environments (spec §16).
//!
//! A release is the set of versions it needs, at every C4 level, joined to it by `supports`
//! edges (version → release, `{node}`), plus a snapshot of the code graph and trace edges taken
//! when its selection was applied, so gates and runs are reproducible.
//!
//! - **Selection** (§16.2): newest | stable | as_of | explicit, optionally filtered by tags or
//!   attributes of the node or its version, with per-node pins. Stored on the release.
//! - **States:** `open` while in DEV (reselect, or swap through a changeset); `cut` (immutable)
//!   once it first leaves DEV; `retired` once replaced in a single-slot environment.
//! - **Environments** come from the project's `environments` setting (lowest first). A release is
//!   in at most one environment at a time (`in_environment` edge with `validFrom`/`validTo`);
//!   an environment with `max_releases` holds at most that many. Both limits are enforced by
//!   sparse unique indexes (one env per release; one release per single-slot env) and in the
//!   move transaction.
//! - **Clone** into the release's own environment or a lower one only. A **patch release** is a
//!   clone at the same environment with some versions swapped; it must pass that environment's
//!   gate to enter, and the original is retired, unchanged.

use std::collections::{BTreeMap, BTreeSet};

use kodehub_core::model::now_iso;
use kodehub_core::{CodeEdge, CodeGraph, CodeSymbol};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::store::{edge_key, key_of};
use crate::{Result, Store, StoreError};

/// A release's version selection rule (spec §16.2).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SelectionRule {
    /// `stable` (default) | `newest` | `as_of` | `explicit`
    #[serde(default = "stable")]
    pub mode: String,
    /// For `as_of`: RFC 3339 timestamp.
    #[serde(default)]
    pub at: Option<String>,
    #[serde(default)]
    pub filter: Option<Filter>,
    /// node id → version key (overrides the mode for that node).
    #[serde(default)]
    pub pins: BTreeMap<String, String>,
    /// For `explicit`: node id → version key.
    #[serde(default)]
    pub explicit: BTreeMap<String, String>,
    /// Keep the build closure of every selected code node (default true).
    #[serde(default = "yes")]
    pub include_dependencies: bool,
}

impl Default for SelectionRule {
    fn default() -> Self {
        SelectionRule {
            mode: stable(),
            at: None,
            filter: None,
            pins: BTreeMap::new(),
            explicit: BTreeMap::new(),
            include_dependencies: true,
        }
    }
}

fn stable() -> String {
    "stable".into()
}
fn yes() -> bool {
    true
}

/// Structured filter on nodes (data, not code, so new kinds can be added later).
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct Filter {
    #[serde(default)]
    pub tags_any: Vec<String>,
    #[serde(default)]
    pub tags_all: Vec<String>,
    /// `path → value`, matched against the node document first, then the version's
    /// `attributes` (e.g. `{"attributes.channel": "beta"}` or `{"channel": "beta"}`).
    #[serde(default)]
    pub attributes: BTreeMap<String, Value>,
    /// C4 levels to keep (`context`, `container`, `component`, `code`); empty = all.
    #[serde(default)]
    pub levels: Vec<String>,
    /// Explicit node ids to keep; empty = all.
    #[serde(default)]
    pub nodes: Vec<String>,
}

fn get_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    for p in path.split('.') {
        cur = cur.get(p)?;
    }
    Some(cur)
}

impl Filter {
    pub fn matches(&self, node_id: &str, node: &Value, version: &Value) -> bool {
        if !self.nodes.is_empty() && !self.nodes.iter().any(|n| n == node_id) {
            return false;
        }
        if !self.levels.is_empty()
            && !self
                .levels
                .iter()
                .any(|l| l == crate::c4::level_of(node_id))
        {
            return false;
        }
        let tags: BTreeSet<&str> = node["tags"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if !self.tags_any.is_empty() && !self.tags_any.iter().any(|t| tags.contains(t.as_str())) {
            return false;
        }
        if !self.tags_all.iter().all(|t| tags.contains(t.as_str())) {
            return false;
        }
        self.attributes.iter().all(|(path, want)| {
            get_path(node, path)
                .or_else(|| get_path(version, path))
                .or_else(|| get_path(&version["attributes"], path))
                .or_else(|| get_path(&version["definition"]["attributes"], path))
                == Some(want)
        })
    }
}

/// One environment of a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvDef {
    pub name: String,
    pub order: usize,
    pub max_releases: Option<u64>,
    pub requires_human_approval: bool,
}

/// A selected (node, version) pair.
#[derive(Debug, Clone, PartialEq)]
pub struct Selected {
    pub node: String,
    pub coll: String,
    pub version: String,
}

const RELEASE_WRITE: &[&str] = &["releases", "supports", "in_environment", "cloned_from"];

impl Store {
    // ------------------------------------------------------------------ environments

    /// The project's environments, lowest first, synced from the `environments` setting.
    pub async fn env_defs(&self) -> Result<Vec<EnvDef>> {
        let setting = match self.project_id() {
            Some(p) => self.setting(&p, "environments").await?,
            None => Value::Null,
        };
        let list = setting.as_array().cloned().unwrap_or_else(|| {
            crate::settings::definition("environments")
                .map(|d| (d.default)())
                .and_then(|v| v.as_array().cloned())
                .unwrap_or_default()
        });
        let defs: Vec<EnvDef> = list
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                Some(EnvDef {
                    name: e["name"].as_str()?.to_string(),
                    order: i,
                    max_releases: e["max_releases"].as_u64(),
                    requires_human_approval: e["requires_human_approval"] == true,
                })
            })
            .collect();
        for d in &defs {
            let doc = json!({"_key": d.name, "name": d.name, "order": d.order,
                             "max_releases": d.max_releases,
                             "requires_human_approval": d.requires_human_approval,
                             "config": {"stubs_allowed": d.order == 0}});
            self.db.upsert("environments", &doc, None).await?;
        }
        Ok(defs)
    }

    async fn env_def(&self, name: &str) -> Result<EnvDef> {
        self.env_defs()
            .await?
            .into_iter()
            .find(|d| d.name == name)
            .ok_or_else(|| StoreError::NotFound(format!("environment {name}")))
    }

    /// The environment a release is in now, if any.
    pub async fn release_environment(&self, release: &str) -> Result<Option<String>> {
        Ok(self
            .aql(
                "FOR e IN @@in_environment FILTER e._from == CONCAT(@pfx, 'releases/', @r) AND e.validTo == null
                   RETURN PARSE_IDENTIFIER(e._to).key",
                json!({"r": key_of(release)}),
            )
            .await?
            .first()
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// Releases currently in an environment, most recently entered first.
    pub async fn releases_in(&self, env: &str) -> Result<Vec<Value>> {
        self.aql(
            "FOR e IN @@in_environment FILTER e._to == CONCAT(@pfx, 'environments/', @env) AND e.validTo == null
               SORT e.validFrom DESC
               LET r = DOCUMENT(e._from)
               RETURN {release: r._key, name: r.name, state: r.state, since: e.validFrom}",
            json!({"env": env}),
        )
        .await
    }

    // ------------------------------------------------------------------ selection

    /// Apply a selection rule to the graph.
    pub async fn select_versions(&self, rule: &SelectionRule) -> Result<Vec<Selected>> {
        let mode = rule.mode.as_str();
        if !matches!(mode, "stable" | "newest" | "as_of" | "explicit") {
            return Err(StoreError::Invalid(format!(
                "selection mode must be stable, newest, as_of or explicit (got `{mode}`)"
            )));
        }
        if mode == "as_of" && rule.at.is_none() {
            return Err(StoreError::Invalid("as_of selection needs `at`".into()));
        }
        let rows = self
            .aql(
                "FOR n IN UNION((FOR x IN @@symbols RETURN x), (FOR x IN @@contexts RETURN x),
                                (FOR x IN @@containers RETURN x), (FOR x IN @@components RETURN x))
                   LET edges = (FOR e IN @@has_version FILTER e._from == n._id
                                  AND e.state IN ['candidate', 'stable', 'retired'] RETURN e)
                   LET pick = @mode == 'stable' ? FIRST(FOR e IN edges FILTER e.state == 'stable' RETURN e)
                            : @mode == 'newest' ? FIRST(FOR e IN edges SORT e.seq DESC LIMIT 1 RETURN e)
                            : @mode == 'as_of' ? FIRST(FOR e IN edges FILTER e.validFrom != null
                                  AND e.validFrom <= @at AND (e.validTo == null OR e.validTo > @at) RETURN e)
                            : null
                   RETURN {node: n._id, doc: UNSET(n, '_rev'),
                           version: pick ? PARSE_IDENTIFIER(pick._to).key : null,
                           coll: pick ? PARSE_IDENTIFIER(pick._to).collection : null,
                           vdoc: pick ? DOCUMENT(pick._to) : null}",
                json!({"mode": mode, "at": rule.at}),
            )
            .await?;
        let logical = |c: &str| c.rsplit("__").next().unwrap_or(c).to_string();
        let default_coll = |node: &str| {
            if node.starts_with("symbols/") {
                "versions".to_string()
            } else {
                "c4_versions".to_string()
            }
        };
        let mut chosen: BTreeMap<String, Selected> = BTreeMap::new();
        let mut docs: BTreeMap<String, Value> = BTreeMap::new();
        for r in &rows {
            let node = r["node"].as_str().unwrap_or_default().to_string();
            docs.insert(node.clone(), r["doc"].clone());
            let explicit = rule.explicit.get(&node).or_else(|| rule.pins.get(&node));
            let (version, coll, vdoc) = match explicit {
                Some(v) => {
                    let coll = default_coll(&node);
                    let vdoc = self.db.get(&coll, v, None).await?;
                    (Some(v.clone()), coll, vdoc)
                }
                None if mode == "explicit" => (None, String::new(), Value::Null),
                None => (
                    r["version"].as_str().map(str::to_string),
                    r["coll"].as_str().map(logical).unwrap_or_default(),
                    r["vdoc"].clone(),
                ),
            };
            let Some(version) = version else { continue };
            if let Some(f) = &rule.filter {
                if !rule.pins.contains_key(&node) && !f.matches(&node, &r["doc"], &vdoc) {
                    continue;
                }
            }
            chosen.insert(
                node.clone(),
                Selected {
                    node,
                    coll,
                    version,
                },
            );
        }
        // keep the build closure of selected code (filters must not break builds)
        if rule.include_dependencies && rule.filter.is_some() {
            let g = self.load_code_graph().await?;
            let mut extra = Vec::new();
            for sel in chosen.values().filter(|s| s.node.starts_with("symbols/")) {
                let k = key_of(&sel.node);
                for e in g
                    .edges
                    .iter()
                    .filter(|e| e.from == k && e.kind.is_build_dependency())
                {
                    let dep = format!("symbols/{}", e.to);
                    if !chosen.contains_key(&dep) {
                        if let Some(s) = g.symbols.get(&e.to) {
                            extra.push(Selected {
                                node: dep,
                                coll: "versions".into(),
                                version: s.version.clone(),
                            });
                        }
                    }
                }
            }
            for x in extra {
                chosen.entry(x.node.clone()).or_insert(x);
            }
        }
        Ok(chosen.into_values().collect())
    }

    /// Build the code graph a selection describes (code edges among selected symbols).
    async fn selection_graph(&self, sel: &[Selected]) -> Result<CodeGraph> {
        let live = self.load_code_graph().await?;
        let mut g = CodeGraph::default();
        let syms = self
            .aql(
                "FOR s IN @@symbols RETURN {id: s._key, display_name: s.display_name, kind: s.kind,
                                             language: s.language, visibility: s.visibility, tags: s.tags || []}",
                json!({}),
            )
            .await?;
        let by_key: BTreeMap<String, Value> = syms
            .into_iter()
            .filter_map(|s| Some((s["id"].as_str()?.to_string(), s)))
            .collect();
        for s in sel.iter().filter(|s| s.node.starts_with("symbols/")) {
            let k = key_of(&s.node).to_string();
            let Some(mut doc) = by_key.get(&k).cloned() else {
                continue;
            };
            doc["version"] = json!(s.version);
            let cs: CodeSymbol = serde_json::from_value(doc)
                .map_err(|e| StoreError::Invalid(format!("bad symbol: {e}")))?;
            g.symbols.insert(k, cs);
        }
        g.edges = live
            .edges
            .into_iter()
            .filter(|e| g.symbols.contains_key(&e.from) && g.symbols.contains_key(&e.to))
            .collect();
        let mut needed: Vec<String> = g.symbols.values().map(|s| s.version.clone()).collect();
        needed.extend(g.edges.iter().filter_map(|e| e.pinned_version.clone()));
        needed.sort();
        needed.dedup();
        g.versions = self.load_versions(&needed).await?;
        // edges for versions that differ from the working graph come from their own code
        let names = g.clone();
        for s in names.symbols.values() {
            if live.symbols.get(&s.id).map(|l| &l.version) != Some(&s.version) {
                if let Ok(v) = names.version(&s.version) {
                    let text = format!("{}\n{}", v.signature, v.body);
                    g.edges.retain(|e| {
                        !(e.from == s.id
                            && matches!(
                                e.kind,
                                kodehub_core::model::CodeEdgeKind::Calls
                                    | kodehub_core::model::CodeEdgeKind::DependsOn
                            ))
                    });
                    for (to, kind) in
                        kodehub_core::infer::references(&names, &s.id, s.language, &text)
                    {
                        g.edges.push(CodeEdge {
                            from: s.id.clone(),
                            to,
                            kind,
                            pinned_version: None,
                        });
                    }
                }
            }
        }
        g.edges.sort();
        g.edges.dedup();
        Ok(g)
    }

    /// Manifest, graph snapshot, trace snapshot and closures for a selection.
    async fn manifest_for(&self, sel: &[Selected]) -> Result<Value> {
        let g = self.selection_graph(sel).await?;
        let manifest: Vec<Value> = g
            .symbols
            .values()
            .map(|s| json!({"symbol": s.id, "display_name": s.display_name, "kind": s.kind, "language": s.language, "version": s.version}))
            .collect();
        let c4: Vec<Value> = sel
            .iter()
            .filter(|s| !s.node.starts_with("symbols/"))
            .map(|s| json!({"node": s.node, "version": s.version}))
            .collect();
        let closures = g.all_closure_hashes()?;
        let trace = self
            .aql(
                "RETURN {
                   satisfies: (FOR e IN @@satisfies RETURN [e._from, e._to]),
                   verifies: (FOR e IN @@verifies RETURN [e._from, e._to]),
                   contains: (FOR e IN @@contains RETURN [e._from, e._to]),
                   includes: (FOR e IN @@includes RETURN [e._from, e._to]),
                   requirements: (FOR r IN @@requirements RETURN {key: r._key, revision: r.revision, at_risk: r.at_risk == true})
                 }",
                json!({}),
            )
            .await?
            .into_iter()
            .next()
            .unwrap_or(json!({}));
        let hash = kodehub_core::canonical::hash_value(&json!({
            "code": manifest.iter().map(|m| [m["symbol"].clone(), m["version"].clone()]).collect::<Vec<_>>(),
            "c4": c4,
        }));
        Ok(json!({
            "manifest": manifest, "c4_manifest": c4,
            "graph": {"symbols": g.symbols.values().collect::<Vec<_>>(), "edges": g.edges},
            "root_closures": closures, "trace": trace, "manifest_hash": hash,
        }))
    }

    async fn write_supports(&self, release: &str, sel: &[Selected], trx: &str) -> Result<()> {
        let rid = format!("releases/{release}");
        self.db
            .aql(
                "FOR e IN @@supports FILTER e._to == @r REMOVE e IN @@supports",
                json!({"r": rid}),
                Some(trx),
            )
            .await?;
        for s in sel {
            let vid = format!("{}/{}", s.coll, s.version);
            self.db
                .insert(
                    "supports",
                    &json!({"_key": edge_key("supports", &s.node, &rid), "_from": vid, "_to": rid, "node": s.node}),
                    Some(trx),
                )
                .await?;
        }
        Ok(())
    }

    async fn next_release_no(&self) -> Result<i64> {
        Ok(self
            .aql(
                "RETURN MAX(FOR r IN @@releases RETURN r.version_no) || 0",
                json!({}),
            )
            .await?
            .first()
            .and_then(Value::as_i64)
            .unwrap_or(0)
            + 1)
    }

    async fn run_release_trx<T>(
        &self,
        f: impl std::future::Future<Output = Result<T>>,
        trx: &str,
    ) -> Result<T> {
        match f.await {
            Ok(v) => {
                self.db.commit(trx).await?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.db.abort(trx).await;
                Err(e)
            }
        }
    }

    // ------------------------------------------------------------------ create / reselect

    /// Create a release from a selection rule. It is open and enters the lowest environment.
    pub async fn create_release(
        &self,
        name: Option<&str>,
        notes: &str,
        rule: &SelectionRule,
        by: &str,
        build_targets: &[String],
    ) -> Result<Value> {
        let _write = self.write_lock.lock().await;
        let envs = self.env_defs().await?;
        let dev = envs
            .first()
            .ok_or_else(|| StoreError::Invalid("the project has no environments".into()))?
            .name
            .clone();
        let sel = self.select_versions(rule).await?;
        let snap = self.manifest_for(&sel).await?;
        let no = self.next_release_no().await?;
        let key = format!("r{no}");
        let mut doc = snap;
        doc["_key"] = json!(key);
        doc["version_no"] = json!(no);
        doc["name"] = json!(name
            .map(str::to_string)
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| key.clone()));
        doc["notes"] = json!(notes);
        doc["state"] = json!("open");
        doc["selection"] = json!(rule);
        doc["build_targets"] = json!(build_targets);
        doc["created_at"] = json!(now_iso());
        doc["created_by"] = json!(by);
        let trx = self.db.begin(RELEASE_WRITE, &[]).await?;
        let out = self
            .run_release_trx(
                async {
                    let d = self.db.insert("releases", &doc, Some(&trx)).await?;
                    self.write_supports(&key, &sel, &trx).await?;
                    self.enter_in(&key, &dev, by, json!({"created": true}), "created", &trx)
                        .await?;
                    Ok(d)
                },
                &trx,
            )
            .await?;
        Ok(out)
    }

    /// v0.05 entry point: a release of the stable graph.
    pub async fn cut_release(
        &self,
        name: Option<&str>,
        notes: &str,
        by: &str,
        build_targets: &[String],
    ) -> Result<Value> {
        self.create_release(name, notes, &SelectionRule::default(), by, build_targets)
            .await
    }

    fn ensure_open(r: &Value) -> Result<()> {
        if r["state"] != "open" {
            return Err(StoreError::Immutable(format!(
                "release {} is {}; only open (DEV) releases change",
                r["_key"].as_str().unwrap_or("?"),
                r["state"].as_str().unwrap_or("cut")
            )));
        }
        Ok(())
    }

    /// Re-apply an open release's selection rule (or a new one).
    pub async fn reselect_release(
        &self,
        key: &str,
        rule: Option<&SelectionRule>,
        by: &str,
    ) -> Result<Value> {
        let _write = self.write_lock.lock().await;
        let r = self.get_release(key).await?;
        Self::ensure_open(&r)?;
        let rule: SelectionRule = match rule {
            Some(x) => x.clone(),
            None => serde_json::from_value(r["selection"].clone()).unwrap_or_default(),
        };
        let sel = self.select_versions(&rule).await?;
        self.replace_contents(key_of(key), &sel, Some(&rule), by)
            .await
    }

    async fn replace_contents(
        &self,
        key: &str,
        sel: &[Selected],
        rule: Option<&SelectionRule>,
        by: &str,
    ) -> Result<Value> {
        let mut patch = self.manifest_for(sel).await?;
        if let Some(rule) = rule {
            patch["selection"] = json!(rule);
        }
        patch["updated_at"] = json!(now_iso());
        patch["updated_by"] = json!(by);
        let trx = self.db.begin(RELEASE_WRITE, &[]).await?;
        self.run_release_trx(
            async {
                let cur = self.db.get("releases", key, Some(&trx)).await?;
                Self::ensure_open(&cur)?;
                self.write_supports(key, sel, &trx).await?;
                self.db.patch("releases", key, &patch, Some(&trx)).await
            },
            &trx,
        )
        .await
    }

    /// The (node, version) pairs a release supports.
    pub async fn release_selection(&self, key: &str) -> Result<Vec<Selected>> {
        let rows = self
            .aql(
                "FOR e IN @@supports FILTER e._to == CONCAT(@pfx, 'releases/', @r)
                   RETURN {node: e.node, coll: PARSE_IDENTIFIER(e._from).collection, version: PARSE_IDENTIFIER(e._from).key}",
                json!({"r": key_of(key)}),
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| Selected {
                node: r["node"].as_str().unwrap_or_default().into(),
                coll: r["coll"]
                    .as_str()
                    .map(|c| c.rsplit("__").next().unwrap_or(c).to_string())
                    .unwrap_or_default(),
                version: r["version"].as_str().unwrap_or_default().into(),
            })
            .collect())
    }

    /// Swap versions in an open release (a changeset naming the release, spec §5.8).
    pub async fn swap_release_versions(
        &self,
        key: &str,
        swaps: &[Selected],
        by: &str,
    ) -> Result<Value> {
        let r = self.get_release(key).await?;
        Self::ensure_open(&r)?;
        let mut sel = self.release_selection(key).await?;
        apply_swaps(&mut sel, swaps);
        self.replace_contents(key_of(key), &sel, None, by).await
    }

    // ------------------------------------------------------------------ clone / patch

    /// Clone a release into `env` (its own environment or a lower one), with optional version
    /// swaps. A clone into the lowest environment is open and enters it at once; a clone into a
    /// higher environment is a patch release: cut from birth, **staged** until it passes that
    /// environment's gate (`enter_environment`).
    pub async fn clone_release(
        &self,
        src: &str,
        env: &str,
        swaps: &[Selected],
        name: Option<&str>,
        by: &str,
    ) -> Result<Value> {
        let _write = self.write_lock.lock().await;
        let src = key_of(src).to_string();
        let source = self.get_release(&src).await?;
        let envs = self.env_defs().await?;
        let target = envs
            .iter()
            .find(|e| e.name == env)
            .ok_or_else(|| StoreError::NotFound(format!("environment {env}")))?;
        let here = self.release_environment(&src).await?;
        let src_order = here
            .as_deref()
            .and_then(|h| envs.iter().find(|e| e.name == h))
            .map(|e| e.order)
            .or_else(|| {
                // a retired release keeps the order of the environment it last held
                source["last_environment"]
                    .as_str()
                    .and_then(|h| envs.iter().find(|e| e.name == h))
                    .map(|e| e.order)
            })
            .unwrap_or(0);
        if target.order > src_order {
            return Err(StoreError::Invalid(format!(
                "a release is cloned into its own environment or a lower one, never higher \
                 ({src} is at {}, asked for {env})",
                envs[src_order].name
            )));
        }
        let mut sel = self.release_selection(&src).await?;
        apply_swaps(&mut sel, swaps);
        let snap = self.manifest_for(&sel).await?;
        let lowest = target.order == 0;
        let key = if lowest {
            format!("r{}", self.next_release_no().await?)
        } else {
            let n = self
                .aql(
                    "RETURN LENGTH(FOR r IN @@releases FILTER r.patch_of == @s RETURN 1)",
                    json!({"s": src}),
                )
                .await?
                .first()
                .and_then(Value::as_u64)
                .unwrap_or(0);
            format!("{src}.{}", n + 1)
        };
        let mut doc = snap;
        doc["_key"] = json!(key);
        doc["version_no"] = if lowest {
            json!(key.trim_start_matches('r').parse::<i64>().unwrap_or(0))
        } else {
            source["version_no"].clone()
        };
        doc["name"] = json!(name.map(str::to_string).unwrap_or_else(|| key.clone()));
        doc["notes"] = json!(format!("clone of {src}"));
        doc["state"] = json!(if lowest { "open" } else { "staged" });
        doc["selection"] = source["selection"].clone();
        doc["build_targets"] = source["build_targets"].clone();
        doc["cloned_from"] = json!(src);
        if !lowest {
            doc["patch_of"] = json!(src);
            doc["target_environment"] = json!(env);
        }
        doc["swaps"] = json!(swaps
            .iter()
            .map(|s| json!({"node": s.node, "version": s.version}))
            .collect::<Vec<_>>());
        doc["created_at"] = json!(now_iso());
        doc["created_by"] = json!(by);
        let trx = self.db.begin(RELEASE_WRITE, &[]).await?;
        self.run_release_trx(
            async {
                let d = self.db.insert("releases", &doc, Some(&trx)).await?;
                self.write_supports(&key, &sel, &trx).await?;
                self.db
                    .insert(
                        "cloned_from",
                        &json!({"_from": format!("releases/{key}"), "_to": format!("releases/{src}"), "at": now_iso()}),
                        Some(&trx),
                    )
                    .await?;
                if lowest {
                    self.enter_in(&key, env, by, json!({"cloned_from": src}), "cloned", &trx)
                        .await?;
                }
                Ok(d)
            },
            &trx,
        )
        .await
    }

    // ------------------------------------------------------------------ moving between environments

    /// Put a release into `env` (gates are the caller's job). Closes its current environment
    /// edge, retires occupants of a full environment, opens the new edge — one transaction.
    pub async fn enter_environment(
        &self,
        release: &str,
        env: &str,
        by: &str,
        gate: Value,
        reason: &str,
    ) -> Result<Value> {
        let _write = self.write_lock.lock().await;
        let trx = self.db.begin(RELEASE_WRITE, &[]).await?;
        let rel = key_of(release).to_string();
        self.run_release_trx(
            async { self.enter_in(&rel, env, by, gate, reason, &trx).await },
            &trx,
        )
        .await
    }

    async fn enter_in(
        &self,
        rel: &str,
        env: &str,
        by: &str,
        gate: Value,
        reason: &str,
        trx: &str,
    ) -> Result<Value> {
        let def = self.env_def(env).await?;
        let lowest = def.order == 0;
        let rel_id = format!("releases/{rel}");
        let env_id = format!("environments/{env}");
        let now = now_iso();
        let r = self.db.get("releases", rel, Some(trx)).await?;
        if r["state"] == "retired" && reason != "rollback" {
            return Err(StoreError::Invalid(format!(
                "release {rel} is retired; clone it or roll back to it"
            )));
        }
        // close the release's current environment edge (one environment at a time)
        let cur = self
            .db
            .aql(
                "FOR e IN @@in_environment FILTER e._from == @r AND e.validTo == null RETURN e",
                json!({"r": rel_id}),
                Some(trx),
            )
            .await?;
        for c in &cur {
            if c["_to"] == json!(env_id) {
                return Err(StoreError::Conflict(format!("{rel} is already in {env}")));
            }
            self.db
                .patch(
                    "in_environment",
                    c["_key"].as_str().unwrap_or_default(),
                    &json!({"validTo": now, "current_release": null, "slot": null, "left_for": env}),
                    Some(trx),
                )
                .await?;
        }
        // make room in a full environment: the longest-standing occupants are retired
        let occupants = self
            .db
            .aql(
                "FOR e IN @@in_environment FILTER e._to == @env AND e.validTo == null SORT e.validFrom RETURN e",
                json!({"env": env_id}),
                Some(trx),
            )
            .await?;
        let mut retired = Vec::new();
        if let Some(max) = def.max_releases {
            let excess = (occupants.len() as i64 + 1 - max as i64).max(0) as usize;
            for o in occupants.iter().take(excess) {
                self.db
                    .patch(
                        "in_environment",
                        o["_key"].as_str().unwrap_or_default(),
                        &json!({"validTo": now, "current_release": null, "slot": null,
                                "replaced_by": rel}),
                        Some(trx),
                    )
                    .await?;
                let ok = key_of(o["_from"].as_str().unwrap_or_default()).to_string();
                self.db
                    .patch(
                        "releases",
                        &ok,
                        &json!({"state": "retired", "retired_at": now, "retired_by": rel, "last_environment": env}),
                        Some(trx),
                    )
                    .await?;
                retired.push(ok);
            }
        }
        // a patch release replaces the release it patches, wherever that one is
        if let Some(src) = r["patch_of"].as_str() {
            if r["target_environment"] == json!(env) {
                let src_id = format!("releases/{src}");
                let src_edges = self
                    .db
                    .aql(
                        "FOR e IN @@in_environment FILTER e._from == @r AND e.validTo == null RETURN e",
                        json!({"r": src_id}),
                        Some(trx),
                    )
                    .await?;
                for e in src_edges {
                    self.db
                        .patch(
                            "in_environment",
                            e["_key"].as_str().unwrap_or_default(),
                            &json!({"validTo": now, "current_release": null, "slot": null, "replaced_by": rel}),
                            Some(trx),
                        )
                        .await?;
                    self.db
                        .patch(
                            "releases",
                            src,
                            &json!({"state": "retired", "retired_at": now, "retired_by": rel, "last_environment": env}),
                            Some(trx),
                        )
                        .await?;
                    retired.push(src.to_string());
                }
            }
        }
        let single = def.max_releases == Some(1);
        self.db
            .insert(
                "in_environment",
                &json!({"_from": rel_id, "_to": env_id, "validFrom": now, "validTo": null,
                        "current_release": rel_id, "slot": if single { json!(env_id) } else { Value::Null },
                        "by": by, "gate": gate, "reason": reason}),
                Some(trx),
            )
            .await
            .map_err(|e| {
                if e.is_unique_violation() {
                    StoreError::Invariant(format!(
                        "{rel} is already in an environment, or {env} is full"
                    ))
                } else {
                    e
                }
            })?;
        let mut patch = json!({"environment": env, "last_environment": env});
        if !lowest && r["state"] != "cut" {
            patch["state"] = json!("cut");
            patch["cut_at"] = json!(now);
        }
        if reason == "rollback" {
            patch["state"] = json!("cut");
        }
        self.db.patch("releases", rel, &patch, Some(trx)).await?;
        Ok(json!({"environment": env, "release": rel, "entered_at": now, "retired": retired}))
    }

    /// The release to roll back to in `env`: the most recently retired one that left it.
    pub async fn rollback_target(&self, env: &str) -> Result<Option<String>> {
        Ok(self
            .aql(
                "LET envid = CONCAT(@pfx, 'environments/', @env)
                 LET current = (FOR e IN @@in_environment FILTER e._to == envid AND e.validTo == null RETURN e._from)
                 FOR e IN @@in_environment FILTER e._to == envid AND e.validTo != null AND e.replaced_by != null
                   AND e._from NOT IN current AND e.rolled_back != true
                   LET r = DOCUMENT(e._from)
                   FILTER r.state == 'retired'
                   SORT e.validTo DESC LIMIT 1 RETURN r._key",
                json!({"env": env}),
            )
            .await?
            .first()
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// Mark the replaced edge so the same release is not chosen again by a second rollback.
    pub async fn mark_rolled_back_from(&self, env: &str, release: &str) -> Result<()> {
        self.aql(
            "FOR e IN @@in_environment FILTER e._to == CONCAT(@pfx, 'environments/', @env)
               AND e._from == CONCAT(@pfx, 'releases/', @r) AND e.validTo != null
               UPDATE e WITH {rolled_back: true} IN @@in_environment",
            json!({"env": env, "r": release}),
        )
        .await
        .map(|_| ())
    }

    // ------------------------------------------------------------------ reads

    pub async fn get_release(&self, key: &str) -> Result<Value> {
        self.db.get("releases", key_of(key), None).await
    }

    pub async fn list_releases(&self) -> Result<Vec<Value>> {
        self.aql(
            "FOR r IN @@releases SORT r.created_at DESC
             LET env = FIRST(FOR e IN @@in_environment FILTER e._from == r._id AND e.validTo == null
                             RETURN PARSE_IDENTIFIER(e._to).key)
             LET history = (FOR e IN @@in_environment FILTER e._from == r._id SORT e.validFrom
                            RETURN {environment: PARSE_IDENTIFIER(e._to).key, from: e.validFrom, until: e.validTo, reason: e.reason})
             RETURN MERGE(UNSET(r, 'graph', 'root_closures', 'trace'),
                          {symbols: LENGTH(r.manifest), environment: env, running_in: env ? [env] : [],
                           deployments: history})",
            json!({}),
        )
        .await
    }

    /// Diff = set difference of supported versions per node.
    pub async fn diff_releases(&self, a: &str, b: &str) -> Result<Value> {
        let ra = self.get_release(a).await?;
        let rb = self.get_release(b).await?;
        let index = |r: &Value| -> BTreeMap<String, Value> {
            r["manifest"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|m| Some((m["symbol"].as_str()?.to_string(), m)))
                .collect()
        };
        let (ia, ib) = (index(&ra), index(&rb));
        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut changed = Vec::new();
        for (k, m) in &ib {
            match ia.get(k) {
                None => added.push(m.clone()),
                Some(old) if old["version"] != m["version"] => changed.push(json!({
                    "symbol": k, "display_name": m["display_name"],
                    "from": old["version"], "to": m["version"], "reasons": ["version"],
                })),
                _ => {}
            }
        }
        for (k, m) in &ia {
            if !ib.contains_key(k) {
                removed.push(m.clone());
            }
        }
        let c4 = |r: &Value| -> BTreeMap<String, Value> {
            r["c4_manifest"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|m| Some((m["node"].as_str()?.to_string(), m["version"].clone())))
                .collect()
        };
        let (ca, cb) = (c4(&ra), c4(&rb));
        let c4_changed: Vec<Value> = cb
            .iter()
            .filter(|(k, v)| ca.get(*k).is_some_and(|o| o != *v))
            .map(|(k, v)| json!({"node": k, "from": ca[k], "to": v}))
            .collect();
        Ok(
            json!({"from": key_of(a), "to": key_of(b), "added": added, "removed": removed,
                  "changed": changed, "c4_changed": c4_changed}),
        )
    }

    /// Reconstruct a release's code graph.
    pub async fn release_graph(&self, key: &str) -> Result<CodeGraph> {
        let r = self.get_release(key).await?;
        let mut g = CodeGraph::default();
        let syms: Vec<CodeSymbol> = serde_json::from_value(r["graph"]["symbols"].clone())
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        let edges: Vec<CodeEdge> = serde_json::from_value(r["graph"]["edges"].clone())
            .map_err(|e| StoreError::Invalid(e.to_string()))?;
        for s in syms {
            g.symbols.insert(s.id.clone(), s);
        }
        g.edges = edges;
        let mut needed: Vec<String> = g.symbols.values().map(|s| s.version.clone()).collect();
        needed.extend(g.edges.iter().filter_map(|e| e.pinned_version.clone()));
        needed.sort();
        needed.dedup();
        g.versions = self.load_versions(&needed).await?;
        Ok(g)
    }

    /// Coverage and orphans evaluated on a release's own snapshot (code graph + trace edges +
    /// test results at the release's closure hashes), so gates are reproducible per release.
    pub async fn release_trace_report(&self, key: &str) -> Result<Value> {
        use kodehub_core::model::{CodeEdgeKind, Kind};
        let r = self.get_release(key).await?;
        let g = self.release_graph(key).await?;
        let pairs = |f: &str| -> Vec<(String, String)> {
            r["trace"][f]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string())))
                .collect()
        };
        let (sat, ver, con) = (pairs("satisfies"), pairs("verifies"), pairs("contains"));
        let sym = |id: &str| id.strip_prefix("symbols/").map(String::from);
        let mut adj: BTreeMap<String, Vec<(String, bool)>> = BTreeMap::new();
        for (f, t) in &con {
            adj.entry(f.clone()).or_default().push((t.clone(), false));
        }
        for e in &g.edges {
            adj.entry(format!("symbols/{}", e.from)).or_default().push((
                format!("symbols/{}", e.to),
                e.kind == CodeEdgeKind::TestedBy,
            ));
        }
        let reach = |starts: &[String], follow_tests: bool| -> BTreeSet<String> {
            let mut seen: BTreeSet<String> = starts.iter().cloned().collect();
            let mut q: Vec<String> = starts.to_vec();
            while let Some(n) = q.pop() {
                for (t, tb) in adj.get(&n).cloned().unwrap_or_default() {
                    if (!tb || follow_tests) && seen.insert(t.clone()) {
                        q.push(t);
                    }
                }
            }
            seen
        };
        let mut status: BTreeMap<String, String> = BTreeMap::new();
        for s in g.symbols.values().filter(|s| s.kind == Kind::Test) {
            let closure = g.closure_hash(&s.id)?;
            let rows = self
                .aql(
                    "FOR r IN @@test_runs FILTER r.test_version == @v AND r.closure_hash == @c
                       AND r.formal != false SORT r.ran_at DESC LIMIT 1 RETURN r.status",
                    json!({"v": s.version, "c": closure}),
                )
                .await?;
            status.insert(
                s.id.clone(),
                rows.first()
                    .and_then(Value::as_str)
                    .unwrap_or("not_run")
                    .to_string(),
            );
        }
        let mut failing = Vec::new();
        let mut unverified = Vec::new();
        for req in r["trace"]["requirements"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            let rid = format!("requirements/{}", req["key"].as_str().unwrap_or(""));
            let starts: Vec<String> = sat
                .iter()
                .filter(|(_, t)| *t == rid)
                .map(|(f, _)| f.clone())
                .collect();
            let beneath = reach(&starts, false);
            let mut tests: BTreeSet<String> = ver
                .iter()
                .filter(|(_, t)| *t == rid)
                .filter_map(|(f, _)| sym(f))
                .collect();
            for n in &beneath {
                for (t, tb) in adj.get(n).cloned().unwrap_or_default() {
                    if tb {
                        if let Some(k) = sym(&t) {
                            tests.insert(k);
                        }
                    }
                }
            }
            let st: Vec<&str> = tests
                .iter()
                .filter(|t| g.symbols.contains_key(*t))
                .map(|t| status.get(t).map(String::as_str).unwrap_or("not_run"))
                .collect();
            if st.contains(&"fail") {
                failing.push(req["key"].clone());
            } else if st.is_empty() || st.iter().any(|s| *s != "pass") {
                unverified.push(req["key"].clone());
            }
        }
        let mut starts: Vec<String> = sat.iter().map(|(f, _)| f.clone()).collect();
        starts.extend(ver.iter().map(|(f, _)| f.clone()));
        let reached = reach(&starts, true);
        let orphans: Vec<String> = g
            .symbols
            .keys()
            .filter(|k| !reached.contains(&format!("symbols/{k}")))
            .cloned()
            .collect();
        Ok(
            json!({"failing_requirements": failing, "unverified_requirements": unverified, "orphans": orphans}),
        )
    }

    /// The most recently entered release in an environment.
    pub async fn running_release(&self, env: &str) -> Result<Option<Value>> {
        let Some(top) = self.releases_in(env).await?.into_iter().next() else {
            return Ok(None);
        };
        let mut r = self
            .get_release(top["release"].as_str().unwrap_or_default())
            .await?;
        if let Some(o) = r.as_object_mut() {
            o.remove("graph");
            o.remove("root_closures");
        }
        r["since"] = top["since"].clone();
        Ok(Some(r))
    }

    pub async fn release_graph_for_env(&self, env: &str) -> Result<Option<(String, CodeGraph)>> {
        match self.running_release(env).await? {
            Some(r) => {
                let k = r["_key"].as_str().unwrap_or_default().to_string();
                Ok(Some((k.clone(), self.release_graph(&k).await?)))
            }
            None => Ok(None),
        }
    }

    pub async fn environments(&self) -> Result<Vec<Value>> {
        self.env_defs().await?;
        self.aql(
            "FOR env IN @@environments SORT env.order
             LET here = (FOR e IN @@in_environment FILTER e._to == env._id AND e.validTo == null
                           SORT e.validFrom DESC
                           RETURN {release: PARSE_IDENTIFIER(e._from).key, name: DOCUMENT(e._from).name,
                                   state: DOCUMENT(e._from).state, since: e.validFrom})
             LET prev = (FOR e IN @@in_environment FILTER e._to == env._id AND e.validTo != null
                           SORT e.validTo DESC LIMIT 5
                           RETURN {release: PARSE_IDENTIFIER(e._from).key, from: e.validFrom, until: e.validTo})
             RETURN MERGE(env, {currently_running: FIRST(here), releases: here, history: prev})",
            json!({}),
        )
        .await
    }
}

/// Replace (or add) the version of each swapped node.
pub fn apply_swaps(sel: &mut Vec<Selected>, swaps: &[Selected]) {
    for s in swaps {
        match sel.iter_mut().find(|x| x.node == s.node) {
            Some(x) => *x = s.clone(),
            None => sel.push(s.clone()),
        }
    }
}

/// Build a swap for a code node from its version key.
pub fn code_swap(symbol: &str, version: &str) -> Selected {
    Selected {
        node: format!("symbols/{}", key_of(symbol)),
        coll: "versions".into(),
        version: version.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters() {
        let f = Filter {
            tags_any: vec!["payments".into()],
            attributes: [("channel".to_string(), json!("beta"))].into(),
            ..Default::default()
        };
        let node = json!({"tags": ["payments"], "channel": "beta"});
        assert!(f.matches("symbols/x", &node, &json!({})));
        assert!(!f.matches(
            "symbols/x",
            &json!({"tags": ["auth"], "channel": "beta"}),
            &json!({})
        ));
        // attribute found on the version
        let v = json!({"attributes": {"channel": "beta"}});
        assert!(f.matches("symbols/x", &json!({"tags": ["payments"]}), &v));
        let lv = Filter {
            levels: vec!["component".into()],
            ..Default::default()
        };
        assert!(lv.matches("components/a", &json!({}), &json!({})));
        assert!(!lv.matches("symbols/a", &json!({}), &json!({})));
    }

    #[test]
    fn swaps() {
        let mut sel = vec![code_swap("a", "v1"), code_swap("b", "v1")];
        apply_swaps(&mut sel, &[code_swap("b", "v2"), code_swap("c", "v1")]);
        assert_eq!(sel.len(), 3);
        assert_eq!(sel[1].version, "v2");
    }

    #[test]
    fn rule_defaults() {
        let r: SelectionRule = serde_json::from_value(json!({})).unwrap();
        assert_eq!(r, SelectionRule::default());
        assert_eq!(r.mode, "stable");
        assert!(r.include_dependencies);
    }
}
