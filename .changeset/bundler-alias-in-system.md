---
"cartograph": patch
---

Bare JS/TS imports that match a literal Vite or webpack `resolve.alias` key are no longer classified as external packages. Cartograph reads the bundler config as data, without running it. When the alias's literal path points at a file in the repository, the import resolves to that file and cites the config. Otherwise the import stays an explicit gap that names the config. Aliases whose key or target isn't a literal (and aliases that map one package name to another) are classified as before.
