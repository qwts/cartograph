---
"cartograph": patch
---

Changing the local path while a preflight is still scanning (for example after going Back mid-scan) now stops that scan, as switching the source away from a local tree already did. The abandoned scan no longer keeps running in the background or records findings for the path you left, and its late progress or result can't reach the Preflight screen.
