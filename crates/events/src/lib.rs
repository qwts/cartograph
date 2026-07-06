//! Channel-identity resolution — deterministic (T0) stitching of
//! producer/consumer event sites into the channel graph (SPEC-00 §3.4,
//! US-0004).
//!
//! Language adapters emit [`adapters_fw::events::EventSite`] facts; this
//! crate resolves each site's channel identity (literal → as-is, env ref →
//! via the [`ConfigIndex`] built from env files present in the repo) and
//! emits `Channel{kind, identity}` nodes with Confirmed `PUBLISHES` /
//! `SUBSCRIBES` edges (AC-0010, AC-0011). A runtime-computed identity
//! cannot be resolved at T0: the site emits an explicit `Gap` node with a
//! reason and the attempted tiers — never silently dropped (AC-0012,
//! R-INT-4). The T1/T2 rungs of the ladder join at M6/M7; cross-repo
//! matching at M5.

use adapters_fw::events::{ChannelRole, EVENT_SDK_VERSION, EventSite, IdentityExpr};
use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::BTreeMap;
use std::path::Path;

/// Stitching errors.
#[derive(Debug, thiserror::Error)]
pub enum StitchError {
    /// Filesystem failure while scanning for config files.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Facts stitched from event sites (same shape as the extractor outputs).
#[derive(Debug, Default)]
pub struct Extraction {
    /// `Channel` and `Gap` nodes.
    pub nodes: Vec<Node>,
    /// `PUBLISHES` / `SUBSCRIBES` edges.
    pub edges: Vec<Edge>,
}

/// Identity of the source being stitched; lands in every `EvidenceRef`.
pub struct SourceId<'a> {
    /// Repository identifier (e.g. `owner/name`, or `local` for a bare dir).
    pub repo: &'a str,
    /// Commit SHA, or `workdir` when extracting an unversioned tree.
    pub commit: &'a str,
}

const EXTRACTOR_ID: &str = "t0.events";

/// Env keys resolved from env files present in the repo (AC-0011). Only
/// files literally named `.env` or `.env.*` participate — this is the
/// deterministic config resolver, not environment guessing.
#[derive(Debug, Default)]
pub struct ConfigIndex {
    /// key → (value, repo-relative source file)
    env: BTreeMap<String, (String, String)>,
}

impl ConfigIndex {
    /// Scan `root` for env files (skipping `node_modules`, `dist`, and
    /// dot-directories) and index their `KEY=VALUE` lines. Files are
    /// visited in sorted order and the first definition of a key wins, so
    /// the index is deterministic regardless of walk order (US-0014).
    pub fn from_dir(root: &Path) -> Result<Self, StitchError> {
        let mut files = Vec::new();
        collect_env_files(root, root, &mut files)?;
        files.sort();
        let mut index = ConfigIndex::default();
        for rel in files {
            let content = std::fs::read_to_string(root.join(&rel))?;
            for line in content.lines() {
                let line = line.trim();
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let Some((key, value)) = line.split_once('=') else {
                    continue;
                };
                let key = key.trim().trim_start_matches("export ").trim();
                let value = value.trim().trim_matches(['"', '\'']);
                if key.is_empty() || key.contains(char::is_whitespace) {
                    continue;
                }
                index
                    .env
                    .entry(key.to_string())
                    .or_insert_with(|| (value.to_string(), rel.clone()));
            }
        }
        Ok(index)
    }

    /// Resolve an env key to `(value, source file)`.
    pub fn resolve(&self, key: &str) -> Option<(&str, &str)> {
        self.env.get(key).map(|(v, f)| (v.as_str(), f.as_str()))
    }
}

fn collect_env_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name == "node_modules" || name == "dist" || name.starts_with('.') {
                continue;
            }
            collect_env_files(root, &path, out)?;
        } else if name == ".env" || name.starts_with(".env.") {
            let rel = path
                .strip_prefix(root)
                .expect("entry under root")
                .to_string_lossy()
                .replace('\\', "/");
            out.push(rel);
        }
    }
    Ok(())
}

fn prov(
    id: &SourceId,
    path: &str,
    span: (u64, u64),
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let p = Provenance::new(
        Tier::Deterministic,
        confidence,
        vec![EvidenceRef {
            repo: id.repo.into(),
            path: path.into(),
            byte_start: span.0,
            byte_end: span.1,
            commit_sha: id.commit.into(),
        }],
        EXTRACTOR_ID,
        fact.as_bytes(),
    )
    .expect("Deterministic tier admits every confidence tier at or below Confirmed");
    serde_json::to_value(p).expect("provenance serializes")
}

