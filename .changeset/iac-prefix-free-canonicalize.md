---
"cartograph": patch
---

Terraform extraction now canonicalizes its root, module ancestors and module sources the same way the app does, without the Windows `\\?\` verbatim prefix, so the two sides compare one path form.
