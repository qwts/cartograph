// The budget formula and the admission decision. Pure: every input is passed
// in, so the whole policy is testable at any machine size without owning that
// machine.
//
// Everything derives from the effective machine total (clamped to a container
// cgroup limit when present). The rule that makes this safe under
// a rollout is one-directional: a caller may always ask for LESS than the cap
// and gets it; asking for more is clamped down, never honoured. So a stale
// `--rss-mb 8192` in a consuming repo's npm script becomes 3072 on an 8 GB
// machine instead of an unreachable ceiling, and the same script stays 8192 on
// a 32 GB machine where that is a sane number.

export const SWAP_REFUSE_RATIO = 0.5;

// The per-lane hard cap (#179/#180). The incident this guard exists for was a
// single test lane ballooning to ~100 GB and taking the machine into swap; no
// sanctioned local lane needs more than this, and the cap is enforced on the
// running process tree by run-guarded, not merely assumed. A machine-scaled
// "half the budget" cap reserved 9.2 GB on a 24 GB machine for a lane whose
// measured peak was under 2 GB — overstating demand until admission refused a
// healthy machine.
export const LANE_CAP_MB = 2048;

// macOS kern.memorystatus_vm_pressure_level values.
export const PRESSURE_NORMAL = 1;
export const PRESSURE_WARNING = 2;

/**
 * Machine-derived limits.
 *
 * `reserveMb` is what guarded runs may never plan to use: the OS, the
 * compositor, the browser and the editor the owner is looking at. A quarter of
 * RAM with a 1.5 GB floor is deliberately generous on small machines, because
 * the small machine is the one that bricks — on 8 GB the desktop alone was
 * measured holding ~2 GB before any test started.
 *
 * `maxRunMb` — half the remaining budget, never above the per-lane cap — is
 * the statable rule that no single run may reserve so much that a second run
 * can never be admitted, and that no lane may plan for more than the cap the
 * runner enforces. It is the cap that `--rss-mb 8192` gets clamped to.
 */
export function deriveBudget(totalMb) {
  const reserveMb = Math.max(1536, Math.round(totalMb * 0.25));
  const machineBudgetMb = Math.max(512, totalMb - reserveMb);
  const maxRunMb = Math.max(512, Math.min(LANE_CAP_MB, Math.floor(machineBudgetMb / 2)));
  return {
    totalMb,
    reserveMb,
    machineBudgetMb,
    maxRunMb,
    // The hard floor of real, measured availability that must survive the run.
    // Cross this and the machine starts trading pages for progress.
    availabilityFloorMb: Math.max(768, Math.round(totalMb * 0.125)),
    // A run at or below this is exempt from the pressure and swap gates
    // (never from the headroom floor). A lint or unit lane is not what turns
    // a thrashing machine into a frozen one — three Electron workers are —
    // and a guard that refuses every command on a busy machine is a guard
    // people switch off, which protects nothing at all. Kept strictly below
    // maxRunMb: on large machines floor/2 reaches the lane cap, and a
    // carve-out as big as the cap would exempt every run from both gates
    // (#203 review).
    lightRunMb: Math.max(256, Math.min(Math.round(Math.max(768, Math.round(totalMb * 0.125)) / 2), Math.floor(maxRunMb / 2))),
  };
}

export function deriveBudgetForMemory(memory) {
  return deriveBudget(memory.totalMb);
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
 * What a lane should *reserve*, as opposed to what it may never exceed.
 *
 * The ceiling is enforcement; the reservation is planning. When a lane has a
 * trustworthy recent measured peak, reserving that peak plus a conservative
 * margin stops a ~2 GB lane from booking the full cap and being refused on a
 * healthy machine (#180). Unknown lanes reserve the ceiling. History can only
 * lower the reservation, never raise the ceiling — a fabricated low peak
 * under-reserves the plan but the runner still kills the tree at the ceiling,
 * and heartbeats correct the arithmetic as real usage materializes.
 */
export function laneReservationMb(ceilingMb, peakMb) {
  if (!Number.isFinite(peakMb) || peakMb <= 0) return ceilingMb;
  const margin = Math.max(256, Math.round(peakMb * 0.25));
  return Math.min(ceilingMb, Math.max(512, peakMb + margin));
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
  return leases.reduce((total, lease) => {
    // Optional heartbeat data is untrusted filesystem input. Anything other
    // than a finite, non-negative number is treated as not materialized yet —
    // the conservative value that cannot manufacture headroom.
    const observedMb = Number.isFinite(lease.observedMb) && lease.observedMb >= 0 ? lease.observedMb : 0;
    return total + Math.max(0, lease.estimatedMb - observedMb);
  }, 0);
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
  const pressureLevel = Number.isFinite(memory.pressureLevel) ? memory.pressureLevel : null;
  const arithmetic = {
    requestMb,
    outstandingMb: outstanding,
    unmaterializedMb: unmaterialized,
    availableMb: memory.availableMb,
    projectedFreeMb,
    swapUsedRatio: Number(swapUsedRatio.toFixed(3)),
    pressureLevel,
    budget,
  };

  // The kernel's live pressure verdict outranks static swap arithmetic in
  // both directions (#180): warning/critical refuses a heavy run even when
  // page counts look plausible, and normal pressure means swap that was
  // committed during an earlier squeeze is history, not evidence — macOS
  // keeps it allocated after pressure subsides.
  if (pressureLevel !== null && pressureLevel >= PRESSURE_WARNING && requestMb > budget.lightRunMb) {
    return {
      granted: false,
      reason: 'memory-pressure',
      message:
        `the kernel reports memory pressure level ${pressureLevel} (2 = warning, 4 = critical). ` +
        'The machine is actively short of memory right now; starting another memory-heavy run makes that worse. ' +
        `Runs reserving up to ${budget.lightRunMb} MB are still admitted.`,
      ...arithmetic,
    };
  }

  const staticSwapApplies = pressureLevel === null || pressureLevel > PRESSURE_NORMAL;
  if (staticSwapApplies && memory.swapTotalMb > 0 && swapUsedRatio >= SWAP_REFUSE_RATIO && requestMb > budget.lightRunMb) {
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
