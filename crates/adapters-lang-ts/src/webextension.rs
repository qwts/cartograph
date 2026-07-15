//! WebExtension manifest adapter (US-0016): deterministic T0 topology and
//! security facts from Manifest V2/V3 `manifest.json`.
//!
//! Emits, per recognized manifest: an `Extension` node, `ExtensionContext`
//! nodes for the manifest-declared execution contexts (background service
//! worker / scripts, content scripts, extension pages, toolbar action),
//! `Command` nodes for declared commands, and `Permission` nodes with
//! `GRANTS` edges carrying exact scopes — wildcard host patterns surface in
//! the security projection exactly like over-broad IAM grants (ADR-0013).
//! Every fact carries [`core_prov::Provenance`] with an evidence span into
//! the manifest itself. A declared entry file that cannot be found in the
//! source tree becomes an explicit Gap, never a silent omission (R-INT-4).

use core_graph::{Edge, Node};
use core_prov::{ConfidenceTier, EvidenceRef, Provenance, Tier};
use std::collections::BTreeSet;
use std::path::Path;

use crate::{ExtractError, Extraction, SourceId};

const EXTRACTOR_ID: &str = "t0.webextension";

/// Byte span of `needle` scoped under `anchor` — the manifest key that
/// declares the fact — so a value repeated elsewhere (e.g. a host pattern
/// in both `content_scripts.matches` and `optional_host_permissions`)
/// cites its own declaration, not the first occurrence in the file
/// (#148 review). Quoted occurrences win, so a pattern embedded in a
/// longer string can't match; the span covers the string content.
/// Falls back progressively: unscoped quoted → unscoped raw → whole file.
fn span_scoped(raw: &str, anchor: &str, needle: &str) -> (u64, u64) {
    let from = raw.find(&format!("\"{anchor}\"")).unwrap_or(0);
    let quoted = format!("\"{needle}\"");
    if let Some(pos) = raw[from..].find(&quoted) {
        let start = from + pos + 1;
        return (start as u64, (start + needle.len()) as u64);
    }
    for haystack_from in [from, 0] {
        if let Some(pos) = raw[haystack_from..].find(needle) {
            let start = haystack_from + pos;
            return (start as u64, (start + needle.len()) as u64);
        }
    }
    (0, raw.len() as u64)
}

