//! Recovery metrics (#119): per-extractor coverage and the per-ingest
//! history record that makes the determinism invariant observable — two
//! ingests of the same commit must show identical graph content hashes
//! (ADR-0014, AC-0039).

use core_graph::{Edge, Node};
use rusqlite::{Connection, params};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// Coverage of one extractor over its scope for a single ingest.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExtractorCoverage {
    /// Extractor id as recorded on provenance (e.g. `t0.adapter-ts`).
    pub extractor: String,
    /// Source files the extractor's scope contained.
    pub files_in_scope: u64,
    /// Distinct files that produced at least one fact.
    pub files_with_facts: u64,
    /// Facts (nodes + edges) attributed to the extractor.
    pub facts: u64,
    /// files_with_facts / files_in_scope, in percent; null when the scope
    /// is unknown (an extractor outside the declared layer map).
    pub coverage_pct: Option<f64>,
}

/// One per-ingest history record: tier tallies, register counts, and the
/// whole-graph content hash.
#[derive(Debug, Clone, Serialize)]
pub struct IngestRecord {
    pub id: i64,
    pub job_id: i64,
    pub repo: String,
    pub commit_sha: String,
    pub confirmed: u64,
    pub inferred_strong: u64,
    pub inferred_weak: u64,
    pub gap: u64,
    pub unsupported: u64,
    pub no_evidence: u64,
    pub graph_facts: u64,
    /// BLAKE3 over the sorted fact identities + their content hashes.
    pub content_hash: String,
    pub created_at: String,
}

/// Tier tallies plus coverage, computed from one graph projection.
pub struct GraphMetrics {
    pub confirmed: u64,
    pub inferred_strong: u64,
    pub inferred_weak: u64,
    pub gap: u64,
    pub graph_facts: u64,
    pub content_hash: String,
    pub coverage: Vec<ExtractorCoverage>,
}

/// Compute tallies, coverage, and the canonical graph content hash from a
/// whole-graph projection. `scope` maps extractor id → source files in its
/// scope this ingest (extractors it omits report a null coverage_pct).
/// Counts use the spec register's own provenance definition — one
/// definition, every surface (#116).
pub fn compute(nodes: &[Node], edges: &[Edge], scope: &BTreeMap<String, u64>) -> GraphMetrics {
    let mut confirmed = 0u64;
    let mut inferred_strong = 0u64;
    let mut inferred_weak = 0u64;
    let mut gap = 0u64;
    let mut facts_by_extractor: BTreeMap<String, u64> = BTreeMap::new();
    let mut files_by_extractor: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    // Canonical fact lines, sorted before hashing so store order is
    // irrelevant: identity + per-fact content hash.
    let mut lines: Vec<String> = Vec::with_capacity(nodes.len() + edges.len());

    let mut tally = |provenance: &core_prov::Provenance| {
        use core_prov::ConfidenceTier::*;
        match provenance.confidence_tier {
            Confirmed => confirmed += 1,
            InferredStrong => inferred_strong += 1,
            InferredWeak => inferred_weak += 1,
            Gap => gap += 1,
        }
    };

    for node in nodes {
        let provenance = spec::provenance(&node.props, &node.id);
        tally(&provenance);
        *facts_by_extractor
            .entry(provenance.extractor_id.clone())
            .or_default() += 1;
        for evidence in &provenance.evidence {
            files_by_extractor
                .entry(provenance.extractor_id.clone())
                .or_default()
                .insert(format!("{}:{}", evidence.repo, evidence.path));
        }
        lines.push(format!(
            "n {} {} {}",
            node.id, node.label, provenance.content_hash
        ));
    }
    for edge in edges {
        let identity = format!("{} {} {}", edge.src, edge.label, edge.dst);
        let provenance = spec::provenance(&edge.props, &identity);
        tally(&provenance);
        *facts_by_extractor
            .entry(provenance.extractor_id.clone())
            .or_default() += 1;
        for evidence in &provenance.evidence {
            files_by_extractor
                .entry(provenance.extractor_id.clone())
                .or_default()
                .insert(format!("{}:{}", evidence.repo, evidence.path));
        }
        lines.push(format!("e {} {}", identity, provenance.content_hash));
    }
    lines.sort();

    // Scope-declared extractors always appear — a covering adapter that
    // produced zero facts is a 0% row, not a missing row.
    let mut extractors: BTreeSet<String> = scope.keys().cloned().collect();
    extractors.extend(facts_by_extractor.keys().cloned());
    let coverage = extractors
        .into_iter()
        .map(|extractor| {
            let files_with_facts = files_by_extractor
                .get(&extractor)
                .map(|files| files.len() as u64)
                .unwrap_or(0);
            let files_in_scope = scope.get(&extractor).copied().unwrap_or(0);
            let coverage_pct = if scope.contains_key(&extractor) {
                Some(if files_in_scope == 0 {
                    0.0
                } else {
                    (files_with_facts as f64 / files_in_scope as f64) * 100.0
                })
            } else {
                None
            };
            ExtractorCoverage {
                facts: facts_by_extractor.get(&extractor).copied().unwrap_or(0),
                files_with_facts,
                files_in_scope,
                coverage_pct,
                extractor,
            }
        })
        .collect();

    GraphMetrics {
        confirmed,
        inferred_strong,
        inferred_weak,
        gap,
        graph_facts: (nodes.len() + edges.len()) as u64,
        content_hash: core_prov::content_hash(lines.join("\n").as_bytes()),
        coverage,
    }
}

