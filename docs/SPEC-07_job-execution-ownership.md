# SPEC-07 — Durable job execution ownership

Status: implementation in progress, #393. Decision: [ADR-0025](adr/ADR-0025-job-execution-ownership.md).
SPEC-00 job durability, SPEC-03 completed proposal staging and SPEC-05 source ownership remain binding.

## Scope and authority

Opening another app process must not interrupt a live job. A cancelled worker can
continue unwinding or finish an already-started model request; retry must not reuse
its job ID while that execution remains alive. Every worker mutation identifies
its own attempt, rather than relying on the current status of a reusable job ID.
This applies to plugin gates, single/class escalation, direct intake, managed
add-repo, add-system and the existing supported retry dispatcher. It does not add
a job to semantic preview or enable new retry kinds.

The durable job record is not a liveness proof. A PID, timestamp, heartbeat age,
stored owner token or status flag cannot establish process death. Private
per-job OS ownership and guarded database transitions establish the limited
same-job execution contract below. Source guards protect different resources.

## Private metadata and legacy history

Keep existing jobs columns, values, IDs, kinds and creation history intact. Add a
jobs-owned versioned schema for a database namespace, attempt generation and
opaque owner token. Do not reuse global SQLite user_version. Initialize metadata
and new jobs atomically; a new job begins with recorded unclaimed ownership.
Generations increase on every successful execution claim and overflow fails.
Owner tokens are host-generated, bounded and private; never accept them from UI
requests or serialize them into proposals, graph facts or job responses.

Existing rows without execution metadata remain legacy_unknown. Startup does not
rewrite them or label them abandoned. Same-ID claim/retry is unavailable for those
rows, even when their kind is otherwise supported: start a fresh operation with
a new job ID. Explicit user cancellation remains a guarded lifecycle transition,
not proof that an old worker exited. No forced takeover or implicit quiescence
assumption is introduced. Concurrent older/newer binaries sharing state are not
promised coordinated execution. This migration preserves history rather than
inventing ownership for earlier work.

The Job response adds execution_tracking: recorded or legacy_unknown, copied
coherently with the row from one database snapshot. It describes whether the
ownership protocol applies, not whether a process is currently alive. All existing
stored history fields retain their values. Proposal IDs, immutable payloads,
review revisions, source binding and context eligibility remain unchanged.

## Execution guards and claims

Use a private host-owned directory and persistent per-job lock files. Open fresh
try-only exclusive handles without following substituted entries; validate
regular-file/directory identity and scope to the exact state store. Unix modes
are owner-only using portable rooted permission operations. Never unlink,
replace or recreate live lock files to bypass contention. Busy is distinct from
unavailable/invalid locking; both fail without unlocked fallback. Private namespace
checks do not promise isolation against arbitrary concurrent mutation by a
process with the same user's filesystem authority.

Before returning a claimable reservation, hold the acquired lock while syncing
its file, namespace directory, job-executions directory and existing app-data
directory, in that order, then revalidate their identities. This persists every
entry created by the lock manager before SQLite can commit ownership. Repeat
the chain for existing entries because an earlier preparation may have stopped
before syncing them. Rooted readable directory handles avoid O_PATH-only handles.
Any unsupported or failed sync rejects the reservation, releases its lock and
leaves the job unclaimed; there is no unsynced fallback. The pre-existing app-data
trust root's entry in its external parent is outside this creation boundary.

Windows remains a supported target. For this execution protocol, private job
storage must report NTFS through `GetVolumeInformationByHandleW` on the actual
retained file and reopened directory handles. Unknown filesystems, non-NTFS
filesystems and failed queries reject the reservation before SQL ownership;
an apparent successful flush alone does not certify their directory durability.
Reopen each rooted directory with read and write access plus backup semantics,
without following symlinks or allowing replacement of the retained directory.
Validate its identity, then perform the real `FlushFileBuffers` through
`sync_all`; do not substitute a no-op, privileged volume flush or ambient-path
reopen. Use stable handle-derived cap metadata for link counts and identity.
This is a private-storage capability boundary, not a limit on the filesystem of
repositories being analyzed. It requires no administrator privileges beyond
ordinary access to the app's own storage.

Microsoft requires [write access for FlushFileBuffers](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-flushfilebuffers).
Its [flush contract](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/0de7dc40-9627-437e-a4df-c4696cdc3d02)
includes directory persistence, with [product behavior note 80](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-fsa/4e3695bd-7574-4f24-a223-b4679c065b63)
limiting that guarantee to NTFS. Handle-based [volume information](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getvolumeinformationbyhandlew)
is checked without selecting a second ambient path.

Separate reservation from mutation: copy the required claim/recovery plan under
the jobs mutex, release it, acquire the OS reservation, then enter a short SQLite
IMMEDIATE transaction. No database, graph, cache or settings mutex spans an OS
lock attempt. Existing source admission may hold source guards first; try-only
job reservations introduce no waiting cycle. Retain the exact same reservation
through transition and worker use, with no unlocked handoff.

