// Conformance: this repo is still actually protected.
//
// Ships WITH the guard into every governed repo and must be wired into that
// repo's own test command. It is a contract test, not a style check, and it
// exists because the failure it checks for has already happened once: commit
// `e1d86f6a` ("governance: sync .codex from playbook-engineering") replaced
// `.claude/settings.json` wholesale in qwts/overlook and silently dropped the
// process-guard hook, leaving AGENTS.md and the docs describing an enforcement
// point that no longer existed. A wholesale rewrite now fails here first.
//
// Every assertion is cheap and offline. Nothing here starts a suite, spawns a
// worker, or allocates memory — a conformance test for a memory guard that
// needed memory to run would be self-defeating.

import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { after, describe, test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { evaluateCommand } from '../guard-agent-command.mjs';
import { clampCeiling, deriveBudget } from '../lib/budget.mjs';
import { isCi } from '../lib/policy.mjs';

// <repo>/tools/agent-guard/tests/this-file → <repo>
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..', '..');
const scratch = mkdtempSync(path.join(tmpdir(), 'agent-guard-conformance-'));
const env = { AGENT_GUARD_STATE_DIR: scratch };

after(() => rmSync(scratch, { recursive: true, force: true }));

function json(relative) {
  return JSON.parse(readFileSync(path.join(root, relative), 'utf8'));
}

function hookCommands(value) {
  return (value ?? []).flatMap((entry) => (entry.hooks ?? []).map((hook) => hook.command ?? ''));
}

describe('agent-guard conformance (ENG-0138)', () => {
  test('the guard implementation is present', () => {
    for (const file of [
      'tools/agent-guard/run-guarded.mjs',
      'tools/agent-guard/guard-agent-command.mjs',
      'tools/agent-guard/arbiter.mjs',
      'tools/agent-guard/lib/budget.mjs',
      'tools/agent-guard/lib/leases.mjs',
      'tools/agent-guard/lib/policy.mjs',
      'tools/agent-guard/lib/protocol.mjs',
      'tools/agent-guard/lib/system-memory.mjs',
    ]) {
      assert.ok(existsSync(path.join(root, file)), `${file} must exist — the guard is a governed file, not an optional one`);
    }
  });

  test('Claude Code registers the guard on Bash', () => {
    const settings = json('.claude/settings.json');
    const bash = (settings.hooks?.PreToolUse ?? []).find((entry) => entry.matcher === 'Bash');
    assert.ok(bash, '.claude/settings.json must register a PreToolUse hook matching Bash');
    assert.ok(
      hookCommands([bash]).some((command) => command.includes('guard-agent-command.mjs') && command.includes('--protocol=claude')),
      'the Bash PreToolUse hook must invoke tools/agent-guard/guard-agent-command.mjs --protocol=claude',
    );
  });

  test('Cursor registers the same guard on its own protocol', () => {
    const hooks = json('.cursor/hooks.json');
    assert.ok(
      (hooks.hooks?.beforeShellExecution ?? []).some((hook) => (hook.command ?? '').includes('guard-agent-command.mjs') && (hook.command ?? '').includes('--protocol=cursor')),
      '.cursor/hooks.json must invoke the guard with --protocol=cursor',
    );
  });

  test('Codex registers it too — a guard only one harness honours is not a guard', () => {
    const hooks = json('.codex/hooks.json');
    assert.ok(
      hookCommands(hooks.hooks?.PreToolUse).some((command) => command.includes('guard-agent-command.mjs') && command.includes('--protocol=codex')),
      '.codex/hooks.json must invoke the guard with --protocol=codex',
    );
  });

  test('the worktree-identity hook survives alongside it', () => {
    assert.ok(
      hookCommands(json('.claude/settings.json').hooks?.WorktreeCreate).some((command) => command.includes('claude-worktree-create')),
      'WorktreeCreate must still mint the per-worktree bot identity (ENG-0016)',
    );
  });

  test('tool permissions stay least-privilege: no blanket Bash or wildcard allow', () => {
    const allow = (json('.claude/settings.json').permissions?.allow ?? []).map(String);
    for (const rule of allow) {
      assert.notEqual(rule, '*', 'a wildcard allow defeats the permission prompt entirely');
      assert.notEqual(rule, 'Bash', 'blanket Bash allow defeats the guard hook');
      assert.doesNotMatch(rule, /^Bash\(\*\)$/u, 'blanket Bash allow defeats the guard hook');
    }
  });

  test('the heavy lanes are not pre-approved back open in the permission allow-list', () => {
    // An `allow` entry does not beat a deny hook, but it records an intent that
    // contradicts the record — and it is how these lanes became routine for
    // agents in the first place.
    const allow = (json('.claude/settings.json').permissions?.allow ?? []).map(String);
    for (const rule of allow) {
      assert.doesNotMatch(rule, /npm run (ci|test:e2e|test:stories|test:perf|test:cov)/u, `${rule} pre-approves a heavy lane that ENG-0138 denies agents by default`);
    }
  });

  test('the guard denies the commands from the incident', () => {
    for (const command of ['npm run ci', 'npm run test:e2e', 'npm run test:stories:ci', 'npx playwright test', 'test-storybook --ci']) {
      assert.equal(evaluateCommand(command, { env }).allow, false, `expected the guard to deny: ${command}`);
    }
  });

  test('the guard denies tampering with its own controls', () => {
    for (const command of ['AGENT_GUARD_FORCE=1 npm run test:dom', 'AGENT_GUARD_ASSUME_HUMAN=1 npm run test:dom', 'node tools/agent-guard/arbiter.mjs grant e2e']) {
      assert.equal(evaluateCommand(command, { env }).allow, false, `expected the guard to deny: ${command}`);
    }
  });

  test('ordinary work is untouched', () => {
    for (const command of ['npm run lint', 'npm run typecheck', 'git status --short', 'node tools/agent-guard/arbiter.mjs status']) {
      assert.equal(evaluateCommand(command, { env }).allow, true, `expected the guard to allow: ${command}`);
    }
  });

  test('ceilings derive from the machine, so no repo can pin an unreachable one', () => {
    // The defect this record fixes: a constant ceiling at or above total RAM
    // can never trip. Whatever a script asks for, the cap stays under the
    // machine's own size.
    for (const totalMb of [4096, 8192, 16384, 65536]) {
      const budget = deriveBudget(totalMb);
      assert.ok(budget.maxRunMb < totalMb, `a ${totalMb} MB machine must cap a run below its own RAM`);
      assert.equal(clampCeiling(totalMb * 4, budget).ceilingMb, budget.maxRunMb, 'an oversized request must clamp to the cap');
    }
  });

  test('CI is exempt, so this never slows a hosted runner down', () => {
    assert.equal(isCi({ GITHUB_ACTIONS: 'true' }), true);
    assert.equal(isCi({ CI: 'true' }), true);
    assert.equal(isCi({}), false);
  });
});
