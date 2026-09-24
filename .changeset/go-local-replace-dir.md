---
"cartograph": patch
---

Go imports into a module that a `go.mod` locally replaces (`replace example.com/shared => ./shared`) now resolve to the repository's own package directory and are shown as Confirmed internal instead of Gaps. An import stays a Gap when its package directory or Go file leaves the repository (including through a symlink), or when the module has a version-qualified `replace` (`replace m v1.0.0 => …`), because the version in use is unknown.
