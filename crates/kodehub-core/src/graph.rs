//! In-memory code graph: the subset of the Kodehub graph needed to compute closures, closure
//! hashes and render order. Loaded from the store (optionally with a changeset overlay applied).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::model::{CodeEdgeKind, Kind, Language, VersionContent, Visibility};

#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    #[error("unknown symbol `{0}`")]
    UnknownSymbol(String),
    #[error("version `{0}` not loaded")]
    MissingVersion(String),
    #[error("symbol `{symbol}` is needed at two versions ({a} and {b}) in one closure")]
    PinConflict {
        symbol: String,
        a: String,
        b: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CodeSymbol {
    pub id: String,
    pub display_name: String,
    pub kind: Kind,
    pub language: Language,
    #[serde(default)]
    pub visibility: Visibility,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Hash of the current (or overlaid) version.
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CodeEdge {
    pub from: String,
    pub to: String,
    pub kind: CodeEdgeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CodeGraph {
    pub symbols: BTreeMap<String, CodeSymbol>,
    pub versions: BTreeMap<String, VersionContent>,
    pub edges: Vec<CodeEdge>,
}

/// One resolved member of a closure: a symbol at a specific version.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Resolved {
    pub symbol: String,
    pub version: String,
}

impl CodeGraph {
    pub fn symbol(&self, id: &str) -> Result<&CodeSymbol, GraphError> {
        self.symbols
            .get(id)
            .ok_or_else(|| GraphError::UnknownSymbol(id.to_string()))
    }

    pub fn version(&self, hash: &str) -> Result<&VersionContent, GraphError> {
        self.versions
            .get(hash)
            .ok_or_else(|| GraphError::MissingVersion(hash.to_string()))
    }

    pub fn find_by_name(&self, language: Language, name: &str) -> Option<&CodeSymbol> {
        self.symbols
            .values()
            .find(|s| s.language == language && s.display_name == name)
    }

    pub fn out_edges<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a CodeEdge> + 'a {
        self.edges.iter().filter(move |e| e.from == id)
    }

    pub fn in_edges<'a>(&'a self, id: &'a str) -> impl Iterator<Item = &'a CodeEdge> + 'a {
        self.edges.iter().filter(move |e| e.to == id)
    }

    /// Build closure of `roots`: every symbol reachable over build-dependency edges, each resolved
    /// to its current version or the version pinned by the edge that reached it. When
    /// `include_tests` is set, `tested_with` edges are followed too (test builds).
    pub fn closure(
        &self,
        roots: &[String],
        include_tests: bool,
    ) -> Result<BTreeMap<String, String>, GraphError> {
        let mut resolved: BTreeMap<String, String> = BTreeMap::new();
        let mut queue: VecDeque<(String, Option<String>)> = VecDeque::new();
        for r in roots {
            self.symbol(r)?;
            queue.push_back((r.clone(), None));
        }
        while let Some((id, pin)) = queue.pop_front() {
            let sym = self.symbol(&id)?;
            let version = pin.unwrap_or_else(|| sym.version.clone());
            if let Some(existing) = resolved.get(&id) {
                if *existing != version {
                    return Err(GraphError::PinConflict {
                        symbol: id,
                        a: existing.clone(),
                        b: version,
                    });
                }
                continue;
            }
            resolved.insert(id.clone(), version);
            for e in self.out_edges(&id) {
                let follow = e.kind.is_build_dependency()
                    || (include_tests && e.kind == CodeEdgeKind::TestedWith);
                if follow {
                    queue.push_back((e.to.clone(), e.pinned_version.clone()));
                }
            }
        }
        Ok(resolved)
    }

    /// Closure hash of one symbol: BLAKE3 over the root id plus every closure member as
    /// `id:display_name:content_hash`, sorted by id. Display names are included because they
    /// appear in rendered code (see docs/decisions.md); content hashes never include them.
    pub fn closure_hash(&self, root: &str) -> Result<String, GraphError> {
        let members = self.closure(&[root.to_string()], false)?;
        Ok(self.hash_members(&format!("root:{root}"), &members))
    }

    /// Closure hash of a set of roots (used for multi-root builds and render manifests).
    pub fn closure_hash_of(
        &self,
        roots: &[String],
        include_tests: bool,
    ) -> Result<String, GraphError> {
        if roots.len() == 1 && !include_tests {
            return self.closure_hash(&roots[0]);
        }
        let members = self.closure(roots, include_tests)?;
        let mut sorted_roots = roots.to_vec();
        sorted_roots.sort();
        sorted_roots.dedup();
        let tag = format!("roots:{}:tests={include_tests}", sorted_roots.join(","));
        Ok(self.hash_members(&tag, &members))
    }

    fn hash_members(&self, tag: &str, members: &BTreeMap<String, String>) -> String {
        let mut h = blake3::Hasher::new();
        h.update(b"kodehub-closure-v1\n");
        h.update(tag.as_bytes());
        h.update(b"\n");
        for (id, version) in members {
            let (name, vis) = self
                .symbols
                .get(id)
                .map(|s| (s.display_name.as_str(), s.visibility))
                .unwrap_or(("", crate::model::Visibility::Public));
            // visibility changes rendered code (`pub`), so it is part of the build key
            h.update(format!("{id}:{name}:{vis:?}:{version}\n").as_bytes());
        }
        h.finalize().to_hex().to_string()
    }

    /// Closure hash of every symbol, for storing on symbol documents.
    pub fn all_closure_hashes(&self) -> Result<BTreeMap<String, String>, GraphError> {
        self.symbols
            .keys()
            .map(|id| Ok((id.clone(), self.closure_hash(id)?)))
            .collect()
    }

    /// Deterministic topological order of `members` (dependencies first). Ties are broken by
    /// (types before functions before tests, display name, id). Cycles are legal in Rust and
    /// Python function bodies; remaining members of a cycle are emitted in tie-break order.
    pub fn topo_order(&self, members: &BTreeSet<String>) -> Vec<String> {
        let rank = |id: &String| {
            let s = &self.symbols[id];
            let k = match s.kind {
                Kind::Enum | Kind::Trait => 0,
                Kind::Class => 1,
                Kind::Function => 2,
                Kind::Test => 3,
            };
            (k, s.display_name.clone(), id.clone())
        };
        // deps[x] = members that x depends on
        let mut deps: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for id in members {
            deps.insert(id.clone(), BTreeSet::new());
        }
        for e in &self.edges {
            if e.kind.is_build_dependency()
                && members.contains(&e.from)
                && members.contains(&e.to)
                && e.from != e.to
            {
                deps.get_mut(&e.from).unwrap().insert(e.to.clone());
            }
        }
        let mut order = Vec::new();
        let mut done: BTreeSet<String> = BTreeSet::new();
        while done.len() < members.len() {
            let mut ready: Vec<&String> = members
                .iter()
                .filter(|id| !done.contains(*id) && deps[*id].iter().all(|d| done.contains(d)))
                .collect();
            if ready.is_empty() {
                // cycle: take the lowest-ranked remaining member
                let mut rest: Vec<&String> =
                    members.iter().filter(|id| !done.contains(*id)).collect();
                rest.sort_by_key(|id| rank(id));
                ready = vec![rest[0]];
            }
            ready.sort_by_key(|id| rank(id));
            let next = ready[0].clone();
            done.insert(next.clone());
            order.push(next);
        }
        order
    }

    /// Symbols whose closure includes `id` (inverse closure), excluding `id` itself.
    pub fn dependents(&self, id: &str) -> BTreeSet<String> {
        let mut seen = BTreeSet::new();
        let mut queue = VecDeque::from([id.to_string()]);
        while let Some(cur) = queue.pop_front() {
            for e in self.in_edges(&cur) {
                if e.kind.is_build_dependency() && seen.insert(e.from.clone()) {
                    queue.push_back(e.from.clone());
                }
            }
        }
        seen.remove(id);
        seen
    }

    /// Tests affected by a change to `id`: test-kind dependents, `tested_with` targets, and `id`
    /// itself if it is a test.
    pub fn tests_reaching(&self, id: &str) -> BTreeSet<String> {
        let mut out: BTreeSet<String> = self
            .dependents(id)
            .into_iter()
            .filter(|d| {
                self.symbols
                    .get(d)
                    .map(|s| s.kind == Kind::Test)
                    .unwrap_or(false)
            })
            .collect();
        for e in self.out_edges(id) {
            if e.kind == CodeEdgeKind::TestedWith {
                out.insert(e.to.clone());
            }
        }
        if self
            .symbols
            .get(id)
            .map(|s| s.kind == Kind::Test)
            .unwrap_or(false)
        {
            out.insert(id.to_string());
        }
        out
    }

    /// Methods of a class symbol, sorted by display name.
    pub fn methods_of(&self, class_id: &str) -> Vec<&CodeSymbol> {
        let mut v: Vec<&CodeSymbol> = self
            .edges
            .iter()
            .filter(|e| e.kind == CodeEdgeKind::MemberOf && e.to == class_id)
            .filter_map(|e| self.symbols.get(&e.from))
            .collect();
        v.sort_by(|a, b| a.display_name.cmp(&b.display_name).then(a.id.cmp(&b.id)));
        v
    }

    /// The class a method belongs to, if any.
    pub fn class_of(&self, method_id: &str) -> Option<&CodeSymbol> {
        self.edges
            .iter()
            .find(|e| e.kind == CodeEdgeKind::MemberOf && e.from == method_id)
            .and_then(|e| self.symbols.get(&e.to))
    }

    /// The name used in code for a symbol: methods are stored as `Class::method` (Rust) or
    /// `Class.method` (Python); code uses only the last segment.
    pub fn code_name(&self, id: &str) -> String {
        let s = &self.symbols[id];
        short_name(&s.display_name)
    }
}