A claim validates the observed kind, status, attempt metadata and store namespace,
then transitions directly to running with a new generation and owner in one
transaction. Retry preserves existing kind/source/availability/guard admission
before mutation, accepts only the supported terminal states and recorded metadata,
and claims running directly; no unowned queued-to-running interval remains.
Concurrent claims have one winner. Cancellation or a changed plan before claim
rejects the claim without overwriting the winning transition.

The resulting opaque JobExecution owns an internally shared guard and immutable
private namespace/job/generation/owner identity. Cloning the execution shares
that same ownership; only the last holder releases it. Release explicitly unlocks
and closes; abnormal process exit releases OS ownership. Hold it across the entire
worker lifetime, including publication, job settlement, source readiness and
cancellation cleanup. Terminal job status alone never releases a live worker.

## Guarded worker and user transitions

Production progress, execution checks, finish and fail require JobExecution.
Every SQL mutation matches job ID, generation, owner and allowed status in one
transaction, returning the winning job from the same snapshot. No production
bare-ID set_status/progress/finish/fail operation remains. A missing row, foreign
store, invalid or superseded attempt, interrupted execution or storage failure
stops work explicitly, without being misreported as user cancellation.

Progress changes only a running attempt. Finish/fail change only that matching
running attempt. For the same attempt already cancelled, these operations return
the unchanged cancelled row, retaining progress/error/artifact history; they never
revive it or treat completed model output as a newer attempt. Cancel is a single
conditional transaction over queued/running states; completion before cancel stays
done, and cancel before completion stays cancelled. Terminal or missing rows are
explicit invalid transitions.

Cancellation is cooperative. Worker boundaries check the exact attempt and stop
before launching further work; already-completed side effects are not rolled back.
Clear-finished keeps its existing terminal-row policy. If a cancelled row is
cleared while its worker is alive, later checks stop and lifecycle writes fail;
historical proposal rows and the persistent lock file remain independent of job
cleanup. An already-started model call may still durably stage its completed
result using its retained execution after explicit missing-row detection. That
narrow persistence exception requires valid retained ownership and does not
permit another model call, graph publication or job transition; stale, foreign,
interrupted or invalid ownership still fails.

## Recovery

Startup copies exact recorded running candidates, including their attempt/owner,
then attempts each job's OS reservation without holding the database mutex. Busy
candidates remain untouched. For an acquired reservation, one guarded transaction
changes only the exact observed still-running attempt to interrupted. Hold the
reservation through the update; return only IDs actually changed. A concurrent
completion, cancellation, deletion or different attempt wins and is never
rewritten. A malformed metadata/lock state fails explicitly; it is not evidence
that a job is abandoned. Legacy-unknown rows are listed honestly and unchanged.

Recovery neither restarts work nor replays model calls automatically. A recovered
attempt requires the existing explicit supported retry path.

## Host and UI integration

Fresh job starts, plugin workers, ingest/add workers and supported retries use
one claim helper. Shared progress/failure/settlement helpers accept the execution.
Metrics, details, summaries and staging keep the stable job ID; this does not
change immutable proposal identity or make separate stores globally transactional.

Both context-assembly and model/staging blocking closures retain their own
JobExecution clone. Dropping or aborting an awaiting async command must not release
ownership while a detached blocking worker continues. Single-result and partial
batch staging preserve AC-0128: a completed result persists after cancellation,
returns for review, and leaves the job cancelled. Cancellation callbacks stop on
invalid ownership too, but the host reports that distinct failure rather than
calling every stopped execution cancelled.

The Jobs surface explains legacy-unknown rows and requires fresh recovery instead
of offering unsupported retry/resume or claiming that historical progress is live.
Recorded jobs keep their existing controls. Busy or rejected retry leaves history
unchanged and produces an actionable visible error; cancellation does not promise
instant worker termination. The UI does not infer liveness from tracking metadata.

## Verification and limits

Traced AC-0156–0163 cover schema/legacy handling, real OS ownership, fenced worker
updates, candidate-conditional recovery, guarded cancel/retry races, complete host
lifetime, visible UI handling and unchanged historical/source semantics. Use
independent database connections and deterministic interleaving hooks, not sleeps.
A handshake-controlled subprocess proves live ownership excludes recovery and
confirmed exit permits it. Test cancelled workers that still hold ownership,
competing claims, stale-attempt writes, removal while active, dropped async waiters
with live blocking workers, and exact retained proposal history.
Windows tests exercise actual NTFS handles and flushes, rejected filesystem
queries/classification before claim, and unchanged jobs after failure. A retained
Windows directory handle may prevent substitution outright; assert that refusal
and the unchanged rooted identity rather than requiring a successful rename.
These tests establish API behavior and synchronization order, not a simulation of
physical power loss or a guarantee about hardware that ignores flush requests.

This slice establishes same-job ownership and accurate lifecycle transitions.
It does not establish exactly-once external model execution, rollback of published
graph/settings/proposal side effects, whole-operation atomicity, H2 business truth,
H3 curated projection or MCP/ACP runtime. SPEC-05 managed-source readiness and
SPEC-06 retained-source guards remain separate contracts.
