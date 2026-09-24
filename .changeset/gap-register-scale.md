---
"cartograph": patch
---

The exported Gap register stays readable on large systems. Past 12 gaps, `gap_register.md` groups gaps by cause (stop reason × extractor, largest first) and shows up to 5 representative instances for each of at most 50 classes. Every omitted instance or class is stated as a counted "N more" line. A new `gap_register.json` sidecar lists every instance by class, and each instance keeps its full provenance in the bundle. On a VSCode checkout the Markdown drops from 105 MB to under 100 KB.
