//! JVM package boundary shared by the Java and Kotlin adapters (#237,
//! ADR-0031): which import paths lie inside the recovered system, and how
//! an import target no declaration resolved is classified.

use core_graph::placeholder::Boundary;
use std::collections::BTreeSet;
use std::path::Path;

/// Every dotted prefix of `path`, longest first (`a.b.C` → `a.b.C`, `a.b`, `a`).
pub fn dotted_prefixes(path: &str) -> impl Iterator<Item = &str> {
    std::iter::once(path).chain(path.rmatch_indices('.').map(move |(dot, _)| &path[..dot]))
}

/// A path lies inside the system when any dotted prefix is a package this
/// repository declares — `a.b.C`, a static `a.b.C.m`, a nested `a.b.O.I`,
/// and a generated `a.b.gen.X` beneath a declared `a.b` alike.
pub fn in_system(path: &str, repo_packages: &BTreeSet<String>) -> bool {
    dotted_prefixes(path).any(|prefix| repo_packages.contains(prefix))
}

/// Classify an import target no declaration resolved (#237, ADR-0031): a
/// declared package is an internal boundary, anything else in the system is
/// an explicit Gap, and only a package the repository provably does not
/// declare is external. An enclosing package of a declared one (`a.b.X`
/// beside a declared `a.b.c`) cannot be proven either way and stays a Gap.
pub fn classify_import(module: &str, repo_packages: &BTreeSet<String>) -> Boundary {
    if repo_packages.contains(module) {
        return Boundary::Internal {
            reason: "package declared in this repository".into(),
            evidence: vec![],
        };
    }
    let parent = module.rsplit_once('.').map_or("", |(parent, _)| parent);
    let encloses_repo_package = !parent.is_empty()
        && repo_packages
            .iter()
            .any(|package| package.starts_with(&format!("{parent}.")));
    if in_system(module, repo_packages) || encloses_repo_package {
        return Boundary::Unresolved {
            reason: "import of a repository package with no unique declaration".into(),
        };
    }
    Boundary::External {
        reason: "package not declared in this repository".into(),
        evidence: vec![],
    }
}

/// Package declarations of the other JVM language's sources beneath `root`
/// (the same `.gitignore`-aware walk and skip set as the Java walk). A light
/// header scan: over-inclusion only turns an external classification into a
/// Gap — fail closed.
pub fn foreign_packages(root: &Path, extensions: &[&str]) -> std::io::Result<Vec<String>> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(
                name,
                "target" | "build" | "out" | "node_modules" | "dist" | "generated"
            )
    };
    let mut out = Vec::new();
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        if !file
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extensions.contains(&extension))
        {
            continue;
        }
        // Only the header is read: the scan stops at the first line that is
        // neither a comment nor a file annotation.
        let Ok(handle) = std::fs::File::open(&file.path) else {
            continue;
        };
        let lines = std::io::BufRead::lines(std::io::BufReader::new(handle));
        out.extend(package_header(lines.map_while(Result::ok)));
    }
    Ok(out)
}

/// The `package a.b.c` header of a JVM source, skipping leading comments
/// and file annotations; `None` once any other declaration starts.
pub fn package_header(lines: impl IntoIterator<Item = impl AsRef<str>>) -> Option<String> {
    let mut in_block = false;
    for line in lines {
        let line = line.as_ref().trim();
        if in_block {
            in_block = !line.contains("*/");
            continue;
        }
        if line.is_empty() || line.starts_with("//") || line.starts_with("#!") {
            continue;
        }
        if line.starts_with("/*") {
            in_block = !line.contains("*/");
            continue;
        }
        if line.starts_with('@') {
            continue;
        }
        let name = line.strip_prefix("package ")?;
        let name: String = name
            .trim()
            .chars()
            .take_while(|ch| ch.is_alphanumeric() || matches!(ch, '.' | '_' | '`'))
            .filter(|ch| *ch != '`')
            .collect();
        return (!name.is_empty()).then_some(name);
    }
    None
}
