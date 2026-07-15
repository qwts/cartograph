---
'cartograph': patch
---

Recovery no longer freezes the app: ingest, add-repo, add-system, and ingest retry/resume commands now run their extraction on a blocking worker thread instead of the webview/main thread, so a large repository recovers with the UI fully interactive (no more macOS beachball).
