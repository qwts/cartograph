#!/usr/bin/env node

// Operator surface for the machine-scoped guard: see what holds the machine
// and dry-run an admission decision. The legacy grant command remains only to
// fail closed with an explanation; same-user files cannot prove human intent.
//
// Usage:
//   node tools/agent-guard/arbiter.mjs status
//   node tools/agent-guard/arbiter.mjs doctor
//   node tools/agent-guard/arbiter.mjs check [--rss-mb N] [--label name]
//   node tools/agent-guard/arbiter.mjs grant <lane> [--minutes N]
//   node tools/agent-guard/arbiter.mjs revoke <lane>

import path from 'node:path';
import process from 'node:process';

import { clampCeiling, decideAdmission, deriveBudgetForMemory } from './lib/budget.mjs';
import { readLeases } from './lib/leases.mjs';
import { HEAVY_LANES, harnessName, isAgentSession, isCi, listGrants, revokeGrant, writeGrant } from './lib/policy.mjs';
import { machineToken, stateDir } from './lib/protocol.mjs';
import { readMemoryStatus, topConsumers } from './lib/system-memory.mjs';

function out(line = '') {
  process.stdout.write(`${line}\n`);
}

function flag(argv, name) {
  const index = argv.indexOf(name);
  return index >= 0 && index + 1 < argv.length ? argv[index + 1] : null;
}

function status() {
  const memory = readMemoryStatus();
  const budget = deriveBudgetForMemory(memory);
  const leases = readLeases(process.env, { reap: false });
  const grants = listGrants();

  out(`machine        ${budget.totalMb} MB RAM, reserve ${budget.reserveMb} MB, budget ${budget.machineBudgetMb} MB, max/run ${budget.maxRunMb} MB`);
  out(`available      ${memory.availableMb} MB (${memory.source}${memory.degraded ? ', DEGRADED' : ''}), floor ${budget.availabilityFloorMb} MB`);
  out(
    memory.swapTotalMb > 0
      ? `swap           ${memory.swapUsedMb} MB of ${memory.swapTotalMb} MB used (${Math.round((memory.swapUsedMb / memory.swapTotalMb) * 100)}%)`
      : 'swap           none configured or unreadable',
  );
  out(`state          ${stateDir()}`);
  out();
  if (leases.length === 0) out('leases         none');
  else {
    out(`leases         ${leases.length}`);
    for (const lease of leases) {
      out(`  ${lease.label ?? 'run'}  repo=${lease.repo ?? '?'}  harness=${lease.harness ?? '?'}  pid=${lease.pid}  reserved=${lease.estimatedMb} MB  resident=${lease.observedMb ?? 0} MB  since=${lease.grantedAt}`);
    }
  }
  out(grants.length === 0 ? 'legacy grants  none' : `legacy grants  ${grants.map((grant) => `${grant.laneId} until ${grant.expiresAt} (revoke)`).join(', ')}`);
  const consumers = topConsumers(5);
  if (consumers.length > 0) {
    out();
    out(`holding memory ${consumers.map((entry) => `${entry.name} ${entry.rssMb} MB`).join(', ')}`);
  }
}

function check(argv) {
  const memory = readMemoryStatus();
  const budget = deriveBudgetForMemory(memory);
  const requested = Number(flag(argv, '--rss-mb'));
  const ceiling = clampCeiling(Number.isFinite(requested) ? requested : budget.maxRunMb, budget);
  const decision = decideAdmission({
    budget,
    memory,
    leases: readLeases(process.env, { reap: false }),
    requestMb: ceiling.ceilingMb,
  });
  out(`request        ${ceiling.requestedMb} MB${ceiling.clamped ? ` → clamped to ${ceiling.ceilingMb} MB` : ''}`);
  out(`verdict        ${decision.granted ? 'GRANT' : `REFUSE (${decision.reason})`}`);
  if (decision.message) out(`               ${decision.message}`);
  out(`arithmetic     available ${decision.availableMb} MB − unmaterialized ${decision.unmaterializedMb} MB − request ${decision.requestMb} MB = ${decision.projectedFreeMb} MB (floor ${budget.availabilityFloorMb} MB)`);
  out(`               leased ${decision.outstandingMb} MB of ${budget.machineBudgetMb} MB machine budget`);
  return decision.granted ? 0 : 1;
}

function doctor() {
  const memory = readMemoryStatus();
  const problems = [];
  out(`platform       ${process.platform}`);
  out(`probe          ${memory.source}`);
  out(`state dir      ${stateDir()}`);
  out(`machine token  ${machineToken().slice(0, 8)}…`);
  out(`session        ${isAgentSession(process.env) ? 'agent' : 'human'}${isCi(process.env) ? ' (CI — guard is a no-op)' : ''}`);
  out(`heavy lanes    ${HEAVY_LANES.map((lane) => lane.id).join(', ')}`);
  if (memory.degraded) problems.push('platform memory probes unavailable — availability is estimated and swap is unknown');
  if (memory.swapTotalMb === 0 && process.platform !== 'win32') problems.push('swap total reads as 0 — swap-pressure refusal cannot fire');
  out();
  if (problems.length === 0) out('all checks passed');
  else for (const problem of problems) out(`warn  ${problem}`);
  return 0;
}

function grant(argv) {
  const laneId = argv[1];
  const lane = HEAVY_LANES.find((entry) => entry.id === laneId);
  if (!lane) {
    process.stderr.write(`unknown lane ${JSON.stringify(laneId ?? '')}; expected one of ${HEAVY_LANES.map((entry) => entry.id).join(', ')}\n`);
    return 1;
  }
  // Defense in depth: the command hook already denies this invocation from
  // agent sessions, but the arbiter refuses marked callers on its own too.
  // The grant is honored by lane policy only in an UNMARKED session, so a
  // grant minted here is owner evidence, not agent self-approval (#180).
  if (harnessName(process.env) !== 'human') {
    process.stderr.write(
      `grants cannot be minted from an agent session (${harnessName(process.env)} markers present). ` +
        "Run this from the owner's own terminal.\n",
    );
    return 1;
  }
  const requested = Number(argv[2]);
  const minutes = Number.isFinite(requested) && requested > 0 ? Math.min(Math.round(requested), 240) : 30;
  const written = writeGrant({ laneId: lane.id, minutes });
  out(`granted ${lane.id} for ${minutes} minutes (expires ${written.expiresAt})`);
  out('Run the lane through its guarded entrypoint; lease, ceiling, timeout, and admission enforcement still apply.');
  return 0;
}

function revoke(argv) {
  const laneId = argv[1];
  const lane = HEAVY_LANES.find((entry) => entry.id === laneId);
  if (!lane) {
    process.stderr.write(`unknown lane ${JSON.stringify(laneId ?? '')}; expected one of ${HEAVY_LANES.map((entry) => entry.id).join(', ')}\n`);
    return 1;
  }
  if (!revokeGrant(lane.id)) {
    process.stderr.write(`no active grant for ${lane.id}\n`);
    return 1;
  }
  out(`revoked ${lane.id}`);
  return 0;
}

function main() {
  const argv = process.argv.slice(2);
  switch (argv[0]) {
    case 'status':
    case undefined:
      status();
      return 0;
    case 'check':
      return check(argv);
    case 'doctor':
      return doctor();
    case 'grant':
      return grant(argv);
    case 'revoke':
      return revoke(argv);
    default:
      process.stderr.write(`unknown command ${JSON.stringify(argv[0])}\n`);
      return 1;
  }
}

const entry = process.argv[1] ? path.resolve(process.argv[1]) : null;
if (entry && import.meta.filename === entry) process.exit(main());
