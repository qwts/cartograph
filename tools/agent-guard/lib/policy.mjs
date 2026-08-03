// Who is asking, what they are asking to run, and whether they are allowed to.
//
// Three separable questions, kept separate:
//   1. Is this CI?      → the entire mechanism is off. Hosted runners are
//                         disposable, isolated, and already bounded by
//                         job timeouts. They were never the problem and must
//                         not be slowed down.
//   2. Is this an agent? → agents do not get the heavy local suites. They push
//                         and let GitHub CI verify, which is the authoritative
//                         lane regardless.
//   3. Is there a grant? → the owner can hand an agent one heavy lane, briefly,
//                         out of band.

import { readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import path from 'node:path';

import { ensureStateDirs, grantsDir } from './protocol.mjs';

/**
 * CI detection. Broad on purpose — a false positive costs a hosted runner
 * nothing, while a false negative slows down every build in the fleet.
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
 * A human proves themselves by the absence of every agent marker, which a
 * plain terminal satisfies for free.
 */
export function isAgentSession(env = process.env) {
  if (env.AGENT_GUARD_ASSUME_HUMAN === '1') return false;
  const aiAgent = typeof env.AI_AGENT === 'string' ? env.AI_AGENT.toLowerCase() : '';
  return (
    env.CLAUDECODE === '1' ||
    (typeof env.CLAUDE_CODE_ENTRYPOINT === 'string' && env.CLAUDE_CODE_ENTRYPOINT !== '') ||
    Object.keys(env).some((key) => key.startsWith('CODEX_')) ||
    Object.keys(env).some((key) => key.startsWith('CURSOR_')) ||
    aiAgent !== ''
  );
}

export function harnessName(env = process.env) {
  if (env.CLAUDECODE === '1' || (typeof env.CLAUDE_CODE_ENTRYPOINT === 'string' && env.CLAUDE_CODE_ENTRYPOINT !== '')) return 'claude';
  if (Object.keys(env).some((key) => key.startsWith('CODEX_'))) return 'codex';
  if (Object.keys(env).some((key) => key.startsWith('CURSOR_'))) return 'cursor';
  if (typeof env.AI_AGENT === 'string' && env.AI_AGENT !== '') return env.AI_AGENT.toLowerCase().split(/[^a-z]/u)[0] || 'agent';
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

// --- Out-of-band grants ------------------------------------------------------
//
// The opt-in deliberately cannot be expressed in the command an agent runs.
// An env var would be self-serve: the agent that wants the heavy lane is the
// same agent writing the command line, so `AGENT_GUARD_ALLOW_HEAVY=1 npm run
// test:e2e` would be an opt-in it grants itself — the same hole
// `IMAGE_TRAIL_GUARD_DISABLE=` had until image-trail's hook started blocking
// the string. A grant is a file the owner creates, time-boxed, and the hook
// blocks agents from running the command that creates it.

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
    rmSync(grantPath(laneId, env), { force: true });
    return true;
  } catch {
    return false;
  }
}

/**
 * The agent-vs-human gate, resolved.
 *
 * Humans are never refused *by policy* — only clamped, and headroom-checked
 * like everything else. Agents are refused the heavy lanes unless the owner
 * has granted that specific lane, and told exactly how to proceed instead:
 * push and let CI verify.
 */
export function evaluateLanePolicy({ label, command, env = process.env, now = Date.now() }) {
  const lane = classifyLane(label) ?? classifyLane(command);
  if (!lane) return { allowed: true, lane: null };
  if (!isAgentSession(env)) return { allowed: true, lane, actor: 'human' };
  const grant = readGrant(lane.id, env, now);
  if (grant) return { allowed: true, lane, actor: 'agent', grant };
  return {
    allowed: false,
    lane,
    actor: 'agent',
    message:
      `The "${lane.id}" lane is a heavy local suite (${lane.why}) and agents do not run it on this machine by default. ` +
      'Push the branch and let GitHub CI verify — CI is the authoritative lane, and it is exempt from this guard. ' +
      `If a local run is genuinely required, ask the owner to run: node tools/agent-guard/arbiter.mjs grant ${lane.id} --minutes 30`,
  };
}
