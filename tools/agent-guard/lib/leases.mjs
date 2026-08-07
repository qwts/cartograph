// The machine-scoped lease store.
//
// This is the file that fixes the core bug. The guard this replaces locked
// `<worktree>/.guard/active.json`, so every worktree, every repo and every
// agent tool got its own lock and each one correctly concluded it was the only
// run on the box. N agents, N locks, one machine, no coordination at all.
// Leases live in ONE directory per machine (lib/protocol.mjs), so a Codex
// session in image-trail and a Claude Code session in overlook are visible to
// each other.
//
// One file per lease, not one shared file: writers never contend, and a
// half-written lease can only ever corrupt itself. Readers tolerate junk.
//
// VALIDITY IS LIVENESS, AND ONLY LIVENESS. Before spawn, a lease follows its
// wrapper pid; after spawn, it follows the detached process group doing the
// work, so killing the wrapper cannot release budget while descendants remain.
// It is not keyed on hostname — qwts/overlook#842 is the org's paid lesson
// there: `.local` ↔ `.lan` drift made crashed same-machine locks permanently
// unreclaimable, so the crash-recovery path became the outage.

import { randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, readFileSync, readdirSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';

import { CORE_LEASE_FIELDS, PROTOCOL_VERSION, ensureStateDirs, leasesDir, machineToken, stateDir } from './protocol.mjs';

export function isProcessAlive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    // EPERM means the process exists but belongs to another user — alive.
    return error.code === 'EPERM';
  }
}

export function isProcessGroupAlive(processGroupId) {
  if (!Number.isInteger(processGroupId) || processGroupId <= 0) return false;
  try {
    process.kill(-processGroupId, 0);
    return true;
  } catch (error) {
    // EPERM means the group exists but belongs to another user — alive.
    return error.code === 'EPERM';
  }
}

export function psExecutable() {
  return ['/bin/ps', '/usr/bin/ps'].find((candidate) => existsSync(candidate)) ?? null;
}

export function processGroupIdFor(pid = process.pid) {
  if (!Number.isInteger(pid) || pid <= 0 || process.platform === 'win32') return null;
  const executable = psExecutable();
  if (executable === null) return null;
  try {
    const raw = execFileSync(executable, ['-o', 'pgid=', '-p', String(pid)], {
      encoding: 'utf8',
      timeout: 2000,
    }).trim();
    const processGroupId = Number(raw);
    return Number.isInteger(processGroupId) && processGroupId > 0 ? processGroupId : null;
  } catch {
    return null;
  }
}

function parseLease(raw) {
  let value;
  try {
    value = JSON.parse(raw);
  } catch {
    return null;
  }
  if (value === null || typeof value !== 'object' || Array.isArray(value)) return null;
  // Core fields only: a lease written by a newer copy of this tool must still
  // count against the budget, or a mixed-version fleet silently oversubscribes.
  for (const field of CORE_LEASE_FIELDS) {
    if (value[field] === undefined) return null;
  }
  if (!Number.isInteger(value.pid) || value.pid <= 0 || !Number.isFinite(value.estimatedMb) || value.estimatedMb <= 0) return null;
  return value;
}

/**
 * Every live lease on this machine, reaping the dead ones on the way past.
 *
 * Reaping is a side effect of reading by design: there is no daemon here and
 * there must not be one, so the next run to look is what cleans up after a
 * force-quit agent. `unlink` failures are ignored — another process reaping
 * the same lease concurrently is the expected case, not an error.
 */
export function readLeases(env = process.env, { reap = true } = {}) {
  const dir = leasesDir(env);
  const token = machineToken(env);
  let entries;
  try {
    entries = readdirSync(dir).filter((name) => name.endsWith('.json'));
  } catch {
    return [];
  }
  const live = [];
  for (const name of entries) {
    const file = path.join(dir, name);
    let lease;
    try {
      lease = parseLease(readFileSync(file, 'utf8'));
    } catch {
      lease = null;
    }
    // Unparseable, foreign (a restored or copied state directory — see
    // machineToken), or dead: none of these may hold budget.
    const alive =
      lease !== null && Number.isInteger(lease.processGroupId) && lease.processGroupId > 0
        ? isProcessGroupAlive(lease.processGroupId)
        : lease !== null && isProcessAlive(lease.pid);
    const stale = lease === null || (lease.machineToken !== undefined && lease.machineToken !== token) || !alive;
    if (stale) {
      if (reap) {
        try {
          rmSync(file, { force: true });
        } catch {
          // Concurrent reap; harmless.
        }
      }
      continue;
    }
    live.push({ ...lease, file });
  }
  return live;
}

function writeLeaseFile(file, lease) {
  // Write-then-rename: a reader never sees a partial lease, which would
  // otherwise be reaped as junk and free budget that is genuinely in use.
  const temp = `${file}.${process.pid}.tmp`;
  writeFileSync(temp, `${JSON.stringify(lease, null, 2)}\n`);
  renameSync(temp, file);
}

