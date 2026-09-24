//! JVM package boundary shared by the Java and Kotlin adapters (#237,
//! ADR-0031): which import paths lie inside the recovered system, and how
//! an import target no declaration resolved is classified.

use core_graph::placeholder::Boundary;
use std::collections::BTreeSet;
use std::io::Read;
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
/// beside a declared `a.b.c`) cannot be proven either way and stays a Gap,
/// as does every undeclared import when `packages_complete` is false.
pub fn classify_import(
    module: &str,
    repo_packages: &BTreeSet<String>,
    packages_complete: bool,
) -> Boundary {
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
    if !packages_complete {
        return Boundary::Unresolved {
            reason: "import not provable external: a repository source header is unreadable".into(),
        };
    }
    Boundary::External {
        reason: "package not declared in this repository".into(),
        evidence: vec![],
    }
}

/// Package declarations of the other JVM language's sources beneath `root`
/// (the same `.gitignore`-aware walk and skip set as the Java walk).
#[derive(Debug, Default)]
pub struct ForeignPackages {
    /// Every package a header scan found.
    pub packages: Vec<String>,
    /// False when some source's header could not be read or parsed: its
    /// package is unknown, so no import can be proven external (fail closed).
    pub complete: bool,
}

/// Largest header prefix read per file; a header still undecided after it
/// is unknown.
const HEADER_BYTES: u64 = 64 * 1024;

/// Scan the headers of the other JVM language's sources beneath `root`.
/// Over-inclusion only turns an external classification into a Gap, and an
/// unreadable or unparseable header marks the scan incomplete — fail closed.
pub fn foreign_packages(root: &Path, extensions: &[&str]) -> std::io::Result<ForeignPackages> {
    let skip = |name: &str| {
        name.starts_with('.')
            || matches!(
                name,
                "target" | "build" | "out" | "node_modules" | "dist" | "generated"
            )
    };
    let mut out = ForeignPackages {
        packages: Vec::new(),
        complete: true,
    };
    for file in source_walk::files(root, &skip, source_walk::Gitignores::Honor)? {
        if !file
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extensions.contains(&extension))
        {
            continue;
        }
        // Only the header is read, never more than `HEADER_BYTES`.
        let mut header = Vec::new();
        let read = std::fs::File::open(&file.path)
            .and_then(|handle| handle.take(HEADER_BYTES).read_to_end(&mut header));
        let truncated = header.len() as u64 >= HEADER_BYTES;
        match (read, package_header(&String::from_utf8_lossy(&header))) {
            (Ok(_), Header::Package(name)) => out.packages.push(name),
            (Ok(_), Header::Default) if !truncated => {}
            _ => out.complete = false,
        }
    }
    Ok(out)
}

/// What a JVM source's header declares.
#[derive(Debug, PartialEq, Eq)]
pub enum Header {
    /// `package a.b.c`.
    Package(String),
    /// No package clause before the first import or declaration.
    Default,
    /// The header could not be parsed (e.g. an unbalanced annotation).
    Unknown,
}

/// The package clause of a JVM source. Skips a leading `#!` line, comments,
/// and annotations — file annotations included, with arguments that may
/// span lines (`@file:JvmName(\n"X"\n)`, `@file:[A B]`) and hold string
/// literals.
pub fn package_header(text: &str) -> Header {
    let bytes = text.as_bytes();
    let mut at = 0;
    if text.starts_with("#!") {
        at = text.find('\n').unwrap_or(text.len());
    }
    loop {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let rest = &text[at..];
        if rest.is_empty() {
            return Header::Default;
        }
        if rest.starts_with("//") {
            at += rest.find('\n').unwrap_or(rest.len());
        } else if rest.starts_with("/*") {
            let Some(end) = rest.find("*/") else {
                return Header::Unknown;
            };
            at += end + 2;
        } else if rest.starts_with('@') {
            // `@`, then a (possibly `file:`-targeted, dotted) name.
            at += 1;
            while at < bytes.len()
                && (bytes[at].is_ascii_alphanumeric()
                    || matches!(bytes[at], b'_' | b'.' | b':' | b'`'))
            {
                at += 1;
            }
            let mut probe = at;
            while probe < bytes.len() && bytes[probe].is_ascii_whitespace() {
                probe += 1;
            }
            if probe < bytes.len() && matches!(bytes[probe], b'(' | b'[') {
                match balanced_end(text, probe) {
                    Some(end) => at = end,
                    None => return Header::Unknown,
                }
            }
        } else {
            let word: &str = rest
                .split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
                .next()
                .unwrap_or("");
            if word == "package" {
                let name: String = rest["package".len()..]
                    .trim_start()
                    .chars()
                    .take_while(|ch| {
                        ch.is_alphanumeric() || matches!(ch, '.' | '_' | '`' | ' ' | '\t')
                    })
                    .filter(|ch| !matches!(ch, '`' | ' ' | '\t'))
                    .collect();
                return if name.is_empty() {
                    Header::Unknown
                } else {
                    Header::Package(name)
                };
            }
            return if word.is_empty() {
                Header::Unknown
            } else {
                Header::Default
            };
        }
    }
}

/// The byte just past the bracket group opening at `open`, skipping nested
/// groups, comments, and string/char literals; `None` if it never closes.
fn balanced_end(text: &str, open: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut at = open;
    while at < bytes.len() {
        let rest = &text[at..];
        if let Some(body) = rest.strip_prefix("\"\"\"") {
            at += 3 + body.find("\"\"\"")? + 3;
            continue;
        }
        match bytes[at] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at + 1);
                }
            }
            quote @ (b'"' | b'\'') => {
                at += 1;
                while at < bytes.len() && bytes[at] != quote {
                    if bytes[at] == b'\\' {
                        at += 1;
                    }
                    at += 1;
                }
                if at >= bytes.len() {
                    return None;
                }
            }
            b'/' if rest.starts_with("//") => {
                at += rest.find('\n')?;
                continue;
            }
            b'/' if rest.starts_with("/*") => {
                at += rest.find("*/")? + 2;
                continue;
            }
            _ => {}
        }
        at += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiline_file_annotations_do_not_hide_the_package() {
        let header = "/* license */\n@file:JvmName(\n    \"Util\"\n)\n@file:[Suppress(\"a)\")\n JvmMultifileClass]\n\npackage com.demo.kt\n\nclass X\n";
        assert_eq!(
            package_header(header),
            Header::Package("com.demo.kt".into())
        );
        assert_eq!(
            package_header("@Deprecated\npackage a.b;\n"),
            Header::Package("a.b".into())
        );
        assert_eq!(package_header("import x.Y;\nclass A {}"), Header::Default);
        assert_eq!(package_header(""), Header::Default);
    }

    #[test]
    fn an_unparseable_header_is_unknown() {
        assert_eq!(package_header("@file:JvmName(\n\"X\"\n"), Header::Unknown);
        assert_eq!(package_header("/* never closed"), Header::Unknown);
        assert_eq!(package_header("package ;"), Header::Unknown);
    }
}