pub fn short_name(display_name: &str) -> String {
    display_name
        .rsplit(['.', ':'])
        .next()
        .unwrap_or(display_name)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::empty_object;

    fn content(body: &str) -> VersionContent {
        VersionContent {
            kind: Kind::Function,
            language: Language::Rust,
            signature: "() -> i32".into(),
            body: body.into(),
            attributes: empty_object(),
        }
    }

    fn graph() -> CodeGraph {
        // a -> b -> c ; d -> c ; e standalone
        let mut g = CodeGraph::default();
        for (id, body) in [
            ("a", "b()"),
            ("b", "c()"),
            ("c", "1"),
            ("d", "c()"),
            ("e", "5"),
        ] {
            let v = content(body);
            let h = v.hash();
            g.versions.insert(h.clone(), v);
            g.symbols.insert(
                id.into(),
                CodeSymbol {
                    id: id.into(),
                    display_name: id.into(),
                    kind: Kind::Function,
                    language: Language::Rust,
                    visibility: Visibility::Public,
                    tags: vec![],
                    version: h,
                },
            );
        }
        for (f, t) in [("a", "b"), ("b", "c"), ("d", "c")] {
            g.edges.push(CodeEdge {
                from: f.into(),
                to: t.into(),
                kind: CodeEdgeKind::Calls,
                pinned_version: None,
            });
        }
        g
    }

    #[test]
    fn changing_a_dependency_changes_exactly_its_dependents() {
        let g = graph();
        let before = g.all_closure_hashes().unwrap();
        let mut g2 = g.clone();
        let v = content("2");
        let h = v.hash();
        g2.versions.insert(h.clone(), v);
        g2.symbols.get_mut("c").unwrap().version = h;
        let after = g2.all_closure_hashes().unwrap();
        let changed: BTreeSet<&String> =
            before.keys().filter(|k| before[*k] != after[*k]).collect();
        let expected: BTreeSet<String> =
            ["a", "b", "c", "d"].iter().map(|s| s.to_string()).collect();
        assert_eq!(changed, expected.iter().collect());
        assert_eq!(g.dependents("c").len(), 3);
    }

    #[test]
    fn topo_order_puts_dependencies_first() {
        let g = graph();
        let all: BTreeSet<String> = g.symbols.keys().cloned().collect();
        let order = g.topo_order(&all);
        let pos = |x: &str| order.iter().position(|o| o == x).unwrap();
        assert!(pos("c") < pos("b") && pos("b") < pos("a") && pos("c") < pos("d"));
    }

    #[test]
    fn cycles_are_tolerated() {
        let mut g = graph();
        g.edges.push(CodeEdge {
            from: "c".into(),
            to: "a".into(),
            kind: CodeEdgeKind::Calls,
            pinned_version: None,
        });
        let all: BTreeSet<String> = g.symbols.keys().cloned().collect();
        assert_eq!(g.topo_order(&all).len(), 5);
        assert!(g.closure_hash("a").is_ok());
    }

    #[test]
    fn pins_resolve_and_conflict() {
        let mut g = graph();
        let old = content("0");
        let old_h = old.hash();
        g.versions.insert(old_h.clone(), old);
        g.edges[1].pinned_version = Some(old_h.clone()); // b -> c pinned
        let cl = g.closure(&["a".into()], false).unwrap();
        assert_eq!(cl["c"], old_h);
        assert!(matches!(
            g.closure(&["a".into(), "d".into()], false),
            Err(GraphError::PinConflict { .. })
        ));
    }

    #[test]
    fn short_names() {
        assert_eq!(short_name("UserStore::find_user"), "find_user");
        assert_eq!(short_name("UserStore.find_user"), "find_user");
        assert_eq!(short_name("login"), "login");
    }
}
