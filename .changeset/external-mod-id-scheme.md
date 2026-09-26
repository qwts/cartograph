---
"cartograph": patch
---

A Java or Kotlin import proven external now keys its `Module` node and `IMPORTS` edge on the imported package (`mod:jakarta.persistence`) instead of the imported type or static member (`mod:jakarta.persistence.Entity`), so multiple types imported from the same external package share one node instead of one each. The edge still records exactly what was imported. A Kotlin top-level function import with no capitalized package/type boundary (for example `kotlinx.coroutines.launch`) is unaffected and keeps its full path, since there is no boundary to split on. This is a graph-contract change; existing graphs are cleared and rebuilt on the next recovery.
