// The budget formula and the admission decision. Pure: every input is passed
// in, so the whole policy is testable at any machine size without owning that
// machine.
//
// Everything derives from `os.totalmem()`. The rule that makes this safe under
// a rollout is one-directional: a caller may always ask for LESS than the cap
// and gets it; asking for more is clamped down, never honoured. So a stale
// `--rss-mb 8192` in a consuming repo's npm script becomes 3072 on an 8 GB
// machine instead of an unreachable ceiling, and the same script stays 8192 on
// a 32 GB machine where that is a sane number.

export const SWAP_REFUSE_RATIO = 0.5;

/**
 * Machine-derived limits.
 *
 * `reserveMb` is what guarded runs may never plan to use: the OS, the
 * compositor, the browser and the editor the owner is looking at. A quarter of
 * RAM with a 1.5 GB floor is deliberately generous on small machines, because
 * the small machine is the one that bricks — on 8 GB the desktop alone was
 * measured holding ~2 GB before any test started.
 *
 * `maxRunMb` — half the remaining budget — is the statable rule that no single
 * run may reserve so much that a second run can never be admitted. It is the
 * cap that `--rss-mb 8192` gets clamped to.
 */
export function deriveBudget(totalMb) {
  const reserveMb = Math.max(1536, Math.round(totalMb * 0.25));
  const machineBudgetMb = Math.max(512, totalMb - reserveMb);
  return {
    totalMb,
    reserveMb,
    machineBudgetMb,
    maxRunMb: Math.max(512, Math.floor(machineBudgetMb / 2)),
    // The hard floor of real, measured availability that must survive the run.
    // Cross this and the machine starts trading pages for progress.
    availabilityFloorMb: Math.max(768, Math.round(totalMb * 0.125)),
    // A run at or below this is exempt from the swap gate (never from the
    // headroom floor). A lint or unit lane is not what turns a thrashing
    // machine into a frozen one — three Electron workers are — and a guard that
    // refuses every command on a busy machine is a guard people switch off,
    // which protects nothing at all.
    lightRunMb: Math.max(256, Math.round(Math.max(768, Math.round(totalMb * 0.125)) / 2)),
  };
}

/**
 * Clamp a requested ceiling to the machine cap.
 *
 * Tightening is always allowed; loosening never is. A caller that asks for
 * nothing gets the cap.
 */
export function clampCeiling(requestedMb, budget) {
  const requested = Number.isFinite(requestedMb) && requestedMb > 0 ? Math.round(requestedMb) : budget.maxRunMb;
  return {
    ceilingMb: Math.max(256, Math.min(requested, budget.maxRunMb)),
    clamped: requested > budget.maxRunMb,
    requestedMb: requested,
  };
}

/**
 * How much of a lease has NOT yet materialized as resident memory.
 *
 * This is what stops the double-count that would otherwise make the arithmetic
 * useless: a run that already holds 800 MB has had that 800 MB subtracted from
 * `availableMb` by the kernel, so only the remainder of its reservation is
 * still a claim on the future. Leases heartbeat their observed tree RSS
 * precisely so this number shrinks as a run warms up.
 */
export function unmaterializedMb(leases) {
  return leases.reduce((total, lease) => total + Math.max(0, lease.estimatedMb - (lease.observedMb ?? 0)), 0);
}

export function outstandingMb(leases) {
  return leases.reduce((total, lease) => total + lease.estimatedMb, 0);
}

/**
 * Grant or refuse, with the reason and the arithmetic that produced it.
 *
 * Order matters: the checks run most-diagnostic first, so the message a user
 * sees names the condition they can actually act on. Swap pressure is checked
 * before headroom because a swapping machine reports plausible-looking
 * "available" memory right up until it stops responding.
 */
export function decideAdmission({ budget, memory, leases = [], requestMb }) {
  const outstanding = outstandingMb(leases);
  const unmaterialized = unmaterializedMb(leases);
  const projectedFreeMb = memory.availableMb - unmaterialized - requestMb;
  const swapUsedRatio = memory.swapTotalMb > 0 ? memory.swapUsedMb / memory.swapTotalMb : 0;
  const arithmetic = {
    requestMb,
    outstandingMb: outstanding,
    unmaterializedMb: unmaterialized,
    availableMb: memory.availableMb,
    projectedFreeMb,
    swapUsedRatio: Number(swapUsedRatio.toFixed(3)),
    budget,
  };

  if (memory.swapTotalMb > 0 && swapUsedRatio >= SWAP_REFUSE_RATIO && requestMb > budget.lightRunMb) {
    return {
      granted: false,
      reason: 'swap-pressure',
      message:
        `swap is ${Math.round(swapUsedRatio * 100)}% committed (${memory.swapUsedMb} MB of ${memory.swapTotalMb} MB). ` +
        'The machine is already trading pages for progress; starting another memory-heavy run is what turns that into a freeze. ' +
        `Runs reserving up to ${budget.lightRunMb} MB are still admitted.`,
      ...arithmetic,
    };
  }

  if (projectedFreeMb < budget.availabilityFloorMb) {
    return {
      granted: false,
      reason: 'insufficient-headroom',
      message:
        `only ${memory.availableMb} MB is available and ${unmaterialized} MB is already promised to ${leases.length} running ` +
        `guarded run(s); admitting ${requestMb} MB would leave ${projectedFreeMb} MB against a ${budget.availabilityFloorMb} MB floor.`,
      ...arithmetic,
    };
  }

  if (outstanding + requestMb > budget.machineBudgetMb) {
    return {
      granted: false,
      reason: 'machine-budget',
      message:
        `${outstanding} MB of the ${budget.machineBudgetMb} MB machine budget is already leased; ` +
        `${requestMb} MB more would oversubscribe it. This budget is shared by every repo, worktree and agent on this machine.`,
      ...arithmetic,
    };
  }

  return { granted: true, reason: 'granted', message: null, ...arithmetic };
}
