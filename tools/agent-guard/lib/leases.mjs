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

import { createHash, createHmac, randomUUID } from 'node:crypto';
import { execFileSync } from 'node:child_process';
import { accessSync, constants as fsConstants, existsSync, mkdirSync, readFileSync, readdirSync, realpathSync, renameSync, rmSync, statSync, writeFileSync } from 'node:fs';
import path from 'node:path';

import { CORE_LEASE_FIELDS, PROTOCOL_VERSION, ensureStateDirs, lanePeaksDir, leasesDir, machineToken, stateDir } from './protocol.mjs';

const GIT_EXECUTABLES = ['/usr/bin/git', '/bin/git'];
const BEHAVIOR_IDENTITY_VERSION = 6;
const MAX_INTERPRETER_PAYLOAD_BYTES = 16 * 1024 * 1024;
const INDIRECT_EXECUTABLES = new Set([
  'chrt',
  'doas',
  'env',
  'flock',
  'ionice',
  'nice',
  'nohup',
  'setsid',
  'strace',
  'stdbuf',
  'sudo',
  'time',
  'timeout',
  'xargs',
]);
const TRANSITIVE_DISPATCHERS = new Set([
  'bun',
  'bunx',
  'c8',
  'corepack',
  'deno',
  'electron',
  'npm',
  'npx',
  'pnpm',
  'pnpx',
  'playwright',
  'test-storybook',
  'vitest',
  'yarn',
  'yarnpkg',
]);
// The guard's run journal moved under the machine state directory (#239), so
// no wrapper-owned artifact is untracked in the worktree anymore: any
// untracked entry fails closed, as it always should have.

function canonicalWorktreePath(worktree) {
  try {
    return realpathSync(worktree);
  } catch {
    return path.resolve(worktree);
  }
}

function gitEnvironment(env) {
  const clean = { ...env };
  for (const name of Object.keys(clean)) {
    if (name.startsWith('GIT_')) delete clean[name];
  }
  clean.GIT_OPTIONAL_LOCKS = '0';
  return clean;
}

/**
 * Dormant future-provenance identity for a peak store namespace.
 *
 * Git's common directory is the one filesystem object shared by a checkout's
 * primary worktree and all of its linked worktrees. Canonicalising it also
 * prevents symlink spellings of the same checkout from splitting history.
 * Separate clones keep separate common directories even when their worktree
 * basenames match. If Git cannot identify the checkout, fall back to the
 * canonical full worktree path: losing history is safe; aliasing repositories
 * is not.
 */
export function repositoryIdentity(worktree, { env = process.env } = {}) {
  const canonicalWorktree = canonicalWorktreePath(worktree);

  const git = GIT_EXECUTABLES.find((candidate) => existsSync(candidate));
  if (git === undefined) return canonicalWorktree;

  // Git identity variables are caller-controlled process state. Letting them
  // redirect this lookup would let a run borrow another checkout's light-lane
  // history. Resolve from the worktree on disk instead.
  try {
    const raw = execFileSync(git, ['-C', canonicalWorktree, 'rev-parse', '--path-format=absolute', '--git-common-dir'], {
      encoding: 'utf8',
      env: gitEnvironment(env),
      timeout: 2000,
    }).trim();
    const commonDir = path.isAbsolute(raw) ? raw : path.resolve(canonicalWorktree, raw);
    return realpathSync(commonDir);
  } catch {
    return canonicalWorktree;
  }
}

function resolveExecutable(worktree, command, env) {
  const requested = command[0];
  const candidates = requested.includes(path.sep)
    ? [path.isAbsolute(requested) ? requested : path.resolve(worktree, requested)]
    : (env.PATH ?? '/usr/bin:/bin').split(path.delimiter).map((entry) => path.resolve(entry || worktree, requested));

  let executable;
  for (const candidate of candidates) {
    try {
      accessSync(candidate, fsConstants.X_OK);
      executable = realpathSync(candidate);
      break;
    } catch {
      // execvp-style search: keep looking for a runnable candidate.
    }
  }
  return executable ?? null;
}

