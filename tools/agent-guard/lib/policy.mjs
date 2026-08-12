// Who is asking, what they are asking to run, and whether they are allowed to.
//
// One authorization question: is this an agent? Agents do not get the heavy
// local suites. They push and let GitHub CI verify, which is the authoritative
// lane regardless. The wrapper does not try to infer whether it is running in
// CI because no available process-local evidence proves process locality.
// Heavy lanes are never delegated back to an agent process. A same-user file
// or local token is forgeable by that process and cannot prove human approval.

import { readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';

import { ensureStateDirs, grantsDir } from './protocol.mjs';

/**
 * Informational CI-marker detection. Broad by design, but never a trust or
 * authorization decision: process-local evidence cannot prove process locality.
 */
export function isCi(env = process.env) {
  return (
    env.CI === 'true' ||
    env.CI === '1' ||
    env.GITHUB_ACTIONS === 'true' ||
    env.CONTINUOUS_INTEGRATION === 'true' ||
    typeof env.BUILDKITE === 'string' ||
    typeof env.GITLAB_CI === 'string' ||
    typeof env.JENKINS_URL === 'string'
  );
}

/**
 * Is an agent driving this shell?
 *
 * Mirrors the *narrow* markers agent-bot-identity's `detectAgentHarness` uses
 * — the ones that mean "an agent process", not merely "a terminal opened
 * inside an editor" — but inverts the default. That inversion is the whole
 * point and is why this is not a duplicated fact under ENG-0006: identity
 * resolution must not misattribute a human's commit to a bot, so it answers
 * `null` when unsure; admission control must not hand a heavy suite to an
 * unrecognised agent, so it answers `true` when unsure. Same evidence,
 * opposite and deliberate failure directions.
 *
 * Absence of a marker is not human authentication: an agent-controlled script
 * can unset ordinary environment variables before invoking the wrapper. Local
 * callers therefore fail closed. GitHub workflows run the underlying CI
 * entrypoints directly instead of asking this local wrapper to infer where it
 * is executing; a human owner can run the underlying lane directly too.
 */
export function isAgentSession(_env = process.env) {
  return true;
}

// A regex for the `<NAME>_AGENT` marker shape that every agent runtime sets in
// the process it drives (Devin's DEVIN_AGENT, Windsurf's WINDSURF_AGENT). Keyed
// ONLY on the `_AGENT` suffix — never on ambient editor variables like VSCODE_*,
// CURSOR_TRACE_ID, or WINDSURF_IDE_TYPE, which mean an editor is open, not that
// an agent is driving (agent-bot-identity#12). This is what keeps a human's
// terminal from being misread as an agent while still catching harnesses the
// registry does not know yet.
const AGENT_MARKER = /^[A-Z][A-Z0-9_]*_AGENT$/u;

export function harnessName(env = process.env) {
  if (env.CLAUDECODE === '1' || (typeof env.CLAUDE_CODE_ENTRYPOINT === 'string' && env.CLAUDE_CODE_ENTRYPOINT !== '')) return 'claude';
  if (Object.keys(env).some((key) => key.startsWith('CODEX_'))) return 'codex';
  if (Object.keys(env).some((key) => key.startsWith('CURSOR_'))) return 'cursor';
  if (typeof env.AI_AGENT === 'string' && env.AI_AGENT !== '') return env.AI_AGENT.toLowerCase().split(/[^a-z]/u)[0] || 'agent';
  // Any other `<NAME>_AGENT` marker names an unregistered harness (#142). A
  // Devin/Windsurf session used to fall through to 'human' here, and that
  // matters more than a label: the owner-grant path and `arbiter grant` both
  // key on harnessName === 'human', so an unrecognised agent resolving to
  // 'human' could mint and use its own heavy-lane grant. Deterministic pick
  // (sorted) when several are present.
  const marker = Object.keys(env)
    .filter((key) => AGENT_MARKER.test(key) && typeof env[key] === 'string' && env[key] !== '')
    .sort()[0];
  if (marker) return marker.slice(0, -'_AGENT'.length).toLowerCase().replace(/_/gu, '-') || 'agent';
  return 'human';
}

/**
 * The lanes that caused the incident, each with the reason it is heavy — a
 * refusal that explains itself is one an agent can act on instead of retrying.
 *
 * Matched against a guard label OR a raw command line, so the same table backs
 * both the wrapper and the pre-execution hook.
 */
export const HEAVY_LANES = [
  { id: 'e2e', pattern: /\be2e\b|\bplaywright\b/u, why: 'every Playwright worker boots a full Electron app' },
  { id: 'stories', pattern: /\bstories\b|\bstorybook\b|\btest-storybook\b/u, why: 'the Storybook build plus a browser-driven test run' },
  { id: 'perf', pattern: /\bperf\b|\bbenchmark\b/u, why: 'the perf harness seeds a large synthetic library' },
  { id: 'coverage', pattern: /\bcov\b|\bcoverage\b|(^|[\s;(&|])(npx\s+)?c8\s/u, why: 'coverage instrumentation runs the whole suite with extra retention' },
  { id: 'full-ci', pattern: /(^|[\s;(&|])npm\s+run\s+ci(?![\w:-])|^ci$/u, why: 'the full gate chains lint, typecheck, the suites and a build' },
];

export function classifyLane(text) {
  if (typeof text !== 'string' || text === '') return null;
  return HEAVY_LANES.find((lane) => lane.pattern.test(text)) ?? null;
}

// --- Legacy grant artifacts -------------------------------------------------
//
// These helpers remain only so old artifacts can expire, be listed, and be
// revoked during rollout. They are never consulted for admission: an agent
// running as the same OS user can write the file directly, so it cannot
// authenticate human intent.

export function grantPath(laneId, env = process.env) {
  return path.join(grantsDir(env), `${laneId}.json`);
}

export function writeGrant({ laneId, minutes = 30, env = process.env, now = Date.now() }) {
  ensureStateDirs(env);
  const grant = {
    laneId,
    grantedAt: new Date(now).toISOString(),
    expiresAt: new Date(now + minutes * 60_000).toISOString(),
  };
  writeFileSync(grantPath(laneId, env), `${JSON.stringify(grant, null, 2)}\n`);
  return grant;
}

export function readGrant(laneId, env = process.env, now = Date.now()) {
  let grant;
  try {
    grant = JSON.parse(readFileSync(grantPath(laneId, env), 'utf8'));
  } catch {
    return null;
  }
  const expires = Date.parse(grant?.expiresAt ?? '');
  if (!Number.isFinite(expires) || expires <= now) {
    try {
      rmSync(grantPath(laneId, env), { force: true });
    } catch {
      // Expired grant already gone.
    }
    return null;
  }
  return grant;
}

export function listGrants(env = process.env, now = Date.now()) {
  try {
    return readdirSync(grantsDir(env))
      .filter((name) => name.endsWith('.json'))
      .map((name) => readGrant(name.replace(/\.json$/u, ''), env, now))
      .filter(Boolean);
  } catch {
    return [];
  }
}

export function revokeGrant(laneId, env = process.env) {
  try {
    rmSync(grantPath(laneId, env));
    return true;
  } catch {
    return false;
  }
}

/**
 * The agent-vs-human gate, resolved.
 *
 * Classification stays fail-closed: every local caller is an agent until
 * proven otherwise, and no marker's absence proves otherwise. The owner path
 * is a GRANT (#180): `arbiter.mjs grant <lane>` is deniable to agents by the
 * command hook, marker scrubbing is denied by the same hook, so the pair
 * "unmarked session + live grant on disk" is evidence the owner opened this
 * window from their own terminal. A granted run stays fully guarded — lease,
 * ceiling, timeout, and admission enforcement all still apply; the grant
 * only answers WHO. Marked agent sessions are never admitted by a grant.
 */
export function evaluateLanePolicy({ label, command, env = process.env, now = Date.now() }) {
  const lane = classifyLane(label) ?? classifyLane(command);
  if (!lane) return { allowed: true, lane: null };
  if (!isAgentSession(env)) return { allowed: true, lane, actor: 'human' };
  if (harnessName(env) === 'human' && readGrant(lane.id, env, now) !== null) {
    return { allowed: true, lane, actor: 'owner-grant' };
  }
  return {
    allowed: false,
    lane,
    actor: 'agent',
    message:
      `The "${lane.id}" lane is a heavy local suite (${lane.why}) and agents do not run it on this machine by default. ` +
      'Push the branch and let GitHub CI verify — the workflow invokes its underlying CI entrypoint directly. ' +
      `If a local run is genuinely required, the owner can open a window from their own (non-agent) terminal with \`node tools/agent-guard/arbiter.mjs grant ${lane.id}\` and run this same guarded entrypoint — enforcement stays on. Agent sessions cannot mint or use grants.`,
  };
}