/// Graph endpoint a site's edge starts from: its enclosing symbol, or the
/// file itself for top-level sites.
fn site_source(site: &EventSite) -> String {
    site.symbol
        .clone()
        .unwrap_or_else(|| format!("file:{}", site.path))
}

/// Stitch event sites into channel facts. Deterministic: channels are
/// emitted in sorted id order, edges in site order (sites arrive sorted by
/// file from the extractor).
pub fn stitch(sites: &[EventSite], cfg: &ConfigIndex, id: &SourceId) -> Extraction {
    let mut out = Extraction::default();
    let mut channels: BTreeMap<String, Node> = BTreeMap::new();

    for site in sites {
        let edge_label = match site.role {
            ChannelRole::Produces => "PUBLISHES",
            ChannelRole::Consumes => "SUBSCRIBES",
        };
        let src = site_source(site);

        // Resolve the identity at T0: literal as-is (AC-0010), env ref via
        // the config index (AC-0011).
        let resolved: Option<(String, String)> = match &site.identity {
            IdentityExpr::Literal(s) => Some((s.clone(), "literal".into())),
            IdentityExpr::EnvRef(key) => cfg
                .resolve(key)
                .map(|(value, file)| (value.to_string(), format!("config:{file}"))),
            IdentityExpr::Computed(_) => None,
        };

        match (resolved, &site.identity) {
            (Some((identity, resolver)), _) => {
                let chan_id = format!("chan:{}:{}", site.kind, identity);
                channels.entry(chan_id.clone()).or_insert_with(|| Node {
                    id: chan_id.clone(),
                    label: "Channel".into(),
                    props: serde_json::json!({
                        "kind": site.kind,
                        "identity": identity,
                        "prov": prov(
                            id, &site.path, (site.byte_start, site.byte_end),
                            ConfidenceTier::Confirmed, &format!("Channel {chan_id}"),
                        ),
                    }),
                });
                out.edges.push(Edge {
                    src,
                    dst: chan_id.clone(),
                    label: edge_label.into(),
                    props: serde_json::json!({
                        "resolver": resolver,
                        "registry": EVENT_SDK_VERSION,
                        "prov": prov(
                            id, &site.path, (site.byte_start, site.byte_end),
                            ConfidenceTier::Confirmed, &format!("{edge_label} -> {chan_id}"),
                        ),
                    }),
                });
            }
            (None, identity) => {
                // Unresolved at T0. No higher rung exists yet (T1 at M6,
                // T2 at M7), so the ladder bottoms out here: an explicit
                // Gap node truncates the hop with a reason (AC-0012,
                // R-INT-4) — the site is never silently dropped.
                let (reason, raw) = match identity {
                    IdentityExpr::EnvRef(key) => (
                        format!("env key {key} not found in any env file in the repo"),
                        format!("process.env.{key}"),
                    ),
                    IdentityExpr::Computed(raw) => {
                        ("runtime-computed channel identity".to_string(), raw.clone())
                    }
                    IdentityExpr::Literal(_) => unreachable!("literals always resolve"),
                };
                let gap_id = format!("gap:chan:{}@{}", site.path, site.byte_start);
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": reason,
                        "raw": raw,
                        "kind": site.kind,
                        "attempted_tiers": ["T0"],
                        "prov": prov(
                            id, &site.path, (site.byte_start, site.byte_end),
                            ConfidenceTier::Gap, &format!("Gap {gap_id}"),
                        ),
                    }),
                });
                out.edges.push(Edge {
                    src,
                    dst: gap_id.clone(),
                    label: edge_label.into(),
                    props: serde_json::json!({
                        "registry": EVENT_SDK_VERSION,
                        "prov": prov(
                            id, &site.path, (site.byte_start, site.byte_end),
                            ConfidenceTier::Gap, &format!("{edge_label} -> {gap_id}"),
                        ),
                    }),
                });
            }
        }
    }

    out.nodes.extend(channels.into_values());
    out
}

/// Path portion of a fetch URL: query/fragment stripped, scheme+host
/// stripped when present (`https://api.example/users?x=1` → `/users`).
fn url_path(url: &str) -> String {
    let u = url.split(['?', '#']).next().unwrap_or(url);
    if let Some(i) = u.find("://") {
        let rest = &u[i + 3..];
        match rest.find('/') {
            Some(j) => rest[j..].to_string(),
            None => "/".to_string(),
        }
    } else {
        u.to_string()
    }
}