function executableEvidence(worktree, executable) {
  try {
    const stats = statSync(executable, { bigint: true });
    if (!stats.isFile()) return null;
    const relative = path.relative(worktree, executable);
    const insideWorktree = relative === '' || (!relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative));
    if (insideWorktree) {
      return {
        scope: 'worktree',
        path: relative || '.',
        sha256: createHash('sha256').update(readFileSync(executable)).digest('hex'),
      };
    }
    return {
      scope: 'external',
      path: executable,
      device: String(stats.dev),
      inode: String(stats.ino),
      mode: String(stats.mode),
      size: String(stats.size),
      modifiedNs: String(stats.mtimeNs),
      changedNs: String(stats.ctimeNs),
    };
  } catch {
    return null;
  }
}

function executableNames(requested, canonical) {
  return [requested, canonical].map((value) => path.basename(value).toLowerCase());
}

function interpreterNameFamily(name) {
  if (/^(?:node|nodejs)(?:[.-]?\d+(?:\.\d+)*)?$/u.test(name)) return 'node';
  if (/^(?:python|pypy)(?:[.-]?\d+(?:\.\d+)*)?$/u.test(name)) return 'python';
  if (/^(?:bash|dash|ksh|sh)(?:[.-]?\d+(?:\.\d+)*)?$/u.test(name)) return 'shell';
  if (/^(?:ash|csh|fish|mksh|powershell|pwsh|tcsh|zsh)(?:[.-]?\d+(?:\.\d+)*)?$/u.test(name)) return 'unsupported-shell';
  return null;
}

function interpreterFamily(requested, canonical) {
  const [requestedFamily, canonicalFamily] = executableNames(requested, canonical).map(interpreterNameFamily);
  if (canonicalFamily === null) return requestedFamily === null ? null : 'conflict';
  if (requestedFamily !== null && requestedFamily !== canonicalFamily) return 'conflict';
  return canonicalFamily;
}

function sameStableFile(before, after) {
  return ['dev', 'ino', 'mode', 'size', 'mtimeNs', 'ctimeNs'].every((name) => before[name] === after[name]);
}

function payloadFileEvidence(raw, canonicalCwd, repositoryRoot) {
  if (raw === '' || raw === '-') return null;
  const resolved = path.resolve(canonicalCwd, raw);
  try {
    const canonical = realpathSync(resolved);
    const before = statSync(canonical, { bigint: true });
    if (!before.isFile() || before.size < 0n || before.size > BigInt(MAX_INTERPRETER_PAYLOAD_BYTES)) return null;
    const contents = readFileSync(canonical);
    const after = statSync(canonical, { bigint: true });
    if (BigInt(contents.length) !== before.size || !sameStableFile(before, after)) return null;
    return {
      scope: repositoryRelativePath(repositoryRoot, canonical) === null ? 'external' : 'worktree',
      raw,
      resolved,
      canonical,
      repositoryPath: repositoryRelativePath(repositoryRoot, canonical),
      device: String(before.dev),
      inode: String(before.ino),
      mode: String(before.mode),
      size: String(before.size),
      modifiedNs: String(before.mtimeNs),
      changedNs: String(before.ctimeNs),
      sha256: createHash('sha256').update(contents).digest('hex'),
    };
  } catch {
    return null;
  }
}

function hasIndirectEnvironment(family, env) {
  const nonEmpty = (name) => typeof env[name] === 'string' && env[name] !== '';
  if (Object.keys(env).some((name) => (name.startsWith('LD_') || name.startsWith('DYLD_')) && nonEmpty(name))) return true;
  if (family === 'node') {
    if (nonEmpty('NODE_PATH')) return true;
    if (nonEmpty('NODE_OPTIONS') && !/^--max-old-space-size=\d+$/u.test(env.NODE_OPTIONS.trim())) return true;
  }
  if (family === 'python' && ['PYTHONHOME', 'PYTHONINSPECT', 'PYTHONPATH', 'PYTHONSTARTUP', 'PYTHONUSERBASE'].some(nonEmpty)) return true;
  if (family === 'shell' && ['BASH_ENV', 'ENV', 'ZDOTDIR'].some(nonEmpty)) return true;
  return false;
}

