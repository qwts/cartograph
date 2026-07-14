//! GitHub repo ingest (SPEC-00 §10, US-0001): auth ladder, shallow
//! read-only clone, real repo identity.
//!
//! The auth ladder for v1 (ADR-0009): an explicit token from
//! `GH_TOKEN`/`GITHUB_TOKEN`, else a `gh auth token` shell-out for
//! environments already authenticated. Clones are `git2` shallow
//! (depth 1), land in a temp directory and only move into place on
//! success — an auth failure leaves **no partial clone** (AC-0003) and
//! maps to a typed error carrying remediation text.

pub mod manifest;
pub mod preflight;

use std::path::{Path, PathBuf};
use std::process::Command;

/// Ingest errors. `Auth` carries remediation (AC-0003).
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// Authentication/authorization failure — no partial clone remains.
    #[error("auth failure cloning {url}: {message}. {remediation}")]
    Auth {
        /// The URL that failed.
        url: String,
        /// Underlying git message.
        message: String,
        /// What the user can do about it.
        remediation: String,
    },
    /// The URL is not a recognized GitHub (or file://) repo reference.
    #[error("unrecognized repo URL: {0}")]
    InvalidUrl(String),
    /// Any other git failure.
    #[error("git: {0}")]
    Git(#[from] git2::Error),
    /// Filesystem failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// A completed read-only clone.
#[derive(Debug, Clone)]
pub struct ClonedRepo {
    /// Repo identity (`owner/name`, or `local/<name>` for file:// URLs).
    pub repo: String,
    /// Resolved HEAD commit SHA (AC-0001: listed with commit SHA).
    pub commit_sha: String,
    /// Where the working tree lives.
    pub path: PathBuf,
}

/// Discover a token from the v1 auth ladder: `GH_TOKEN`, `GITHUB_TOKEN`,
/// then `gh auth token` (ADR-0009). `None` means anonymous (public repos).
pub fn discover_token() -> Option<String> {
    for var in ["GH_TOKEN", "GITHUB_TOKEN"] {
        if let Ok(t) = std::env::var(var)
            && !t.trim().is_empty()
        {
            return Some(t.trim().to_string());
        }
    }
    let out = Command::new("gh").args(["auth", "token"]).output().ok()?;
    if out.status.success() {
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !t.is_empty() {
            return Some(t);
        }
    }
    None
}

/// Parse a repo reference into `(identity, clone_url)`. Accepts
/// `https://github.com/o/n(.git)`, `git@github.com:o/n(.git)`, bare
/// `o/n` shorthand, and `file://` URLs (offline tests, local mirrors).
pub fn parse_repo_url(url: &str) -> Result<(String, String), IngestError> {
    let url = url.trim().trim_end_matches('/');
    if let Some(rest) = url.strip_prefix("file://") {
        let name = rest.rsplit('/').next().filter(|s| !s.is_empty());
        let name = name.ok_or_else(|| IngestError::InvalidUrl(url.into()))?;
        let name = name.strip_suffix(".git").unwrap_or(name);
        return Ok((format!("local/{name}"), url.to_string()));
    }
    let path = if let Some(rest) = url.strip_prefix("https://github.com/") {
        Some(rest)
    } else if let Some(rest) = url.strip_prefix("git@github.com:") {
        Some(rest)
    } else if url.split('/').count() == 2 && !url.contains(':') && !url.contains(' ') {
        Some(url)
    } else {
        None
    };
    let path = path.ok_or_else(|| IngestError::InvalidUrl(url.into()))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(owner), Some(name), None) if !owner.is_empty() && !name.is_empty() => Ok((
            format!("{owner}/{name}"),
            format!("https://github.com/{owner}/{name}.git"),
        )),
        _ => Err(IngestError::InvalidUrl(url.into())),
    }
}

/// Classify a git2 error: auth-shaped failures become [`IngestError::Auth`]
/// with remediation (AC-0003); anything else passes through.
fn classify_git_error(url: &str, e: git2::Error) -> IngestError {
    let msg = e.message().to_ascii_lowercase();
    let authish = matches!(
        e.class(),
        git2::ErrorClass::Http | git2::ErrorClass::Ssh | git2::ErrorClass::Callback
    ) && (msg.contains("auth")
        || msg.contains("401")
        || msg.contains("403")
        || msg.contains("credential")
        || msg.contains("permission"));
    if authish {
        IngestError::Auth {
            url: url.to_string(),
            message: e.message().to_string(),
            remediation: "Check access to the repo, then set GH_TOKEN (or GITHUB_TOKEN) \
                          to a token with repo read scope, or run `gh auth login`."
                .to_string(),
        }
    } else {
        IngestError::Git(e)
    }
}

/// Shallow-clone `url` under `dest_root`, returning the repo identity and
/// HEAD SHA. The clone lands in a temp directory and moves into place only
/// on success — failures leave no partial clone (AC-0003). Re-adding a
/// repo replaces its previous clone (v1 is one-shot ingest per SPEC §10).
pub fn clone_repo(
    url: &str,
    dest_root: &Path,
    token: Option<&str>,
) -> Result<ClonedRepo, IngestError> {
    let (identity, clone_url) = parse_repo_url(url)?;
    std::fs::create_dir_all(dest_root)?;
    let final_dir = dest_root.join(identity.replace('/', "__"));
    let tmp_dir = dest_root.join(format!(".tmp-{}", identity.replace('/', "__")));
    if tmp_dir.exists() {
        std::fs::remove_dir_all(&tmp_dir)?;
    }

    let mut callbacks = git2::RemoteCallbacks::new();
    if let Some(token) = token {
        let token = token.to_string();
        callbacks.credentials(move |_url, _user, _kinds| {
            // GitHub accepts a token as the password with any username.
            git2::Cred::userpass_plaintext("x-access-token", &token)
        });
    }
    let mut fetch = git2::FetchOptions::new();
    fetch.remote_callbacks(callbacks);
    if !clone_url.starts_with("file://") {
        fetch.depth(1); // shallow per SPEC §10 (local transport can't)
    }

    let result = git2::build::RepoBuilder::new()
        .fetch_options(fetch)
        .clone(&clone_url, &tmp_dir);
    let repo = match result {
        Ok(repo) => repo,
        Err(e) => {
            // No partial clone: whatever git2 left behind goes away.
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(classify_git_error(url, e));
        }
    };
    let commit_sha = repo
        .head()
        .and_then(|h| h.peel_to_commit())
        .map(|c| c.id().to_string())
        .map_err(|e| {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            classify_git_error(url, e)
        })?;
    drop(repo);

    if final_dir.exists() {
        std::fs::remove_dir_all(&final_dir)?;
    }
    std::fs::rename(&tmp_dir, &final_dir)?;
    Ok(ClonedRepo {
        repo: identity,
        commit_sha,
        path: final_dir,
    })
}

#[cfg(test)]
mod tests;
