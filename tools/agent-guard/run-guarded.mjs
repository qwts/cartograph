#!/usr/bin/env node

// Machine-scoped guarded runner. Replaces the per-worktree guard that every
// governed repo carried its own drifting copy of.
//
// What it does, in order:
//   1. Applies the agent-vs-human lane policy (lib/policy.mjs).
//   2. Gets out of the way for allowed nested guarded commands.
//   3. Derives the ceiling from the effective machine/cgroup total and CLAMPS the request down to
//      it — an `--rss-mb 8192` inherited from an old npm script becomes 3072 on
//      an 8 GB machine instead of a ceiling that can never trip.
//   4. Asks the machine-wide arbiter for admission, counting every other repo's
//      and every other agent's outstanding leases plus real availability and
//      swap. Queues or refuses with the arithmetic.
//   5. Runs the command in its own process group, polls the whole descendant
//      tree's RSS, heartbeats observed usage into its lease, and kills the
//      group on breach (SIGTERM, then SIGKILL).
//
// Usage: node tools/agent-guard/run-guarded.mjs [--label name] [--rss-mb N]
//        [--heap-mb N] [--timeout-s N] [--wait-s N] [--] <command> [args...]
// Env:   AGENT_GUARD_RSS_MB, AGENT_GUARD_HEAP_MB, AGENT_GUARD_TIMEOUT_S,
//        AGENT_GUARD_WAIT_S, AGENT_GUARD_STATE_DIR, AGENT_GUARD_FORCE=1
//        (human escape hatch — the command hook blocks agents from using it),
//        AGENT_GUARDED=1 (set for children so nested guards pass through).

