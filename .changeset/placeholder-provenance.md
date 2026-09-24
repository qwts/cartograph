---
"cartograph": minor
---

Imports of external packages no longer appear as system gaps. A clean spring-petclinic ingest previously reported 206 open findings, all of them imports such as `jakarta.persistence` or `org.springframework`. It now reports none. Every placeholder node now records the import or reference that created it and the reason. A package the repository provably cannot provide is recorded as a confirmed external dependency. A package the repository declares is recorded as internal. An in-repo target that fails to resolve stays an explicit gap and names its cause. Java and Kotlin imports of in-repo types now link to the file that declares the type.
