//! Plugin discovery and per-project lifecycle (#198, AC-0068).
//!
//! Two discovery roots: the project-local `.cartograph/adapters/` and a
//! user-level directory. The project copy wins on adapter-id conflict — a
//! repo can pin the exact artifact it was analyzed with. Artifacts are
//! keyed by BLAKE3 content hash (the same primitive provenance uses), so
//! "same id, different bytes" is always visible.

use core_prov::content_hash;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where a discovered artifact came from; project beats user on conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginScope {
    /// `.cartograph/adapters/` inside the analyzed tree.
    Project,
    /// The user-level adapters directory (app data).
    User,
}

/// One discovered plugin artifact. Discovery reads bytes only to hash
/// them — no wasm is compiled or executed here.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DiscoveredPlugin {
    /// Adapter id: the artifact's file stem (e.g. `t0.adapter-ruby`).
    pub id: String,
    /// Absolute path of the winning artifact.
    pub path: PathBuf,
    /// BLAKE3 hash of the artifact bytes — the version key (AC-0069).
    pub content_hash: String,
    /// Which root supplied the winning artifact.
    pub scope: PluginScope,
    /// True when a user-level artifact with the same id was shadowed.
    pub shadowed_user_copy: bool,
}

/// The project-relative discovery root.
pub const PROJECT_ADAPTER_DIR: &str = ".cartograph/adapters";

fn scan(dir: &Path, scope: PluginScope) -> Vec<DiscoveredPlugin> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("wasm") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        found.push(DiscoveredPlugin {
            id: stem.to_string(),
            path,
            content_hash: content_hash(&bytes),
            scope,
            shadowed_user_copy: false,
        });
    }
    found
}

/// Discover plugin artifacts under `project_root/.cartograph/adapters/`
/// and `user_dir`, project winning on id conflict. Deterministic: sorted
/// by id; a shadowed user copy is recorded on the winner, never dropped
/// silently.
pub fn discover(project_root: Option<&Path>, user_dir: &Path) -> Vec<DiscoveredPlugin> {
    let mut by_id: BTreeMap<String, DiscoveredPlugin> = BTreeMap::new();
    for plugin in scan(user_dir, PluginScope::User) {
        by_id.insert(plugin.id.clone(), plugin);
    }
    if let Some(root) = project_root {
        for plugin in scan(&root.join(PROJECT_ADAPTER_DIR), PluginScope::Project) {
            let shadowed = by_id.contains_key(&plugin.id);
            by_id.insert(
                plugin.id.clone(),
                DiscoveredPlugin {
                    shadowed_user_copy: shadowed,
                    ..plugin
                },
            );
        }
    }
    by_id.into_values().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, bytes: &[u8]) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    #[test]
    fn project_wins_on_id_conflict_and_hashes_version_artifacts() {
        // AC-0068 (#198): both roots scanned, documented precedence on id
        // conflict, artifacts keyed by content hash, deterministic order.
        let project = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        let adapters = project.path().join(PROJECT_ADAPTER_DIR);
        write(&adapters, "t0.adapter-ruby.wasm", b"project ruby build");
        write(&adapters, "t0.adapter-swift.wasm", b"swift build");
        write(user.path(), "t0.adapter-ruby.wasm", b"user ruby build");
        write(user.path(), "t0.adapter-kotlin.wasm", b"kotlin build");
        write(user.path(), "notes.txt", b"not a plugin");

        let plugins = discover(Some(project.path()), user.path());
        let ids: Vec<&str> = plugins.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["t0.adapter-kotlin", "t0.adapter-ruby", "t0.adapter-swift"]
        );

        let ruby = &plugins[1];
        assert_eq!(ruby.scope, PluginScope::Project);
        assert!(ruby.shadowed_user_copy);
        assert_eq!(ruby.content_hash, content_hash(b"project ruby build"));
        assert_ne!(ruby.content_hash, content_hash(b"user ruby build"));

        let kotlin = &plugins[0];
        assert_eq!(kotlin.scope, PluginScope::User);
        assert!(!kotlin.shadowed_user_copy);

        // No project root: user artifacts stand alone.
        let user_only = discover(None, user.path());
        assert_eq!(user_only.len(), 2);

        // Missing directories are empty discoveries, never errors.
        let no_user = discover(Some(project.path()), Path::new("/nonexistent"));
        assert_eq!(no_user.len(), 2);
        assert!(no_user.iter().all(|p| p.scope == PluginScope::Project));
    }
}
