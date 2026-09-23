# Jobs

Every recovery, escalation, and batch run is a durable job with stages,
progress, cancel, retry, and resume. **Clear finished** removes done, failed,
and cancelled rows, plus interrupted rows whose investigation has already
ended (it cannot resume). The investigation itself keeps its Interrupted or
Outcome-unknown status and history. Queued, running, and resumable
interrupted jobs and all graph facts are untouched.
