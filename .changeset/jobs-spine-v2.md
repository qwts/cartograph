---
'cartograph': minor
---

Job spine v2: jobs carry stage, percent progress, failure detail, and
artifact links; new cancel/retry commands cover the full lifecycle
(cancel is cooperative at stage boundaries, retry also resumes jobs
interrupted by an app restart); ingest runs as a staged job emitting
live `job://changed` events. Existing state-spine databases migrate in
place.