export function acquireLease({ env = process.env, label, estimatedMb, repo, worktree, harness, command, pid = process.pid }) {
  ensureStateDirs(env);
  const lease = {
    protocol: PROTOCOL_VERSION,
    id: randomUUID(),
    pid,
    machineToken: machineToken(env),
    label,
    repo,
    worktree,
    harness,
    command,
    estimatedMb,
    observedMb: 0,
    grantedAt: new Date().toISOString(),
    heartbeatAt: new Date().toISOString(),
  };
  const file = path.join(leasesDir(env), `${lease.id}.json`);
  writeLeaseFile(file, lease);
  return { ...lease, file };
}

/**
 * Report the run's real tree RSS into its own lease.
 *
 * This is what keeps the budget honest as a run warms up: the arithmetic in
 * lib/budget.mjs subtracts observed usage from the reservation, because the
 * observed part has already been counted by the kernel in `availableMb`.
 * Without it, a long-running suite would double-count itself and lock the
 * machine down harder the longer it ran.
 */
export function heartbeatLease(lease, observedMb) {
  try {
    writeLeaseFile(lease.file, { ...lease, file: undefined, observedMb, heartbeatAt: new Date().toISOString() });
    return true;
  } catch {
    return false;
  }
}

/**
 * Bind an admitted lease to the detached process group that consumes it.
 *
 * Admission starts before spawn, so acquireLease initially records the
 * wrapper. Once spawn succeeds, group liveness becomes authoritative: if the
 * wrapper is SIGKILLed, descendants still holding memory retain the lease.
 */
export function retargetLease(lease, { pid, processGroupId = pid }) {
  if (!Number.isInteger(pid) || pid <= 0 || !Number.isInteger(processGroupId) || processGroupId <= 0) return false;
  try {
    Object.assign(lease, { pid, processGroupId });
    writeLeaseFile(lease.file, { ...lease, file: undefined });
    return true;
  } catch {
    return false;
  }
}

export function releaseLease(lease) {
  try {
    rmSync(lease.file, { force: true });
    return true;
  } catch {
    return false;
  }
}

/**
 * Does this id name a lease that is currently held?
 *
 * The wrapper marks its children with the id of the lease it holds, and only
 * honours a live lease owned by the caller's own process group. Lease ids are
 * visible to same-user processes, so id knowledge alone is not an ancestry
 * proof; an unrelated process falls through to full admission.
 */
export function leaseExists(id, env = process.env, { processGroupId = processGroupIdFor() } = {}) {
  if (typeof id !== 'string' || id === '' || !Number.isInteger(processGroupId) || processGroupId <= 0) return false;
  return readLeases(env, { reap: false }).some((lease) => lease.id === id && lease.processGroupId === processGroupId);
}

// --- Admission mutex ---------------------------------------------------------
//
// Reading the leases, deciding, and writing the new lease has to be one step.
// Two runs starting together would otherwise both decide against the same
// snapshot and both be admitted — precisely the concurrent-agent case the
// machine budget exists to coordinate, so losing it here would leave the
// headline defect half-fixed.
//
// `mkdir` is the primitive: atomic on every filesystem this runs on, and it
// leaves a directory whose owner can be recorded and whose staleness can be
// judged. The critical section is a few file reads, so contention is brief.

const LOCK_STALE_MS = 30_000;
const LOCK_POLL_MS = 50;

function lockPath(env) {
  return path.join(stateDir(env), 'admission.lock');
}

function breakStaleLock(dir) {
  // A crashed holder must not wedge admission for everyone. Liveness first, and
  // age as the backstop for a holder whose pid was recycled.
  try {
    const owner = JSON.parse(readFileSync(path.join(dir, 'owner.json'), 'utf8'));
    if (Number.isFinite(owner.pid) && isProcessAlive(owner.pid)) {
      const acquiredAt = Date.parse(owner.at);
      // Malformed provenance is not evidence that a live holder is stale.
      // Deleting here would admit a second contender concurrently; retain the
      // lock and let the bounded wait fail closed instead.
      if (!Number.isFinite(acquiredAt) || Date.now() - acquiredAt < LOCK_STALE_MS) return false;
    }
  } catch {
    // Unreadable owner: judge by directory age below.
    try {
      if (Date.now() - statSync(dir).mtimeMs < LOCK_STALE_MS) return false;
    } catch {
      return false;
    }
  }
  try {
    rmSync(dir, { recursive: true, force: true });
    return true;
  } catch {
    return false;
  }
}

export async function withAdmissionLock(env, fn, { timeoutMs = 15_000, now = () => Date.now() } = {}) {
  ensureStateDirs(env);
  const dir = lockPath(env);
  const deadline = now() + timeoutMs;
  for (;;) {
    try {
      mkdirSync(dir);
      break;
    } catch (error) {
      if (error.code !== 'EEXIST') throw error;
      breakStaleLock(dir);
      if (now() >= deadline) {
        throw new Error('timed out waiting for the admission lock; refusing to proceed without machine-wide serialization');
      }
      await new Promise((resolve) => {
        setTimeout(resolve, LOCK_POLL_MS);
      });
    }
  }
  try {
    writeFileSync(path.join(dir, 'owner.json'), JSON.stringify({ pid: process.pid, at: new Date().toISOString() }));
  } catch {
    // Best-effort provenance; the directory itself is the lock.
  }
  try {
    return await fn();
  } finally {
    try {
      rmSync(dir, { recursive: true, force: true });
    } catch {
      // Stale-breaking will collect it.
    }
  }
}