function unrecognizedExecutableStaysCold(worktree, command) {
  const args = command.slice(1);
  if (args.length === 0 || args.some((raw) => raw.startsWith('-') || raw.startsWith('+'))) return true;
  for (const raw of args) {
    const equalsAt = raw.indexOf('=');
    const candidates = equalsAt >= 0 && equalsAt < raw.length - 1 ? [raw, raw.slice(equalsAt + 1)] : [raw];
    for (const candidate of candidates) {
      try {
        realpathSync(path.resolve(worktree, candidate));
        return true;
      } catch {
        // Literal and missing operands do not identify mutable filesystem
        // input. Exact argv still binds their behavior identity.
      }
    }
  }
  return false;
}

function commandPayloadEvidence(worktree, command, env, repositoryRoot, executable) {
  const names = executableNames(command[0], executable);
  // These launchers can execute ignored dependencies, generated outputs,
  // plugins, or network-selected code that their own entry bytes do not bind.
  // Until the guard has immutable provenance for those transitive inputs,
  // they cannot seed reusable light-lane history.
  if (names.some((name) => INDIRECT_EXECUTABLES.has(name) || TRANSITIVE_DISPATCHERS.has(name))) return null;
  const family = interpreterFamily(command[0], executable);
  if (family === null) {
    // A copied or renamed runtime has no trustworthy family. Filesystem and
    // stdin-shaped operands stay cold rather than becoming inert native argv.
    return unrecognizedExecutableStaysCold(worktree, command) ? null : { kind: 'native' };
  }
  if (family === 'conflict' || family === 'unsupported-shell') return null;
  if (hasIndirectEnvironment(family, env)) return null;
  const args = command.slice(1);
  const explicitSeparator = family === 'python' ? args[1] === '--' : args[0] === '--';
  const payloadAt = family === 'python'
    ? (args[0] === '-S' ? (explicitSeparator ? 2 : 1) : -1)
    : (explicitSeparator ? 1 : 0);
  if (
    payloadAt < 0 ||
    payloadAt >= args.length ||
    args[payloadAt] === '-' ||
    (!explicitSeparator && (args[payloadAt].startsWith('-') || args[payloadAt].startsWith('+')))
  ) return null;
  const file = payloadFileEvidence(args[payloadAt], worktree, repositoryRoot);
  if (file === null) return null;
  return { kind: 'interpreter-file', family, file };
}

function repositoryRelativePath(repositoryRoot, target) {
  const relative = path.relative(repositoryRoot, target);
  if (relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) return null;
  return relative ? relative.split(path.sep).join('/') : '.';
}

// Objects, rather than sentinel strings, keep structural tags disjoint from
// literal environment values. Only canonical containment earns a portable
// worktree-relative tag; every other spelling stays bound to its raw and
// absolute resolution evidence.
function structuralPathEvidence(raw, canonicalCwd, repositoryRoot, { emptyMeansCwd = false } = {}) {
  if (raw === '' && !emptyMeansCwd) return { scope: 'literal', raw, value: '' };
  const spelling = raw === '' && emptyMeansCwd ? '.' : raw;
  const resolved = path.resolve(canonicalCwd, spelling);
  try {
    const canonical = realpathSync(resolved);
    const repositoryPath = repositoryRelativePath(repositoryRoot, canonical);
    if (repositoryPath !== null) return { scope: 'worktree', raw, path: repositoryPath };
    return {
      scope: 'external',
      raw,
      resolved,
      canonical,
    };
  } catch {
    return { scope: 'unresolved', raw, resolved };
  }
}

