//! The topology manifest — `cartograph.system.toml` (SPEC-00 §10,
//! US-0001 AC-0002): an author-editable declaration of which repos form
//! one system, per-repo layer hints, and known channel identities.
//! Everything in it is Confirmed-by-author knowledge, applied
//! deterministically at ingest; T2 may *suggest* additions later (M7),
//! always user-confirmed.

use std::collections::BTreeMap;
use std::path::Path;

/// The canonical manifest file name.
pub const MANIFEST_NAME: &str = "cartograph.system.toml";

/// Manifest errors.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// TOML syntax/shape failure — the message names the field.
    #[error("manifest parse: {0}")]
    Parse(#[from] toml::de::Error),
    /// A repo entry names a layer outside the five-layer model.
    #[error("unknown layer \"{0}\" (expected infra, cloud, server, events, client)")]
    UnknownLayer(String),
}

/// The five layers of SPEC-00 §3; hints gate which extractors run.
pub const LAYERS: &[&str] = &["infra", "cloud", "server", "events", "client"];

/// One repo in the system set.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ManifestRepo {
    /// GitHub URL / `owner/name` / `file://` / local path.
    pub url: String,
    /// Layer hints; empty means "extract everything".
    #[serde(default)]
    pub layers: Vec<String>,
    /// Path to `terraform show -json` output (state or plan) for this
    /// repo's infra, relative to the manifest. Observed values feed T1
    /// enrichment (SPEC-00 §3.1, AC-0009); absent means T0 only.
    #[serde(default)]
    pub state_json: Option<String>,
}

/// The parsed manifest.
#[derive(Debug, Clone, Default, serde::Deserialize)]
pub struct SystemManifest {
    /// The repo set forming one analyzable system.
    #[serde(default)]
    pub repos: Vec<ManifestRepo>,
    /// Known identities: key → value, applied to the config resolver ahead
    /// of `.env` files (author-declared, e.g. a queue URL the code reads
    /// from an env var no checked-in file defines).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

impl SystemManifest {
    /// Load and validate a manifest from a file path (or a directory
    /// containing `cartograph.system.toml`).
    pub fn load(path: &Path) -> Result<Self, ManifestError> {
        let file = if path.is_dir() {
            path.join(MANIFEST_NAME)
        } else {
            path.to_path_buf()
        };
        let text = std::fs::read_to_string(&file)?;
        let manifest: SystemManifest = toml::from_str(&text)?;
        for repo in &manifest.repos {
            for layer in &repo.layers {
                if !LAYERS.contains(&layer.as_str()) {
                    return Err(ManifestError::UnknownLayer(layer.clone()));
                }
            }
        }
        Ok(manifest)
    }
}
