---
"cartograph": minor
---

Preserve live job ownership across app startup and cancellation, fence worker updates to their own attempt, and prevent retries while prior work is still stopping. Earlier jobs without execution ownership records remain historical and require a fresh operation.

Windows job execution requires NTFS for private application storage and verifies file and directory flushes before claiming an attempt. This storage requirement does not restrict the repositories Cartograph can analyze.