/// True when a concrete request path matches an endpoint's route pattern:
/// exact segments, or `:param` segments matching any single segment.
fn route_matches(pattern: &str, concrete: &str) -> bool {
    let ps: Vec<&str> = pattern.split('/').collect();
    let cs: Vec<&str> = concrete.split('/').collect();
    ps.len() == cs.len()
        && ps
            .iter()
            .zip(&cs)
            .all(|(p, c)| p == c || (p.starts_with(':') && !c.is_empty()))
}

/// Stitch data-fetch sites against recovered endpoints (US-0005, AC-0014):
/// a resolvable URL whose method+path matches exactly one endpoint route is
/// a Confirmed `FETCHES` edge; anything else — computed URL, unresolved env
/// key, no match, ambiguous match — emits an explicit `Gap` with a reason
/// (the ladder above T0 is empty until M6/M7; never silently dropped).
pub fn stitch_fetches(
    sites: &[adapters_fw::client::FetchSite],
    endpoint_ids: &[String],
    cfg: &ConfigIndex,
    id: &SourceId,
) -> Extraction {
    // ep:VERB:route, parsed once.
    let endpoints: Vec<(&str, &str, &str)> = endpoint_ids
        .iter()
        .filter_map(|eid| {
            let rest = eid.strip_prefix("ep:")?;
            let (verb, route) = rest.split_once(':')?;
            Some((eid.as_str(), verb, route))
        })
        .collect();

    let mut out = Extraction::default();
    for site in sites {
        let src = site
            .symbol
            .clone()
            .unwrap_or_else(|| format!("file:{}", site.path));
        let fetch_prov = |confidence: ConfidenceTier, fact: &str| {
            prov(
                id,
                &site.path,
                (site.byte_start, site.byte_end),
                confidence,
                fact,
            )
        };

        let resolved: Option<(String, String)> = match &site.url {
            IdentityExpr::Literal(u) => Some((url_path(u), "literal".into())),
            IdentityExpr::EnvRef(key) => cfg
                .resolve(key)
                .map(|(value, file)| (url_path(value), format!("config:{file}"))),
            IdentityExpr::Computed(_) => None,
        };

        let outcome: Result<(&str, String), String> = match (&resolved, site.method.as_str()) {
            (None, _) => Err(match &site.url {
                IdentityExpr::EnvRef(key) => {
                    format!("env key {key} not found in any env file in the repo")
                }
                _ => "runtime-computed fetch URL".to_string(),
            }),
            (Some(_), "?") => Err("runtime-computed HTTP method".to_string()),
            (Some((path, resolver)), method) => {
                let hits: Vec<&str> = endpoints
                    .iter()
                    .filter(|(_, verb, route)| *verb == method && route_matches(route, path))
                    .map(|(eid, _, _)| *eid)
                    .collect();
                match hits.as_slice() {
                    [one] => Ok((one, resolver.clone())),
                    [] => Err(format!("no recovered endpoint matches {method} {path}")),
                    many => Err(format!(
                        "ambiguous endpoint match for {method} {path} ({} candidates)",
                        many.len()
                    )),
                }
            }
        };

        match outcome {
            Ok((ep_id, resolver)) => {
                out.edges.push(Edge {
                    src,
                    dst: ep_id.to_string(),
                    label: "FETCHES".into(),
                    props: serde_json::json!({
                        "method": site.method,
                        "resolver": resolver,
                        "prov": fetch_prov(
                            ConfidenceTier::Confirmed,
                            &format!("FETCHES -> {ep_id}"),
                        ),
                    }),
                });
            }
            Err(reason) => {
                let gap_id = format!("gap:fetch:{}@{}", site.path, site.byte_start);
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": reason,
                        "attempted_tiers": ["T0"],
                        "prov": fetch_prov(ConfidenceTier::Gap, &format!("Gap {gap_id}")),
                    }),
                });
                out.edges.push(Edge {
                    src,
                    dst: gap_id.clone(),
                    label: "FETCHES".into(),
                    props: serde_json::json!({
                        "method": site.method,
                        "prov": fetch_prov(ConfidenceTier::Gap, &format!("FETCHES -> {gap_id}")),
                    }),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests;