import { execFile, spawn } from 'node:child_process';
import { createHash } from 'node:crypto';
import { appendFileSync, mkdirSync, realpathSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import process from 'node:process';

import { clampCeiling, decideAdmission, deriveBudgetForMemory } from './lib/budget.mjs';
import { acquireLease, heartbeatLease, leaseExists, psExecutable, readLeases, releaseLease, repositoryIdentity, retargetLease, withAdmissionLock } from './lib/leases.mjs';
import { evaluateLanePolicy, harnessName, isAgentSession } from './lib/policy.mjs';
import { journalDir } from './lib/protocol.mjs';
import { readMemoryStatus, topConsumers } from './lib/system-memory.mjs';

export const POLL_MS = 250;
const HEARTBEAT_MS = 3000;
const SIGKILL_AFTER_MS = 2000;
// A runaway can allocate faster than a SIGTERM shutdown completes; past this
// factor of the ceiling, skip straight to SIGKILL.
const HARD_KILL_FACTOR = 1.25;
const MAX_MONITOR_FAILURES = 3;
const DEFAULT_TIMEOUT_S = 900;
const RETRY_MS = 5000;

function note(message) {
  process.stderr.write(`[guard] ${message}\n`);
}

function fail(message) {
  note(message);
  process.exit(1);
}

export function parseArgs(argv) {
  const options = { label: 'command', rssMb: null, heapMb: null, timeoutS: null, waitS: null };
  let index = 0;
  const takeValue = (flag) => {
    index += 1;
    if (index >= argv.length) throw new Error(`missing value for ${flag}`);
    return argv[index];
  };
  for (; index < argv.length; index += 1) {
    const arg = argv[index];
    if (arg === '--') {
      index += 1;
      break;
    }
    if (arg === '--label') options.label = takeValue(arg);
    else if (arg === '--rss-mb') options.rssMb = Number(takeValue(arg));
    else if (arg === '--heap-mb') options.heapMb = Number(takeValue(arg));
    else if (arg === '--timeout-s') options.timeoutS = Number(takeValue(arg));
    else if (arg === '--wait-s') options.waitS = Number(takeValue(arg));
    else if (arg.startsWith('--')) throw new Error(`unknown flag ${arg}`);
    else break;
  }
  return { options, command: argv.slice(index) };
}

export function resolveRequest(options, env, budget) {
  const pick = (envName, flagValue, fallback) => {
    const raw = env[envName];
    const fromEnv = raw === undefined || raw === '' ? NaN : Number(raw);
    if (!Number.isNaN(fromEnv)) return fromEnv;
    if (flagValue !== null && !Number.isNaN(flagValue)) return flagValue;
    return fallback;
  };
  const ceiling = clampCeiling(pick('AGENT_GUARD_RSS_MB', options.rssMb, budget.maxRunMb), budget);
  return {
    ...ceiling,
    // The per-process V8 heap tracks the tree ceiling rather than a constant:
    // half the tree budget, so one worker cannot single-handedly reach it.
    heapMb: Math.max(256, Math.round(pick('AGENT_GUARD_HEAP_MB', options.heapMb, Math.floor(ceiling.ceilingMb / 2)))),
    timeoutS: pick('AGENT_GUARD_TIMEOUT_S', options.timeoutS, DEFAULT_TIMEOUT_S),
    waitS: pick('AGENT_GUARD_WAIT_S', options.waitS, isAgentSession(env) ? 0 : 180),
  };
}

/**
 * Decide whether this invocation is refused, is already covered by its
 * parent's lease, or needs its own admission. Lane policy comes first: a live
 * lease proves that admission and enforcement already exist for the process
 * group, but it does not authorize a different command hidden inside an
 * innocuously named guarded package script (#235).
 */
export function resolveInvocation({ options, command, env = process.env, processGroupId }) {
  const commandLine = command.join(' ');
  const policy = evaluateLanePolicy({ label: options.label, command: commandLine, env });
  if (!policy.allowed) return { action: 'refuse', commandLine, policy };
  if (leaseExists(env.AGENT_GUARDED, env, { processGroupId })) return { action: 'passthrough', commandLine, policy };
  return { action: 'admit', commandLine, policy };
}

// Automatic peak reuse is deliberately dormant. Polling cannot prove a true
// process-tree high-water mark, and arbitrary commands may consume inherited
// stdin or mutable transitive inputs that are not bound by behavior evidence.
// Keep the admission seam explicit for a future OS-backed/provenance-backed
// design, but never let an existing store entry reduce today's reservation.
export function applyAutomaticLaneHistoryPolicy(request) {
  request.lanePeakMb = null;
  request.reserveMb = request.ceilingMb;
  return request;
}

/**
 * Per-run diagnostics live under the machine state directory, keyed by
 * repository identity — never in the checkout (#239). A `.guard/` directory
 * in the worktree dirtied every tree a guarded run touched, breaking
 * signed-commit fail-closed checks and risking machine-local telemetry being
 * committed. Keying by `repositoryIdentity` (Git's common dir, canonicalised)
 * gives one journal per clone shared by all of its linked worktrees, and the
 * SHA-256 filename mirrors `lanePeakFile`: full paths must not become
 * filesystem component names.
 */
export function guardDiagnosticPaths(worktree, { env = process.env } = {}) {
  let cwd;
  try {
    cwd = realpathSync(worktree);
  } catch {
    cwd = path.resolve(worktree);
  }
  const identity = repositoryIdentity(cwd, { env });
  const digest = createHash('sha256').update(identity).digest('hex');
  const guardDir = path.join(journalDir(env), digest);
  const lastRunPath = path.join(guardDir, 'last-run.json');
  return {
    guardDir,
    lastRunPath,
    // Outside the checkout, so an absolute path is the honest display form.
    lastRunDisplayPath: lastRunPath,
  };
}

// Aggregate RSS (KB) of the guarded tree: descendants of rootPid plus anything
// still in its process group (catches orphans that reparented to launchd/init).
export function collectTreeRssKb(psOutput, rootPid) {
  const rows = psOutput
    .split('\n')
    .map((line) => line.trim().split(/\s+/u).map(Number))
    .filter((fields) => fields.length === 4 && fields.every((value) => Number.isFinite(value)));
  const childrenByParent = new Map();
  for (const [pid, ppid] of rows) {
    if (!childrenByParent.has(ppid)) childrenByParent.set(ppid, []);
    childrenByParent.get(ppid).push(pid);
  }
  const members = new Set([rootPid]);
  const queue = [rootPid];
  while (queue.length > 0) {
    for (const child of childrenByParent.get(queue.pop()) ?? []) {
      if (!members.has(child)) {
        members.add(child);
        queue.push(child);
      }
    }
  }
  let totalKb = 0;
  let processCount = 0;
  for (const [pid, , pgid, rssKb] of rows) {
    if (members.has(pid) || pgid === rootPid) {
      totalKb += rssKb;
      processCount += 1;
    }
  }
  return { totalKb, processCount };
}

// Spawn the exact requested argv without a shell interpolation layer.
export function guardedInvocation(command) {
  return {
    executable: command[0],
    args: command.slice(1),
  };
}

export function recordProcessTreeSample(state, psOutput, rootPid) {
  const { totalKb, processCount } = collectTreeRssKb(psOutput, rootPid);
  // `ps` reports KB, but the public record is whole MB. A real one-process
  // tree must never round down to the same zero used for "not measured".
  const rssMb = processCount > 0 && totalKb > 0 ? Math.max(1, Math.ceil(totalKb / 1024)) : 0;
  if (state.done) return { rssMb, processCount };
  state.peakRssMb = Math.max(state.peakRssMb, rssMb);
  state.peakProcessCount = Math.max(state.peakProcessCount, processCount);
  return { rssMb, processCount };
}

export function startProcessTreeMonitor(sample, { schedule = setInterval, intervalMs = POLL_MS } = {}) {
  sample();
  return schedule(sample, intervalMs);
}

function passthrough(command) {
  const child = spawn(command[0], command.slice(1), { stdio: 'inherit' });
  child.on('exit', (code, signal) => process.exit(code ?? (signal ? 1 : 0)));
  child.on('error', (error) => fail(`failed to start command: ${error.message}`));
}

function describeRefusal(decision, env) {
  const lines = [`admission refused (${decision.reason}): ${decision.message}`];
  const leases = readLeases(env, { reap: false });
  if (leases.length > 0) {
    lines.push('Guarded runs currently holding budget on this machine:');
    for (const lease of leases) {
      lines.push(`  - ${lease.label ?? 'run'} in ${lease.repo ?? 'unknown repo'} (${lease.harness ?? 'unknown'}, pid ${lease.pid}): ${lease.estimatedMb} MB reserved, ${lease.observedMb ?? 0} MB resident`);
    }
  }
  const consumers = topConsumers(5);
  if (consumers.length > 0) {
    lines.push(`Largest resident processes: ${consumers.map((entry) => `${entry.name} ${entry.rssMb} MB`).join(', ')}`);
  }
  lines.push('GitHub CI runs the underlying CI entrypoint directly, so pushing and letting GitHub verify is always available.');
  return lines.join('\n[guard] ');
}

function sleep(ms) {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

/**
 * Ask for admission and, if granted, take the lease — as ONE step.
 *
 * Deciding and then acquiring separately let two runs starting together both
 * measure the machine before either had written its lease, so both were
 * admitted against the same snapshot. The whole read/decide/write sequence runs
 * under the machine-wide admission mutex instead.
 *
 * Humans queue by default because an interactive run that starts 40 seconds
 * late is better than one that is refused; agents do not, because a blocked
 * agent should be pushing to CI, not sitting in a retry loop burning wall clock
 * and tokens. The mutex is released between attempts — holding it while
 * sleeping would serialize the waiting, not just the deciding.
 */
async function admit({ env, request, budget, leaseFields }) {
  const deadline = Date.now() + Math.max(0, request.waitS) * 1000;
  let announced = false;
  for (;;) {
    const attempt = await withAdmissionLock(env, () => {
      const memory = readMemoryStatus();
      const leases = readLeases(env);
      const decision = decideAdmission({
        budget,
        memory,
        leases,
        requestMb: request.reserveMb ?? request.ceilingMb,
        lanePeakMb: request.lanePeakMb,
      });
      if (!decision.granted && env.AGENT_GUARD_FORCE !== '1') return { decision, memory };
      const lease = acquireLease({ env, estimatedMb: request.reserveMb ?? request.ceilingMb, ...leaseFields });
      return { decision, memory, lease };
    });
    if (attempt.lease) {
      if (!attempt.decision.granted) note(`AGENT_GUARD_FORCE=1: proceeding despite ${attempt.decision.reason}. This is the human escape hatch; the machine is not being protected for this run.`);
      if (attempt.memory.degraded) note('WARNING: platform memory probes unavailable; availability is an estimate and swap is unknown.');
      return attempt;
    }
    if (Date.now() >= deadline) return { ...attempt, refused: true };
    if (!announced) {
      note(`waiting for machine memory (${attempt.decision.reason}); up to ${request.waitS}s. Ctrl-C to give up.`);
      announced = true;
    }
    await sleep(RETRY_MS);
  }
}

async function main() {
  let parsed;
  try {
    parsed = parseArgs(process.argv.slice(2));
  } catch (error) {
    fail(error instanceof Error ? error.message : String(error));
  }
  const { options, command } = parsed;
  if (command.length === 0) fail('no command given');

  // Policy is evaluated even for nested guarded scripts. A live parent lease
  // skips only duplicate admission; it cannot authorize a heavy inner lane.
  // Lease ids are visible to same-user processes, so id knowledge alone also
  // cannot prove nesting: an unrecognised marker falls through to admission.
  const resolved = resolveInvocation({ options, command, env: process.env });
  if (resolved.action === 'refuse') fail(resolved.policy.message);
  if (resolved.action === 'passthrough') return passthrough(command);
  if (process.platform === 'win32') {
    note('WARNING: guard unsupported on win32; running unguarded.');
    return passthrough(command);
  }
  const ps = psExecutable();
  if (ps === null) fail('guard requires ps at /bin/ps or /usr/bin/ps to enforce process-group memory limits');

  const { commandLine } = resolved;

  const initialMemory = readMemoryStatus();
  const totalMb = initialMemory.totalMb;
  const budget = deriveBudgetForMemory(initialMemory);
  const request = applyAutomaticLaneHistoryPolicy(resolveRequest(options, process.env, budget));
  if (request.clamped) {
    note(`ceiling clamped: requested ${request.requestedMb} MB, machine cap is ${request.ceilingMb} MB (${totalMb} MB total RAM, ${budget.machineBudgetMb} MB machine budget). Tightening is allowed; loosening is not.`);
  }

  const worktree = process.cwd();
  // One diagnostic location per clone, under the machine state directory: a
  // run from any cwd of any linked worktree shares it, and nothing is ever
  // written into the checkout itself (#239).
  const { guardDir, lastRunPath, lastRunDisplayPath } = guardDiagnosticPaths(worktree, { env: process.env });
  const nodeOptions = [process.env.NODE_OPTIONS, `--max-old-space-size=${request.heapMb}`].filter(Boolean).join(' ');
  // The live lease id is added after admission so nested guarded commands can
  // prove they belong to this process group.
  const childEnvironment = { ...process.env, NODE_OPTIONS: nodeOptions };

  const { decision, memory, lease, refused } = await admit({
    env: process.env,
    request,
    budget,
    leaseFields: {
      label: options.label,
      repo: path.basename(worktree),
      worktree,
      harness: harnessName(process.env),
      command: commandLine,
    },
  });
  if (refused) fail(describeRefusal(decision, process.env));

  // Created after admission so a refused run writes nothing anywhere.
  mkdirSync(guardDir, { recursive: true });

  const startedAt = Date.now();
  const invocation = guardedInvocation(command);
  const child = spawn(invocation.executable, invocation.args, {
    stdio: 'inherit',
    detached: true, // new process group; kill(-pid) reaches every descendant
    // The marker carries this run's lease id, so a child can prove it is
    // nested inside a real guarded run rather than merely asserting it.
    env: { ...childEnvironment, AGENT_GUARDED: lease.id },
  });

  // The admitted reservation must follow the detached group, not this
  // wrapper. A hard-killed wrapper can leave its descendants alive; binding
  // the lease to their group keeps that memory charged until the group exits.
  if (!retargetLease(lease, { pid: child.pid, processGroupId: child.pid })) {
    try {
      if (Number.isInteger(child.pid)) process.kill(-child.pid, 'SIGKILL');
    } catch {
      // Spawn may have failed before the group existed.
    }
    releaseLease(lease);
    fail('failed to bind the admission lease to the guarded process group');
  }

  const state = {
    peakRssMb: 0,
    peakProcessCount: 0,
    reason: null,
    termAt: null,
    done: false,
    polling: false,
    lastBeat: 0,
    monitorFailures: 0,
    killTimer: null,
  };

  const killGroup = (signal) => {
    try {
      process.kill(-child.pid, signal);
    } catch {
      // Group already gone.
    }
  };

  const terminate = (reason) => {
    if (state.termAt !== null) return;
    state.reason = reason;
    state.termAt = Date.now();
    note(
      `${reason}: terminating process group of "${options.label}" ` +
        `(peak RSS ${state.peakRssMb} MB, ceiling ${request.ceilingMb} MB, ${Math.round((Date.now() - startedAt) / 1000)}s elapsed).`,
    );
    killGroup('SIGTERM');
    state.killTimer = setTimeout(() => killGroup('SIGKILL'), SIGKILL_AFTER_MS);
  };

  const timeoutTimer =
    request.timeoutS > 0 ? setTimeout(() => terminate('timeout'), request.timeoutS * 1000) : null;

  const sampleTree = () => {
    if (state.polling) return;
    state.polling = true;
    execFile(ps, ['-axo', 'pid=,ppid=,pgid=,rss='], { maxBuffer: 16 * 1024 * 1024 }, (error, stdout) => {
      state.polling = false;
      if (state.done) return;
      if (error) {
        state.monitorFailures += 1;
        if (state.monitorFailures >= MAX_MONITOR_FAILURES) terminate('monitor-unavailable');
        return;
      }
      state.monitorFailures = 0;
      const sample = recordProcessTreeSample(state, stdout, child.pid);
      const { rssMb } = sample;
      // Report real usage so other repos' arbiters stop counting this run's
      // reservation twice (see lib/budget.mjs unmaterializedMb).
      if (Date.now() - state.lastBeat > HEARTBEAT_MS) {
        state.lastBeat = Date.now();
        heartbeatLease(lease, rssMb);
      }
      if (state.termAt !== null) {
        if (Date.now() - state.termAt > SIGKILL_AFTER_MS || rssMb > request.ceilingMb * HARD_KILL_FACTOR) killGroup('SIGKILL');
        return;
      }
      if (rssMb > request.ceilingMb) {
        terminate('rss-limit');
        return;
      }
      if (request.timeoutS > 0 && Date.now() - startedAt > request.timeoutS * 1000) {
        terminate('timeout');
        return;
      }
    });
  };
  const poll = startProcessTreeMonitor(sampleTree);

  for (const signal of ['SIGINT', 'SIGTERM', 'SIGHUP']) {
    process.on(signal, () => {
      terminate(`signal:${signal}`);
    });
  }

  child.on('exit', (code, signal) => {
    state.done = true;
    clearInterval(poll);
    if (timeoutTimer !== null) clearTimeout(timeoutTimer);
    if (state.killTimer !== null) clearTimeout(state.killTimer);
    killGroup('SIGKILL'); // sweep any stragglers left in the group
    releaseLease(lease);
    const record = {
      label: options.label,
      command: commandLine,
      startedAt: new Date(startedAt).toISOString(),
      durationMs: Date.now() - startedAt,
      peakRssMb: state.peakRssMb,
      peakProcessCount: state.peakProcessCount,
      ceilingMb: request.ceilingMb,
      reservedMb: request.reserveMb,
      requestedMb: request.requestedMb,
      clamped: request.clamped,
      heapMb: request.heapMb,
      timeoutS: request.timeoutS,
      machine: { totalMb, budget, admittedWith: { availableMb: memory.availableMb, swapUsedMb: memory.swapUsedMb, outstandingMb: decision.outstandingMb } },
      exitCode: code,
      signal,
      terminationReason: state.reason ?? 'completed',
    };
    try {
      writeFileSync(lastRunPath, `${JSON.stringify(record, null, 2)}\n`);
      appendFileSync(path.join(guardDir, 'history.jsonl'), `${JSON.stringify(record)}\n`);
    } catch {
      // Diagnostics are best-effort.
    }
    if (state.reason !== null) {
      note(`run failed: ${state.reason} (diagnostics in ${lastRunDisplayPath}).`);
      process.exit(1);
    }
    process.exit(code ?? (signal ? 1 : 0));
  });

  child.on('error', (error) => {
    clearInterval(poll);
    if (timeoutTimer !== null) clearTimeout(timeoutTimer);
    if (state.killTimer !== null) clearTimeout(state.killTimer);
    releaseLease(lease);
    fail(`failed to start command: ${error.message}`);
  });
}

// Strict comparison against the executed entrypoint (not an `endsWith` on this
// module's own filename, which is true for every importer and would leave a
// test awaiting a command that never comes).
const entry = process.argv[1] ? path.resolve(process.argv[1]) : null;
if (entry && import.meta.filename === entry) {
  try {
    await main();
  } catch (error) {
    fail(error instanceof Error ? error.message : String(error));
  }
}
