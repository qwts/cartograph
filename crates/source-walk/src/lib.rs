//! The one source-tree walk every extractor and Preflight share (#248,
//! ADR-0030).
//!
//! What counts as "the system's source" must not depend on which extractor
//! asks: a `build/`, `out/` or `vendor/` tree the repository `.gitignore`s is
//! generated or third-party, and ingesting it produces wrong facts. Every
//! walker therefore goes through [`files`], which applies the tree's own
//! `.gitignore` files on top of the caller's fixed directory skips (kept as the
//! fallback for trees without ignore rules), refuses symlinks, and returns
//! repo-relative `/`-separated paths in sorted order (US-0014).
//!
//! Only `.gitignore` files **inside** the walked root are honored — never
//! ancestors, `.git/info/exclude`, or the user's global excludes file. Those
//! are machine-local, and honoring them would let two machines ingesting the
//! same commit disagree about the graph. `.gitignore` applies with or without
//! a `.git` directory, so a checkout, a plain copy, and a captured tree agree.
//!
//! The captured lane walks through confined directory handles instead of
//! `std::fs`; it shares the matching itself through [`IgnoreRules`], so both
//! lanes select the same files by construction.

pub mod parallel;

use ignore::Match;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The per-directory ignore file honored by every walk.
pub const GITIGNORE: &str = ".gitignore";

/// Largest `.gitignore` read; a bigger one fails the walk rather than being
/// truncated into a different rule set.
pub const MAX_GITIGNORE_BYTES: u64 = 1024 * 1024;

/// The `.gitignore` rules in force for one directory: its own file plus every
/// enclosing one up to the walk root, innermost deciding (git's precedence).
#[derive(Clone, Default)]
pub struct IgnoreRules {
    layers: Vec<Arc<Gitignore>>,
}

impl IgnoreRules {
    /// The rules for the child directory `rel_dir` (repo-relative,
    /// `/`-separated; empty for the root) whose `.gitignore` holds
    /// `gitignore`, if any. Lines that are not valid patterns are skipped,
    /// as git does.
    #[must_use]
    pub fn descend(&self, rel_dir: &str, gitignore: Option<&[u8]>) -> Self {
        let mut rules = self.clone();
        let Some(bytes) = gitignore else {
            return rules;
        };
        let mut builder = GitignoreBuilder::new(if rel_dir.is_empty() { "." } else { rel_dir });
        let text = String::from_utf8_lossy(bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        for line in text.lines() {
            let _ = builder.add_line(None, line);
        }
        if let Ok(matcher) = builder.build()
            && !matcher.is_empty()
        {
            rules.layers.push(Arc::new(matcher));
        }
        rules
    }

    /// Whether the entry at `rel_path` (repo-relative, `/`-separated, inside
    /// the directory these rules belong to) is ignored.
    #[must_use]
    pub fn is_ignored(&self, rel_path: &str, is_dir: bool) -> bool {
        for layer in self.layers.iter().rev() {
            match layer.matched(rel_path, is_dir) {
                Match::Ignore(_) => return true,
                Match::Whitelist(_) => return false,
                Match::None => {}
            }
        }
        false
    }
}

/// Whether a walk applies `.gitignore` rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gitignores {
    /// Skip everything the tree's `.gitignore` files ignore (source walks).
    Honor,
    /// Apply only the caller's directory skips — for inputs that are
    /// conventionally ignored yet deliberately read (local `.env` files).
    Disregard,
}

/// One regular file found by [`files`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkedFile {
    /// Repo-relative, `/`-separated path.
    pub rel: String,
    /// The file's path on disk (`root` joined with `rel`).
    pub path: PathBuf,
    /// The file name alone.
    pub name: String,
}

/// Every regular file under `root`, sorted by repo-relative path.
///
/// Directories whose name `skip_dir` accepts are not entered (the root itself
/// is never tested). Symlinks are skipped, never followed, so a walk cannot
/// leave `root` or loop. With [`Gitignores::Honor`], entries the tree's
/// `.gitignore` files ignore are skipped as well.
pub fn files(
    root: &Path,
    skip_dir: &dyn Fn(&str) -> bool,
    gitignores: Gitignores,
) -> std::io::Result<Vec<WalkedFile>> {
    let mut out = Vec::new();
    visit(
        root,
        "",
        &IgnoreRules::default(),
        skip_dir,
        gitignores,
        &mut out,
    )?;
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

/// Read one directory's `.gitignore` within [`MAX_GITIGNORE_BYTES`].
pub fn read_gitignore(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .by_ref()
        .take(MAX_GITIGNORE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_GITIGNORE_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "a .gitignore exceeds the supported size",
        ));
    }
    Ok(bytes)
}

fn visit(
    dir: &Path,
    rel_dir: &str,
    parent: &IgnoreRules,
    skip_dir: &dyn Fn(&str) -> bool,
    gitignores: Gitignores,
    out: &mut Vec<WalkedFile>,
) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let rules = match gitignores {
        Gitignores::Honor => {
            let own = entries
                .iter()
                .find(|entry| entry.file_name() == GITIGNORE)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .map(|entry| read_gitignore(std::fs::File::open(entry.path())?))
                .transpose()?;
            parent.descend(rel_dir, own.as_deref())
        }
        Gitignores::Disregard => IgnoreRules::default(),
    };
    for entry in entries {
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let rel = if rel_dir.is_empty() {
            name.clone()
        } else {
            format!("{rel_dir}/{name}")
        };
        if kind.is_dir() {
            if skip_dir(&name) || rules.is_ignored(&rel, true) {
                continue;
            }
            visit(&entry.path(), &rel, &rules, skip_dir, gitignores, out)?;
        } else if kind.is_file() && !rules.is_ignored(&rel, false) {
            out.push(WalkedFile {
                rel,
                path: entry.path(),
                name,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
