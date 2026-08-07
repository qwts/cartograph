// The wire contract every copy of the guard on one machine agrees on.
//
// Coordination happens through shared *state*, not shared code: each governed
// repo carries a byte-identical mirror of this tool (synced, never hand-edited
// — see docs/reference/agent-memory-guard.md), and those copies find each
// other through one per-machine directory. That is the whole fix for the
// per-worktree lock: the budget's scope is the machine, so the state must be
// too.
//
// Because copies can be mid-rollout and therefore of different vintages, the
// reader is deliberately permissive: a lease from an unknown protocol version
// still *counts against the budget* as long as its core fields parse. Refusing
// to see a newer neighbour's lease would silently restore the exact
// over-subscription this tool exists to prevent. Only the core fields below may
// ever change meaning; anything else is additive.

import { randomBytes } from 'node:crypto';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { userInfo } from 'node:os';
import path from 'node:path';

export const PROTOCOL_VERSION = 1;

// Core lease fields. Stable forever — a newer copy may add fields, never
// repurpose these, because older copies budget against them.
export const CORE_LEASE_FIELDS = ['id', 'pid', 'estimatedMb', 'grantedAt'];

/**
 * The per-machine, per-user state directory.
 *
 * Deliberately NOT `~/.cache` on macOS and never a path the user might sync.
 * A lease is only meaningful next to the kernel that can answer "is pid N
 * alive?"; a state directory replicated to another machine by iCloud/Dropbox
 * would hand us foreign pids that happen to collide with local ones — the
 * cross-machine trap that made qwts/overlook#842's crashed locks
 * unreclaimable, arriving through a different door. `~/Library/Caches` is
 * excluded from iCloud Drive by design, `$XDG_RUNTIME_DIR` is tmpfs, and
 * `~/.cache` is conventionally unsynced on Linux.
 *
 * Production resolution ignores environment-selected home/cache paths. An
 * agent can set HOME, XDG_CACHE_HOME, XDG_RUNTIME_DIR, or even the test-only
 * override before starting Node; none of those values may mint a private
 * lease namespace. A separately supplied environment object remains an
 * explicit test seam for unit and conformance tests.
 */
export function stateDir(env = process.env) {
  if (env === process.env) {
    const home = userInfo().homedir;
    return process.platform === 'darwin'
      ? path.join(home, 'Library', 'Caches', 'agent-guard')
      : path.join(home, '.cache', 'agent-guard');
  }
  const explicit = typeof env.AGENT_GUARD_STATE_DIR === 'string' ? env.AGENT_GUARD_STATE_DIR.trim() : '';
  if (explicit) return explicit;
  const runtime = typeof env.XDG_RUNTIME_DIR === 'string' ? env.XDG_RUNTIME_DIR.trim() : '';
  if (runtime) return path.join(runtime, 'agent-guard');
  const home = env.HOME || userInfo().homedir;
  if (process.platform === 'darwin') return path.join(home, 'Library', 'Caches', 'agent-guard');
  const cache = typeof env.XDG_CACHE_HOME === 'string' && env.XDG_CACHE_HOME.trim() ? env.XDG_CACHE_HOME.trim() : path.join(home, '.cache');
  return path.join(cache, 'agent-guard');
}

export function leasesDir(env = process.env) {
  return path.join(stateDir(env), 'leases');
}

export function grantsDir(env = process.env) {
  return path.join(stateDir(env), 'grants');
}

/**
 * A random identifier for "the state directory this machine is actually
 * using", minted once and stored beside the leases.
 *
 * This is the #842-safe replacement for a hostname. The lesson from that
 * incident was not "hostnames are unreliable on macOS" but the general one:
 * never key liveness on a name the network can change underneath you. A token
 * generated locally, stored with the state it identifies, and compared only
 * for equality cannot drift — it has no source but itself. If a directory ever
 * is restored or copied from elsewhere, its leases carry the wrong token and
 * are discarded on sight rather than trusted or, worse, made unreclaimable.
 */
export function machineToken(env = process.env) {
  const file = path.join(stateDir(env), 'machine-token');
  try {
    const token = readFileSync(file, 'utf8').trim();
    if (token) return token;
  } catch {
    // Missing or unreadable: mint below.
  }
  const minted = randomBytes(16).toString('hex');
  try {
    mkdirSync(stateDir(env), { recursive: true });
    writeFileSync(file, `${minted}\n`, { flag: 'wx' });
    return minted;
  } catch {
    // Lost the race with a concurrent run, or the directory is read-only.
    // A concurrent winner's token is the right answer; a genuinely unwritable
    // state directory degrades to a per-process token, which reaps every
    // foreign lease and is reported by `arbiter.mjs doctor`.
    try {
      const token = readFileSync(file, 'utf8').trim();
      if (token) return token;
    } catch {
      // Fall through.
    }
    return minted;
  }
}

export function ensureStateDirs(env = process.env) {
  mkdirSync(leasesDir(env), { recursive: true });
  mkdirSync(grantsDir(env), { recursive: true });
  return stateDir(env);
}
