# ADR-0025 — Execution ownership and attempt-fenced job transitions

- **Status:** Accepted for the staged fix in #393
- **Date:** 2026-09-10
- **Deciders:** Cartograph owner direction; implementation agent within the durable job contract

## Context

Startup currently interrupts every running row without knowing whether another
process still owns it. Cancel/retry can reuse a job ID while its earlier worker
continues, and ID-only updates can complete the wrong attempt. Source locks do not
cover all job kinds or provide job liveness. Blocking work also outlives an aborted
async waiter, and completed agent results must remain reviewable after cancellation.

## Decision

Use a private versioned execution namespace, monotonically increasing attempts and
opaque owners alongside existing job rows. Reserve persistent per-job OS locks
outside database mutexes, claim attempts transactionally and carry an opaque shared
execution guard through every worker and blocking closure. Fence worker transitions
by the exact attempt; guard user cancellation and retry atomically. Recovery may
interrupt only an unchanged recorded running candidate whose OS lock it acquired.

Keep ownerless legacy rows unchanged and explicitly unknown. Do not reuse their
job IDs; require fresh operations instead of inferring death or offering forced
takeover. Preserve stable job IDs for tracked retries and all proposal/review
identity. Add a tracking-state response field without claiming actual liveness.
An already-started model call retains the right to stage its completed proposal
after the cancelled job row is cleared, provided its execution guard remains
valid; missing history stops further work and all lifecycle writes.

## Consequences

A second process cannot steal a participating live job, and an old attempt cannot
update a newer one. Cancellation remains cooperative and busy workers exclude
retry until they exit. No database lock spans an OS lock attempt or long work.
Persistent lock files and private metadata add storage; earlier job rows require
fresh recovery. Historical columns and staged proposal bytes remain intact.

No exactly-once external execution, rollback of completed side effects or global
transaction is implied. Older binaries do not participate in this ownership
protocol; legacy state stays explicitly unknown.

## Alternatives

1. Infer abandonment from running status, PID or timestamps: rejected because
   they do not establish execution death or prevent stale-attempt writes.
2. Repair startup recovery only: rejected because cancelled live workers can
   race retries and overwrite later attempts independently of startup.
3. Automatically adopt or force-clear legacy ownership: rejected because missing
   metadata cannot certify quiescence; fresh job IDs preserve the boundary.