function normalizedEnvironment(env, canonicalCwd, repositoryRoot) {
  const entries = [];
  for (const name of Object.keys(env).sort()) {
    if (typeof env[name] !== 'string') return null;
    const raw = env[name];
    // These are structural checkout locations, not caller-selected behavior:
    // model their repository-relative meaning so equivalent sibling
    // worktrees can share. Every other environment value remains exact.
    let value = raw;
    if (name === 'PWD' || name === 'INIT_CWD') {
      value = structuralPathEvidence(raw, canonicalCwd, repositoryRoot);
    } else if (name === 'PATH') {
      value = {
        scope: 'path-list',
        raw,
        entries: raw.split(path.delimiter).map((entry) => (
          structuralPathEvidence(entry, canonicalCwd, repositoryRoot, { emptyMeansCwd: true })
        )),
      };
    }
    entries.push([name, value]);
  }
  return entries;
}

export function repositoryWorktreeRoot(worktree, { env = process.env } = {}) {
  const canonicalCwd = canonicalWorktreePath(worktree);
  const git = GIT_EXECUTABLES.find((candidate) => existsSync(candidate));
  if (git === undefined) return null;
  try {
    const topLevel = execFileSync(
      git,
      ['-C', canonicalCwd, 'rev-parse', '--path-format=absolute', '--show-toplevel'],
      { encoding: 'utf8', env: gitEnvironment(env), timeout: 2000, stdio: ['ignore', 'pipe', 'ignore'] },
    ).trim();
    if (!path.isAbsolute(topLevel)) return null;
    const repositoryRoot = realpathSync(topLevel);
    const relative = path.relative(repositoryRoot, canonicalCwd);
    if (relative === '..' || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) return null;
    return repositoryRoot;
  } catch {
    return null;
  }
}

function repositoryRelativeCwd(repositoryRoot, canonicalCwd) {
  return repositoryRelativePath(repositoryRoot, canonicalCwd);
}

/**
 * Dormant future-provenance evidence that a command still means what a proven
 * peak measured. The production runner does not consume this identity today.
 *
 * The common Git directory deliberately lets sibling worktrees share state,
 * but that is safe only when their clean revision, executable, argv and child
 * environment agree. Any missing or dirty evidence returns null so admission
 * treats the lane as unmeasured. Environment values are authenticated with the
 * machine token and never persisted in the peak filename or file contents.
 */
export function commandBehaviorIdentity(worktree, command, { env = process.env, behaviorEnv = env } = {}) {
  if (!Array.isArray(command) || command.length === 0 || command.some((part) => typeof part !== 'string')) return null;
  const canonicalCwd = canonicalWorktreePath(worktree);
  const git = GIT_EXECUTABLES.find((candidate) => existsSync(candidate));
  if (git === undefined) return null;

  try {
    const options = { encoding: 'utf8', env: gitEnvironment(env), timeout: 5000 };
    const repositoryRoot = repositoryWorktreeRoot(canonicalCwd, { env });
    if (repositoryRoot === null) return null;
    const relativeCwd = repositoryRelativeCwd(repositoryRoot, canonicalCwd);
    if (relativeCwd === null) return null;
    const status = execFileSync(
      git,
      ['-C', canonicalCwd, 'status', '--porcelain=v2', '--branch', '-z', '--untracked-files=all', '--ignore-submodules=none'],
      options,
    ).split('\0').filter(Boolean);
    const revision = status.find((entry) => entry.startsWith('# branch.oid '))?.slice('# branch.oid '.length);
    const dirty = status.some((entry) => !entry.startsWith('# '));
    if (!revision || revision === '(initial)' || dirty) return null;

    // status/diff intentionally trust index hints such as assume-unchanged and
    // skip-worktree. Those hints are local performance controls, not evidence
    // that the worktree bytes still match HEAD, so any such entry makes this a
    // cold start. With `-v`, ordinary tracked entries are tagged `H`, while an
    // assume-unchanged tag is lower-case and skip-worktree is tagged `S`.
    const tracked = execFileSync(git, ['-C', canonicalCwd, 'ls-files', '-v', '-z'], options)
      .split('\0')
      .filter(Boolean);
    if (tracked.some((entry) => !entry.startsWith('H '))) return null;

    const resolvedExecutable = resolveExecutable(canonicalCwd, command, behaviorEnv);
    if (resolvedExecutable === null) return null;
    const executable = executableEvidence(canonicalCwd, resolvedExecutable);
    const payload = commandPayloadEvidence(canonicalCwd, command, behaviorEnv, repositoryRoot, resolvedExecutable);
    const environment = normalizedEnvironment(behaviorEnv, canonicalCwd, repositoryRoot);
    if (executable === null || payload === null || environment === null) return null;
    const evidence = JSON.stringify({
      version: BEHAVIOR_IDENTITY_VERSION,
      revision,
      command,
      cwd: { raw: worktree, canonical: canonicalCwd, repositoryRelative: relativeCwd },
      executable,
      payload,
      environment,
    });
    return createHmac('sha256', machineToken(env)).update(evidence).digest('hex');
  } catch {
    return null;
  }
}

