---
'cartograph': minor
---

Jobs surface hygiene: the dev-only "Enqueue test job" control is gone from the production UI (with its `enqueue_job` command), and a confirm-gated **Clear finished** action removes done/failed/cancelled jobs from the durable spine while queued, running, and interrupted (resumable) work is always kept.
