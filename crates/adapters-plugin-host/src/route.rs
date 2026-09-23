//! Extraction routing for gated plugins (#201): walk a project tree, hand
//! every file the plugin claims (by golden-corpus extension) to
//! `extract-source`, and merge the pinned facts. Routing is deterministic
//! (sorted walk) and fails closed: one host error aborts the whole plugin
//! pass with zero partial facts (AC-0070) — the files stay uncovered
//! rather than half-covered.

use crate::{HostError, LoadedPlugin, PluginExtraction, PluginHost, PluginLimits, SourceId};
use std::path::{Path, PathBuf};

/// Directories never routed to plugins — preflight's skip list plus
/// `.cartograph` itself, so a plugin claiming `wasm`/`json` is never handed
/// its own artifact or golden corpus as project source (#208 review).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    ".git",
    "target",
    "dist",
    "build",
    ".venv",
    ".cartograph",
];

/// A routing failure: IO while walking/reading, or the plugin failing
/// closed on one named file.
#[derive(Debug)]
pub enum RouteError {
    /// Reading the tree or a claimed file failed.
    Io(std::io::Error),
    /// The plugin violated the SPI or its bounds on `path`.
    Host {
        /// Repo-relative path of the file the plugin failed on.
        path: String,
        /// The fail-closed host error.
        error: HostError,
    },
}

impl std::fmt::Display for RouteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(f, "plugin routing: {error}"),
            Self::Host { path, error } => {
                write!(f, "plugin failed closed on {path}: {error}")
            }
        }
    }
}

impl std::error::Error for RouteError {}

impl From<std::io::Error> for RouteError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

/// Repo-relative paths under `root` whose extension is claimed, in sorted
/// order so routing (and therefore the graph) is deterministic.
pub fn claimed_files(root: &Path, extensions: &[String]) -> std::io::Result<Vec<PathBuf>> {
    let skip = |name: &str| SKIP_DIRS.contains(&name);
    Ok(
        source_walk::files(root, &skip, source_walk::Gitignores::Honor)?
            .into_iter()
            .filter(|file| {
                file.path.extension().is_some_and(|extension| {
                    extensions.contains(&extension.to_string_lossy().into_owned())
                })
            })
            .map(|file| PathBuf::from(file.rel))
            .collect(),
    )
}

/// Run one gated plugin over every file it claims under `root`, merging
/// the (host-pinned) facts of each call. All-or-nothing per plugin: any
/// failure returns the error and no facts.
pub fn extract_claimed(
    host: &PluginHost,
    plugin: &LoadedPlugin,
    root: &Path,
    extensions: &[String],
    source_id: &SourceId,
    limits: PluginLimits,
) -> Result<PluginExtraction, RouteError> {
    let mut merged = PluginExtraction {
        nodes: Vec::new(),
        edges: Vec::new(),
    };
    for rel in claimed_files(root, extensions)? {
        let source = std::fs::read(root.join(&rel))?;
        let rel = rel.to_string_lossy().into_owned();
        let extraction = host
            .call_extract(plugin, &source, &rel, source_id, limits)
            .map_err(|error| RouteError::Host { path: rel, error })?;
        merged.nodes.extend(extraction.nodes);
        merged.edges.extend(extraction.edges);
    }
    Ok(merged)
}
