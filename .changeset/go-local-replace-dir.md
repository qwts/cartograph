---
"cartograph": patch
---

Go imports into a module that a `go.mod` locally replaces (`replace example.com/shared => ./shared`) now resolve to the repository's own package directory and are shown as Confirmed internal instead of Gaps. A replacement whose target leaves the repository, including through a symlink, stays a Gap.
