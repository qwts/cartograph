---
'cartograph': patch
---

Third-party notices are now generated from `ui/package-lock.json` instead of
`npm ls`, so the npm inventory is stable across npm majors and environment
noise. A required CI gate regenerates the notices in memory and fails when
the committed copies drift, and the shipped notices file was refreshed for
packages whose pinned versions had moved (cytoscape, material-symbols,
zustand).