function lanePeakFile(env, repo, label, command, behaviorIdentity) {
  const valid =
    typeof repo === 'string' &&
    repo !== '' &&
    typeof label === 'string' &&
    label !== '' &&
    Array.isArray(command) &&
    command.length > 0 &&
    command.every((part) => typeof part === 'string') &&
    typeof behaviorIdentity === 'string' &&
    behaviorIdentity !== '';
  if (!valid) return null;
  // Full canonical paths are deliberately part of the identity, but must not
  // become filenames: long worktree roots can exceed a filesystem component
  // limit, and delimiter-based concatenation has ambiguous edge cases. Hash
  // the structured tuple instead.
  const digest = createHash('sha256')
    .update(JSON.stringify([BEHAVIOR_IDENTITY_VERSION, repo, label, command, behaviorIdentity]))
    .digest('hex');
  return path.join(lanePeaksDir(env), `${digest}.json`);
}

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

function writeFileAtomically(file, content) {
  const temp = `${file}.${process.pid}.${randomUUID()}.tmp`;
  try {
    writeFileSync(temp, content);
    renameSync(temp, file);
  } finally {
    try {
      rmSync(temp, { force: true });
    } catch {
      // The rename consumed it, or cleanup will happen with the state store.
    }
  }
}

function writeLeaseFile(file, lease) {
  // Write-then-rename: a reader never sees a partial lease, which would
  // otherwise be reaped as junk and free budget that is genuinely in use.
  writeFileAtomically(file, `${JSON.stringify(lease, null, 2)}\n`);
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

/**
 * Dormant future-provenance store API, keyed by repository, label, exact argv,
 * and verified behavior identity. run-guarded currently neither calls this nor
 * reads its output: automatic polling cannot prove a true high-water mark.
 * Retained for compatibility tests and a future trusted recorder.
 */
export async function recordLanePeak({ env = process.env, repo, label, command, behaviorIdentity, peakRssMb }) {
  if (!Number.isFinite(peakRssMb) || peakRssMb <= 0) return false;
  const file = lanePeakFile(env, repo, label, command, behaviorIdentity);
  if (file === null) return false;
  try {
    return await withAdmissionLock(env, () => {
      let peaks = [];
      try {
        const parsed = JSON.parse(readFileSync(file, 'utf8'));
        if (Array.isArray(parsed)) peaks = parsed.filter((value) => Number.isFinite(value) && value > 0);
      } catch {
        // First record, or an unreadable file: start fresh.
      }
      peaks.push(Math.round(peakRssMb));
      writeFileAtomically(file, `${JSON.stringify(peaks.slice(-5))}\n`);
      return true;
    });
  } catch {
    // Peak history is an optimization; lock or write failure must not fail the run.
    return false;
  }
}

/** The largest recent recorded peak for a lane, or null without history. */
export function readLanePeakMb({ env = process.env, repo, label, command, behaviorIdentity }) {
  const file = lanePeakFile(env, repo, label, command, behaviorIdentity);
  if (file === null) return null;
  try {
    const parsed = JSON.parse(readFileSync(file, 'utf8'));
    const peaks = Array.isArray(parsed) ? parsed.filter((value) => Number.isFinite(value) && value > 0) : [];
    return peaks.length > 0 ? Math.max(...peaks) : null;
  } catch {
    return null;
  }
}

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
        throw new Error('timed out waiting for the admission lock; refusing to proceed without machine-wide serialization', {
          cause: error,
        });
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