/// Store for ingest history + coverage, backed by the state-spine database.
pub struct MetricsStore {
    conn: Connection,
}

impl MetricsStore {
    pub fn open(path: impl AsRef<Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS ingest_history (
                 id           INTEGER PRIMARY KEY AUTOINCREMENT,
                 job_id       INTEGER NOT NULL,
                 repo         TEXT NOT NULL,
                 commit_sha   TEXT NOT NULL,
                 confirmed    INTEGER NOT NULL,
                 inferred_strong INTEGER NOT NULL,
                 inferred_weak INTEGER NOT NULL,
                 gap          INTEGER NOT NULL,
                 unsupported  INTEGER NOT NULL,
                 no_evidence  INTEGER NOT NULL,
                 graph_facts  INTEGER NOT NULL,
                 content_hash TEXT NOT NULL,
                 created_at   TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%SZ','now'))
             );
             CREATE TABLE IF NOT EXISTS extractor_coverage (
                 history_id      INTEGER NOT NULL REFERENCES ingest_history(id),
                 extractor       TEXT NOT NULL,
                 files_in_scope  INTEGER NOT NULL,
                 files_with_facts INTEGER NOT NULL,
                 facts           INTEGER NOT NULL,
                 scoped          INTEGER NOT NULL CHECK (scoped IN (0, 1)),
                 PRIMARY KEY (history_id, extractor)
             );",
        )?;
        Ok(Self { conn })
    }

    /// Persist one ingest's metrics; returns the stored record.
    pub fn record(
        &mut self,
        job_id: i64,
        repo: &str,
        commit_sha: &str,
        metrics: &GraphMetrics,
        unsupported: u64,
        no_evidence: u64,
    ) -> rusqlite::Result<IngestRecord> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO ingest_history
               (job_id, repo, commit_sha, confirmed, inferred_strong, inferred_weak,
                gap, unsupported, no_evidence, graph_facts, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                job_id,
                repo,
                commit_sha,
                metrics.confirmed as i64,
                metrics.inferred_strong as i64,
                metrics.inferred_weak as i64,
                metrics.gap as i64,
                unsupported as i64,
                no_evidence as i64,
                metrics.graph_facts as i64,
                metrics.content_hash,
            ],
        )?;
        let history_id = tx.last_insert_rowid();
        for coverage in &metrics.coverage {
            tx.execute(
                "INSERT INTO extractor_coverage
                   (history_id, extractor, files_in_scope, files_with_facts, facts, scoped)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    history_id,
                    coverage.extractor,
                    coverage.files_in_scope as i64,
                    coverage.files_with_facts as i64,
                    coverage.facts as i64,
                    coverage.coverage_pct.is_some(),
                ],
            )?;
        }
        tx.commit()?;
        let record = self
            .history(1)?
            .into_iter()
            .next()
            .expect("row just inserted");
        Ok(record)
    }

    /// Ingest records, newest first.
    pub fn history(&self, limit: u32) -> rusqlite::Result<Vec<IngestRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, job_id, repo, commit_sha, confirmed, inferred_strong,
                    inferred_weak, gap, unsupported, no_evidence, graph_facts,
                    content_hash, created_at
             FROM ingest_history ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map(params![limit], |row| {
            Ok(IngestRecord {
                id: row.get(0)?,
                job_id: row.get(1)?,
                repo: row.get(2)?,
                commit_sha: row.get(3)?,
                confirmed: row.get::<_, i64>(4)? as u64,
                inferred_strong: row.get::<_, i64>(5)? as u64,
                inferred_weak: row.get::<_, i64>(6)? as u64,
                gap: row.get::<_, i64>(7)? as u64,
                unsupported: row.get::<_, i64>(8)? as u64,
                no_evidence: row.get::<_, i64>(9)? as u64,
                graph_facts: row.get::<_, i64>(10)? as u64,
                content_hash: row.get(11)?,
                created_at: row.get(12)?,
            })
        })?;
        rows.collect()
    }

    /// Coverage rows for the most recent ingest (empty before the first).
    pub fn latest_coverage(&self) -> rusqlite::Result<Vec<ExtractorCoverage>> {
        let mut stmt = self.conn.prepare(
            "SELECT extractor, files_in_scope, files_with_facts, facts, scoped
             FROM extractor_coverage
             WHERE history_id = (SELECT MAX(id) FROM ingest_history)
             ORDER BY extractor",
        )?;
        let rows = stmt.query_map([], |row| {
            let files_in_scope = row.get::<_, i64>(1)? as u64;
            let files_with_facts = row.get::<_, i64>(2)? as u64;
            let scoped: bool = row.get(4)?;
            Ok(ExtractorCoverage {
                extractor: row.get(0)?,
                files_in_scope,
                files_with_facts,
                facts: row.get::<_, i64>(3)? as u64,
                coverage_pct: scoped.then(|| {
                    if files_in_scope == 0 {
                        0.0
                    } else {
                        (files_with_facts as f64 / files_in_scope as f64) * 100.0
                    }
                }),
            })
        })?;
        rows.collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fact_props(extractor: &str, confidence: &str, path: &str, hash: &str) -> serde_json::Value {
        json!({
            "prov": {
                "tier": "Deterministic",
                "confidence_tier": confidence,
                "evidence": [{
                    "repo": "local/fixture",
                    "path": path,
                    "byte_start": 0,
                    "byte_end": 10,
                    "commit_sha": "workdir",
                }],
                "extractor_id": extractor,
                "content_hash": hash,
            }
        })
    }

    fn node(id: &str, extractor: &str, confidence: &str, path: &str) -> Node {
        Node {
            id: id.into(),
            label: "Symbol".into(),
            props: fact_props(extractor, confidence, path, &"a".repeat(64)),
        }
    }

    fn edge(src: &str, dst: &str, extractor: &str) -> Edge {
        Edge {
            src: src.into(),
            dst: dst.into(),
            label: "CALLS".into(),
            props: fact_props(extractor, "Confirmed", "src/a.ts", &"b".repeat(64)),
        }
    }

    fn ts_scope(files: u64) -> BTreeMap<String, u64> {
        BTreeMap::from([("t0.adapter-ts".to_string(), files)])
    }

    #[test]
    fn coverage_counts_distinct_files_and_reports_unscoped_extractors() {
        let nodes = vec![
            node("a", "t0.adapter-ts", "Confirmed", "src/a.ts"),
            node("b", "t0.adapter-ts", "Confirmed", "src/a.ts"),
            node("c", "t0.adapter-ts", "Confirmed", "src/c.ts"),
            node("d", "t1.dynamic", "InferredStrong", "traces/run.json"),
        ];
        let edges = vec![edge("a", "b", "t0.adapter-ts")];
        let metrics = compute(&nodes, &edges, &ts_scope(4));

        let ts = metrics
            .coverage
            .iter()
            .find(|c| c.extractor == "t0.adapter-ts")
            .unwrap();
        // Two facts in a.ts count one file; the edge shares a.ts too.
        assert_eq!(ts.files_with_facts, 2);
        assert_eq!(ts.facts, 4);
        assert_eq!(ts.coverage_pct, Some(50.0));

        // An extractor outside the declared scope is visible but unmeasured.
        let dynamic = metrics
            .coverage
            .iter()
            .find(|c| c.extractor == "t1.dynamic")
            .unwrap();
        assert_eq!(dynamic.facts, 1);
        assert_eq!(dynamic.coverage_pct, None);

        assert_eq!(metrics.confirmed, 4);
        assert_eq!(metrics.inferred_strong, 1);
        assert_eq!(metrics.graph_facts, 5);
    }

    #[test]
    fn scoped_adapter_with_zero_facts_is_a_zero_row_not_a_missing_row() {
        let metrics = compute(&[], &[], &ts_scope(3));
        let ts = &metrics.coverage[0];
        assert_eq!(ts.extractor, "t0.adapter-ts");
        assert_eq!(ts.coverage_pct, Some(0.0));
    }

    #[test]
    fn content_hash_is_order_independent_and_content_sensitive() {
        let a = node("a", "t0.adapter-ts", "Confirmed", "src/a.ts");
        let b = node("b", "t0.adapter-ts", "Confirmed", "src/b.ts");
        let scope = ts_scope(2);

        let forward = compute(&[a.clone(), b.clone()], &[], &scope);
        let reversed = compute(&[b.clone(), a.clone()], &[], &scope);
        // AC-0039 made observable: same facts ⇒ same hash, store order aside.
        assert_eq!(forward.content_hash, reversed.content_hash);

        let mut changed = a.clone();
        changed.props["prov"]["content_hash"] = json!("c".repeat(64));
        let different = compute(&[changed, b], &[], &scope);
        assert_ne!(forward.content_hash, different.content_hash);
    }

    #[test]
    fn history_round_trips_and_orders_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = MetricsStore::open(dir.path().join("state.db")).unwrap();
        let nodes = vec![node("a", "t0.adapter-ts", "Confirmed", "src/a.ts")];
        let metrics = compute(&nodes, &[], &ts_scope(1));

        let first = store
            .record(7, "local/fixture", "workdir", &metrics, 2, 1)
            .unwrap();
        assert_eq!(first.unsupported, 2);
        assert_eq!(first.no_evidence, 1);
        assert_eq!(first.graph_facts, 1);
        assert_eq!(first.content_hash, metrics.content_hash);

        // A second identical ingest records the identical hash — the
        // determinism invariant as data, not just a test assertion.
        let second = store
            .record(8, "local/fixture", "workdir", &metrics, 2, 1)
            .unwrap();
        let history = store.history(10).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].id, second.id);
        assert_eq!(history[0].content_hash, history[1].content_hash);

        let coverage = store.latest_coverage().unwrap();
        assert_eq!(coverage.len(), 1);
        assert_eq!(coverage[0].coverage_pct, Some(100.0));
    }
}