fn prov_value(
    id: &SourceId,
    path: &str,
    span: (u64, u64),
    confidence: ConfidenceTier,
    fact: &str,
) -> serde_json::Value {
    let provenance = Provenance::new(
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
    .expect("Deterministic within ceiling");
    serde_json::to_value(provenance).expect("provenance serializes")
}

fn strings(value: &serde_json::Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Walk for `manifest.json` files outside dependency/build/hidden trees.
fn collect_manifests(root: &Path, dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if name == "node_modules" || name == "dist" || name.starts_with('.') {
                continue;
            }
            collect_manifests(root, &path, out)?;
        } else if name == "manifest.json" {
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

/// Normalize `{manifest_dir}/{declared}` into a repo-relative path.
fn entry_path(dir: &str, declared: &str) -> String {
    let declared = declared.trim_start_matches('/');
    if dir.is_empty() {
        declared.to_string()
    } else {
        format!("{dir}/{declared}")
    }
}

/// One permission grant: exact scopes, never widened or collapsed.
struct Grant {
    /// Manifest key that declares this grant (evidence-span scope).
    anchor: &'static str,
    /// Permission-node id suffix (namespaced per manifest).
    suffix: String,
    /// The manifest-verbatim permission name or match pattern.
    display: String,
    /// API surface granted (empty for pure host scopes).
    actions: Vec<String>,
    /// Host/match scopes granted (empty for pure API permissions).
    resource_scopes: Vec<String>,
    /// Declared under an `optional_*` manifest key.
    optional: bool,
}

struct ManifestCx<'a> {
    id: &'a SourceId<'a>,
    root: &'a Path,
    path: String,
    dir: String,
    base: String,
    raw: String,
    known_files: &'a BTreeSet<String>,
    emitted_files: BTreeSet<String>,
}

impl ManifestCx<'_> {
    fn prov(&self, anchor: &str, needle: &str, fact: &str) -> serde_json::Value {
        prov_value(
            self.id,
            &self.path,
            span_scoped(&self.raw, anchor, needle),
            ConfidenceTier::Confirmed,
            fact,
        )
    }

    /// Bind a context to its entry source file. Manifests point at built
    /// artifacts, so a missing `.js` deterministically falls back to the
    /// sibling `.ts` source. Neither on disk = explicit Gap (R-INT-4).
    fn bind_entry(&mut self, out: &mut Extraction, ctx_id: &str, anchor: &str, declared: &str) {
        let rel = entry_path(&self.dir, declared);
        let resolved = if self.root.join(&rel).is_file() {
            Some((rel.clone(), false))
        } else {
            rel.strip_suffix(".js")
                .map(|stem| format!("{stem}.ts"))
                .filter(|ts| self.root.join(ts).is_file())
                .map(|ts| (ts, true))
        };
        match resolved {
            Some((file_rel, from_build_path)) => {
                let file_id = format!("file:{}@{}", self.id.repo, file_rel);
                // The TS pass only extracts .ts/.tsx — give plain-JS entries
                // a real File node (cited from the manifest), once.
                if !self.known_files.contains(&file_id)
                    && self.emitted_files.insert(file_id.clone())
                {
                    out.nodes.push(Node {
                        id: file_id.clone(),
                        label: "File".into(),
                        props: serde_json::json!({
                            "path": file_rel,
                            "prov": self.prov(anchor, declared, &format!("File {file_rel}")),
                        }),
                    });
                }
                out.edges.push(Edge {
                    src: ctx_id.into(),
                    dst: file_id,
                    label: "ENTRY".into(),
                    props: serde_json::json!({
                        "declared": declared,
                        "resolved_from_build_path": from_build_path,
                        "prov": self.prov(anchor, declared, &format!("ENTRY {ctx_id} -> {file_rel}")),
                    }),
                });
            }
            None => {
                let gap_id = format!("gap:webext:{}@{rel}", self.id.repo);
                let reason = "manifest entry not found in source tree";
                out.nodes.push(Node {
                    id: gap_id.clone(),
                    label: "Gap".into(),
                    props: serde_json::json!({
                        "reason": reason,
                        "declared": declared,
                        "attempted_tiers": ["T0"],
                        "prov": prov_value(
                            self.id,
                            &self.path,
                            span_scoped(&self.raw, anchor, declared),
                            ConfidenceTier::Gap,
                            &format!("Gap {gap_id}"),
                        ),
                    }),
                });
                out.edges.push(Edge {
                    src: ctx_id.into(),
                    dst: gap_id.clone(),
                    label: "ENTRY".into(),
                    props: serde_json::json!({
                        "declared": declared,
                        "prov": prov_value(
                            self.id,
                            &self.path,
                            span_scoped(&self.raw, anchor, declared),
                            ConfidenceTier::Gap,
                            &format!("ENTRY {ctx_id} -> {gap_id}"),
                        ),
                    }),
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn context(
        &mut self,
        out: &mut Extraction,
        ext_id: &str,
        kind: &str,
        anchor: &str,
        key: &str,
        entries: &[String],
        extra: serde_json::Value,
    ) -> String {
        let ctx_id = format!("extctx:{}:{kind}:{key}", self.base);
        let mut props = serde_json::json!({
            "kind": kind,
            "entries": entries,
            "prov": self.prov(anchor, key, &format!("ExtensionContext {ctx_id}")),
        });
        if let (Some(props_map), Some(extra_map)) = (props.as_object_mut(), extra.as_object()) {
            for (k, v) in extra_map {
                props_map.insert(k.clone(), v.clone());
            }
        }
        out.nodes.push(Node {
            id: ctx_id.clone(),
            label: "ExtensionContext".into(),
            props,
        });
        out.edges.push(Edge {
            src: ext_id.into(),
            dst: ctx_id.clone(),
            label: "DECLARES".into(),
            props: serde_json::json!({
                "prov": self.prov(anchor, key, &format!("DECLARES {ext_id} -> {ctx_id}")),
            }),
        });
        for declared in entries {
            self.bind_entry(out, &ctx_id, anchor, declared);
        }
        ctx_id
    }

    fn grant(&self, out: &mut Extraction, ext_id: &str, grant: Grant) {
        let Grant {
            anchor,
            suffix,
            display,
            actions,
            resource_scopes,
            optional,
        } = grant;
        let perm_id = format!("perm:{}:{suffix}", self.base);
        out.nodes.push(Node {
            id: perm_id.clone(),
            label: "Permission".into(),
            props: serde_json::json!({
                "name": display,
                "optional": optional,
                "prov": self.prov(anchor, &display, &format!("Permission {perm_id}")),
            }),
        });
        out.edges.push(Edge {
            src: ext_id.into(),
            dst: perm_id.clone(),
            label: "GRANTS".into(),
            props: serde_json::json!({
                "actions": actions,
                "resource_scopes": resource_scopes,
                "optional": optional,
                "prov": self.prov(anchor, &display, &format!("GRANTS {ext_id} -> {perm_id}")),
            }),
        });
    }
}

/// Extract WebExtension manifest facts from every recognized
/// `manifest.json` under `root`, returning the facts plus the number of
/// recognized manifests (the extractor's coverage scope). `known_files`
/// are File-node ids already recovered by the language pass, so plain-JS
/// entry files gain a cited File node exactly once.
pub fn extract_manifests(
    root: &Path,
    id: &SourceId,
    known_files: &BTreeSet<String>,
) -> Result<(Extraction, u64), ExtractError> {
    let mut manifests = Vec::new();
    collect_manifests(root, root, &mut manifests)?;
    manifests.sort(); // deterministic order (US-0014)
    let mut out = Extraction::default();
    let mut recognized = 0u64;
    for manifest_rel in manifests {
        let raw = std::fs::read_to_string(root.join(&manifest_rel))?;
        if !raw.contains("\"manifest_version\"") {
            continue; // e.g. a web-app manifest; not an extension manifest.
        }
        recognized += 1;
        let dir = manifest_rel
            .rsplit_once('/')
            .map(|(dir, _)| dir.to_string())
            .unwrap_or_default();
        let base = format!(
            "{}@{}",
            id.repo,
            if dir.is_empty() { "." } else { dir.as_str() }
        );
        let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&raw) else {
            // Recognized but unparseable: an explicit finding-shaped Gap
            // beats silently producing no facts.
            let gap_id = format!("gap:webext-manifest:{base}");
            out.nodes.push(Node {
                id: gap_id.clone(),
                label: "Gap".into(),
                props: serde_json::json!({
                    "reason": "manifest.json is not valid JSON",
                    "attempted_tiers": ["T0"],
                    "prov": prov_value(
                        id,
                        &manifest_rel,
                        (0, raw.len() as u64),
                        ConfidenceTier::Gap,
                        &format!("Gap {gap_id}"),
                    ),
                }),
            });
            continue;
        };

        let ext_id = format!("ext:{base}");
        let mut cx = ManifestCx {
            id,
            root,
            path: manifest_rel.clone(),
            dir,
            base: base.clone(),
            raw,
            known_files,
            emitted_files: BTreeSet::new(),
        };
        out.nodes.push(Node {
            id: ext_id.clone(),
            label: "Extension".into(),
            props: serde_json::json!({
                "name": manifest["name"].as_str().unwrap_or_default(),
                "version": manifest["version"].as_str().unwrap_or_default(),
                "manifest_version": manifest["manifest_version"].as_u64().unwrap_or_default(),
                "web_accessible_matches": manifest["web_accessible_resources"]
                    .as_array()
                    .map(|entries| entries
                        .iter()
                        .flat_map(|entry| strings(&entry["matches"]))
                        .collect::<Vec<_>>())
                    .unwrap_or_default(),
                "prov": cx.prov("manifest_version", "manifest_version", &format!("Extension {ext_id}")),
            }),
        });

        // --- Execution contexts ---------------------------------------------
        let background = &manifest["background"];
        if let Some(worker) = background["service_worker"].as_str() {
            cx.context(
                &mut out,
                &ext_id,
                "service-worker",
                "background",
                worker,
                &[worker.to_string()],
                serde_json::json!({}),
            );
        }
        let mv2_scripts = strings(&background["scripts"]);
        if !mv2_scripts.is_empty() {
            cx.context(
                &mut out,
                &ext_id,
                "background-scripts",
                "background",
                &mv2_scripts[0].clone(),
                &mv2_scripts,
                serde_json::json!({}),
            );
        }
        if let Some(page) = background["page"].as_str() {
            cx.context(
                &mut out,
                &ext_id,
                "background-page",
                "background",
                page,
                &[page.to_string()],
                serde_json::json!({}),
            );
        }
        if let Some(scripts) = manifest["content_scripts"].as_array() {
            for (index, script) in scripts.iter().enumerate() {
                let js = strings(&script["js"]);
                let key = js
                    .first()
                    .cloned()
                    .unwrap_or_else(|| format!("content-script-{index}"));
                cx.context(
                    &mut out,
                    &ext_id,
                    "content-script",
                    "content_scripts",
                    &key,
                    &js,
                    serde_json::json!({ "matches": strings(&script["matches"]) }),
                );
            }
        }
        for (anchor, value) in [
            ("action", manifest["action"]["default_popup"].as_str()),
            (
                "browser_action",
                manifest["browser_action"]["default_popup"].as_str(),
            ),
            ("options_page", manifest["options_page"].as_str()),
            ("options_ui", manifest["options_ui"]["page"].as_str()),
            ("devtools_page", manifest["devtools_page"].as_str()),
        ] {
            if let Some(page) = value {
                cx.context(
                    &mut out,
                    &ext_id,
                    "page",
                    anchor,
                    page,
                    &[page.to_string()],
                    serde_json::json!({}),
                );
            }
        }
        // A toolbar action with no popup is still a user trigger — the MV3
        // `action` and the MV2 `browser_action` shape alike (#148 review).
        for anchor in ["action", "browser_action"] {
            let action = &manifest[anchor];
            if action.is_object() && action["default_popup"].is_null() {
                cx.context(
                    &mut out,
                    &ext_id,
                    "action",
                    anchor,
                    anchor,
                    &[],
                    serde_json::json!({
                        "title": action["default_title"].as_str().unwrap_or_default(),
                    }),
                );
            }
        }

        // --- Commands (user triggers) ----------------------------------------
        if let Some(commands) = manifest["commands"].as_object() {
            for (name, command) in commands {
                let cmd_id = format!("extcmd:{base}:{name}");
                out.nodes.push(Node {
                    id: cmd_id.clone(),
                    label: "Command".into(),
                    props: serde_json::json!({
                        "name": name,
                        "description": command["description"].as_str().unwrap_or_default(),
                        "prov": cx.prov("commands", name, &format!("Command {cmd_id}")),
                    }),
                });
                out.edges.push(Edge {
                    src: ext_id.clone(),
                    dst: cmd_id.clone(),
                    label: "DECLARES".into(),
                    props: serde_json::json!({
                        "prov": cx.prov("commands", name, &format!("DECLARES {ext_id} -> {cmd_id}")),
                    }),
                });
            }
        }

        // --- Permissions as GRANTS: exact scopes, security-projectable -------
        // MV2 mixes host patterns into (optional_)permissions: a pattern is
        // a resource scope, never an API action (#148 review).
        let is_host_pattern = |name: &str| name.contains("://") || name.starts_with('<');
        for (anchor, optional) in [("permissions", false), ("optional_permissions", true)] {
            for name in strings(&manifest[anchor]) {
                let optional_prefix = if optional { "optional:" } else { "" };
                let grant = if is_host_pattern(&name) {
                    Grant {
                        anchor,
                        suffix: format!("{optional_prefix}host:{name}"),
                        display: name.clone(),
                        actions: vec![],
                        resource_scopes: vec![name.clone()],
                        optional,
                    }
                } else {
                    Grant {
                        anchor,
                        suffix: format!("{optional_prefix}{name}"),
                        display: name.clone(),
                        actions: vec![name.clone()],
                        resource_scopes: vec![],
                        optional,
                    }
                };
                cx.grant(&mut out, &ext_id, grant);
            }
        }
        for (field, optional) in [
            ("host_permissions", false),
            ("optional_host_permissions", true),
        ] {
            for pattern in strings(&manifest[field]) {
                cx.grant(
                    &mut out,
                    &ext_id,
                    Grant {
                        anchor: field,
                        suffix: format!("host:{pattern}"),
                        display: pattern.clone(),
                        actions: vec![],
                        resource_scopes: vec![pattern.clone()],
                        optional,
                    },
                );
            }
        }
        // Externally-connectable boundaries are grants to outside pages.
        for pattern in strings(&manifest["externally_connectable"]["matches"]) {
            cx.grant(
                &mut out,
                &ext_id,
                Grant {
                    anchor: "externally_connectable",
                    suffix: format!("externally-connectable:{pattern}"),
                    display: pattern.clone(),
                    actions: vec!["externally_connectable".into()],
                    resource_scopes: vec![pattern.clone()],
                    optional: false,
                },
            );
        }
    }
    Ok((out, recognized))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MANIFEST: &str = r#"{
  "manifest_version": 3,
  "name": "Image Trail",
  "version": "0.10.1",
  "action": { "default_title": "Toggle Image Trail" },
  "commands": {
    "_execute_action": { "description": "Open or hide the panel" },
    "shortcut-download": { "description": "Download current image" }
  },
  "background": { "service_worker": "src/background/service-worker.js", "type": "module" },
  "content_scripts": [
    { "js": ["src/content/content-script.js"], "matches": ["https://*/*"] }
  ],
  "permissions": ["activeTab", "scripting", "storage"],
  "optional_host_permissions": ["http://*/*", "https://*/*"],
  "web_accessible_resources": [
    { "resources": ["src/ui/panel.css"], "matches": ["https://*/*"] }
  ]
}"#;

    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("extension");
        std::fs::create_dir_all(root.join("src/background")).unwrap();
        std::fs::create_dir_all(root.join("src/content")).unwrap();
        std::fs::write(root.join("manifest.json"), MANIFEST).unwrap();
        // Manifest points at built .js; only the .ts sources exist.
        std::fs::write(
            root.join("src/background/service-worker.ts"),
            "export function main(): void {}\n",
        )
        .unwrap();
        dir
    }

    fn extract(dir: &tempfile::TempDir) -> Extraction {
        let id = SourceId {
            repo: "local/image-trail",
            commit: "abc123",
        };
        let (out, recognized) = extract_manifests(dir.path(), &id, &BTreeSet::new()).unwrap();
        assert_eq!(recognized, 1);
        out
    }

    #[test]
    fn manifest_topology_is_deterministic_provenance_tagged_t0() {
        let dir = fixture();
        let out = extract(&dir);
        let ext = out
            .nodes
            .iter()
            .find(|node| node.label == "Extension")
            .expect("extension node");
        assert_eq!(ext.id, "ext:local/image-trail@extension");
        assert_eq!(ext.props["name"], "Image Trail");
        assert_eq!(ext.props["manifest_version"], 3);
        let prov: Provenance = serde_json::from_value(ext.props["prov"].clone()).unwrap();
        assert_eq!(prov.tier, Tier::Deterministic);
        assert_eq!(prov.confidence_tier, ConfidenceTier::Confirmed);
        assert_eq!(prov.extractor_id, EXTRACTOR_ID);
        assert_eq!(prov.evidence[0].path, "extension/manifest.json");
        assert!(prov.evidence[0].byte_end > prov.evidence[0].byte_start);

        // Contexts: service worker, content script, and the popupless action.
        let kinds: Vec<&str> = out
            .nodes
            .iter()
            .filter(|node| node.label == "ExtensionContext")
            .filter_map(|node| node.props["kind"].as_str())
            .collect();
        assert!(kinds.contains(&"service-worker"), "kinds: {kinds:?}");
        assert!(kinds.contains(&"content-script"));
        assert!(kinds.contains(&"action"));

        // Both declared commands exist and are DECLAREd by the extension.
        let commands = out
            .nodes
            .iter()
            .filter(|node| node.label == "Command")
            .count();
        assert_eq!(commands, 2);
        assert!(out.edges.iter().any(|edge| {
            edge.label == "DECLARES"
                && edge.dst == "extcmd:local/image-trail@extension:_execute_action"
        }));

        // Determinism: a second walk yields the same fact set.
        let again = extract(&dir);
        let ids = |ex: &Extraction| ex.nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(&out), ids(&again));
    }

    #[test]
    fn built_js_entries_bind_to_ts_sources_and_missing_entries_gap() {
        let dir = fixture();
        let out = extract(&dir);
        // service-worker.js does not exist; sibling .ts does — bound with
        // the fallback recorded, and a cited File node is emitted for it.
        let entry = out
            .edges
            .iter()
            .find(|edge| {
                edge.label == "ENTRY"
                    && edge.dst
                        == "file:local/image-trail@extension/src/background/service-worker.ts"
            })
            .expect("entry bound to ts source");
        assert_eq!(entry.props["resolved_from_build_path"], true);
        assert!(out.nodes.iter().any(|node| {
            node.label == "File"
                && node.id == "file:local/image-trail@extension/src/background/service-worker.ts"
        }));

        // content-script.js has no source at all: explicit Gap, never silent.
        let gap = out
            .nodes
            .iter()
            .find(|node| {
                node.label == "Gap"
                    && node.id
                        == "gap:webext:local/image-trail@extension/src/content/content-script.js"
            })
            .expect("missing entry is an explicit gap");
        assert_eq!(
            gap.props["reason"],
            "manifest entry not found in source tree"
        );
        let prov: Provenance = serde_json::from_value(gap.props["prov"].clone()).unwrap();
        assert_eq!(prov.confidence_tier, ConfidenceTier::Gap);
        assert!(out.edges.iter().any(|edge| {
            edge.label == "ENTRY"
                && edge.dst
                    == "gap:webext:local/image-trail@extension/src/content/content-script.js"
        }));
    }

    #[test]
    fn evidence_spans_cite_the_declaring_manifest_key() {
        // #148 review: a value repeated under two keys must cite its own
        // declaration — here the same pattern appears in content_scripts
        // matches first and optional_host_permissions later.
        let dir = tempfile::tempdir().unwrap();
        let manifest = r#"{
  "manifest_version": 3,
  "name": "x",
  "content_scripts": [{ "js": [], "matches": ["https://*/*"] }],
  "optional_host_permissions": ["https://*/*"]
}"#;
        std::fs::write(dir.path().join("manifest.json"), manifest).unwrap();
        let id = SourceId {
            repo: "local/x",
            commit: "abc123",
        };
        let (out, _) = extract_manifests(dir.path(), &id, &BTreeSet::new()).unwrap();
        let grant = out
            .edges
            .iter()
            .find(|edge| edge.label == "GRANTS")
            .expect("host grant");
        let prov: Provenance = serde_json::from_value(grant.props["prov"].clone()).unwrap();
        let declaration = manifest.find("optional_host_permissions").unwrap() as u64;
        assert!(
            prov.evidence[0].byte_start > declaration,
            "grant evidence ({}) must point past its declaring key ({declaration}), \
             not at the content-script match",
            prov.evidence[0].byte_start
        );
        let cited =
            &manifest[prov.evidence[0].byte_start as usize..prov.evidence[0].byte_end as usize];
        assert_eq!(cited, "https://*/*");
    }

    #[test]
    fn mv2_manifests_keep_toolbar_actions_and_optional_host_scopes() {
        // #148 review: MV2 `browser_action` without a popup is still a user
        // trigger, and MV2 optional host patterns live in
        // `optional_permissions` — they are scopes, never API actions.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("manifest.json"),
            r#"{
  "manifest_version": 2,
  "name": "legacy",
  "browser_action": { "default_title": "Toggle" },
  "optional_permissions": ["https://*/*", "downloads"]
}"#,
        )
        .unwrap();
        let id = SourceId {
            repo: "local/legacy",
            commit: "abc123",
        };
        let (out, _) = extract_manifests(dir.path(), &id, &BTreeSet::new()).unwrap();
        let action = out
            .nodes
            .iter()
            .find(|node| node.label == "ExtensionContext" && node.props["kind"] == "action")
            .expect("popupless browser_action is a context");
        assert_eq!(action.props["title"], "Toggle");

        let host = out
            .edges
            .iter()
            .find(|edge| edge.label == "GRANTS" && edge.dst.ends_with("host:https://*/*"))
            .expect("optional host grant");
        assert_eq!(host.props["resource_scopes"][0], "https://*/*");
        assert_eq!(host.props["actions"].as_array().unwrap().len(), 0);
        assert_eq!(host.props["optional"], true);

        let api = out
            .edges
            .iter()
            .find(|edge| edge.label == "GRANTS" && edge.dst.ends_with("optional:downloads"))
            .expect("optional api grant");
        assert_eq!(api.props["actions"][0], "downloads");
    }

    #[test]
    fn permissions_become_grants_with_exact_scopes() {
        let dir = fixture();
        let out = extract(&dir);
        // API permission: action named, no resource scope, not optional.
        let api = out
            .edges
            .iter()
            .find(|edge| {
                edge.label == "GRANTS" && edge.dst == "perm:local/image-trail@extension:activeTab"
            })
            .expect("api grant");
        assert_eq!(api.props["actions"][0], "activeTab");
        assert_eq!(api.props["optional"], false);

        // Optional wildcard host pattern keeps its exact scope — the
        // security projection reads `resource_scopes` and flags the `*`.
        let host = out
            .edges
            .iter()
            .find(|edge| {
                edge.label == "GRANTS"
                    && edge.dst == "perm:local/image-trail@extension:host:http://*/*"
            })
            .expect("host grant");
        assert_eq!(host.props["resource_scopes"][0], "http://*/*");
        assert_eq!(host.props["optional"], true);

        // Non-extension manifests (no manifest_version) emit nothing.
        let plain = tempfile::tempdir().unwrap();
        std::fs::write(
            plain.path().join("manifest.json"),
            r#"{ "name": "pwa", "start_url": "/" }"#,
        )
        .unwrap();
        let id = SourceId {
            repo: "local/pwa",
            commit: "abc123",
        };
        let (none, recognized) = extract_manifests(plain.path(), &id, &BTreeSet::new()).unwrap();
        assert!(none.nodes.is_empty());
        assert_eq!(recognized, 0);
    }
}
