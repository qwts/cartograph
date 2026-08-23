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
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { after, describe, test } from 'node:test';
import { fileURLToPath } from 'node:url';

import { evaluateCommand, evaluateHookInput } from '../guard-agent-command.mjs';
import { clampCeiling, decideAdmission, deriveBudget } from '../lib/budget.mjs';
import { acquireLease, releaseLease, retargetLease } from '../lib/leases.mjs';
import { isCi } from '../lib/policy.mjs';
import { readMemoryStatus } from '../lib/system-memory.mjs';
import { resolveInvocation } from '../run-guarded.mjs';

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
    const hook = readFileSync(path.join(root, 'tools/agent-guard/guard-agent-command.mjs'), 'utf8');
    assert.match(hook, /userMessage: `Blocked by the machine memory guard \(see \$\{GUARD_GUIDE\}\)\.`/u);
  });

  test('monitor failures and timeouts terminate independently of RSS polling', () => {
    const runner = readFileSync(path.join(root, 'tools/agent-guard/run-guarded.mjs'), 'utf8');
    assert.match(runner, /monitorFailures >= MAX_MONITOR_FAILURES\) terminate\('monitor-unavailable'\)/u);
    assert.match(runner, /setTimeout\(\(\) => terminate\('timeout'\), request\.timeoutS \* 1000\)/u);
    assert.match(runner, /state\.killTimer = setTimeout\(\(\) => killGroup\('SIGKILL'\)/u);
  });

  test('the runner neither consumes nor records automatic lane peak history', () => {
    const runner = readFileSync(path.join(root, 'tools/agent-guard/run-guarded.mjs'), 'utf8');
    assert.doesNotMatch(runner, /\breadLanePeakMb\b/u, 'pre-existing lane peaks must not grant an admission exemption');
    assert.doesNotMatch(runner, /\brecordLanePeak\b/u, 'a successful polled run must not seed automatic history');
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

  test('Copilot registers the guard in .github/hooks (#290)', () => {
    const hooks = json('.github/hooks/agent-guard.json');
    assert.ok(
      (hooks.hooks?.preToolUse ?? []).some((hook) => [hook.bash, hook.powershell].every((command) => (command ?? '').includes('guard-agent-command.mjs') && (command ?? '').includes('--protocol=copilot'))),
      '.github/hooks/agent-guard.json must invoke the guard with --protocol=copilot on preToolUse, for bash and powershell hosts alike',
    );
  });

  test('Windsurf (Devin desktop) registers it on pre_run_command (#290)', () => {
    const hooks = json('.windsurf/hooks.json');
    assert.ok(
      (hooks.hooks?.pre_run_command ?? []).some((hook) => [hook.command, hook.powershell].every((command) => (command ?? '').includes('guard-agent-command.mjs') && (command ?? '').includes('--protocol=windsurf'))),
      '.windsurf/hooks.json must invoke the guard with --protocol=windsurf on pre_run_command, for both shell hosts',
    );
  });

  test('the copilot and windsurf dialects deny through their own contracts', () => {
    const hook = path.join(root, 'tools/agent-guard/guard-agent-command.mjs');
    const spawnHook = (protocol, payload) => spawnSync(process.execPath, [hook, `--protocol=${protocol}`], {
      cwd: root,
      encoding: 'utf8',
      env: { ...process.env, ...env, AGENT_GUARDED: '' },
      input: JSON.stringify(payload),
    });

    // Copilot: JSON permissionDecision at the top level; silence means allow.
    // The CLI names the tool `shell` with object toolArgs; the coding agent
    // names it `bash` and JSON-encodes toolArgs — both must reach the guard.
    const copilotDeny = spawnHook('copilot', { toolName: 'shell', toolArgs: { command: 'npm run ci' }, cwd: root });
    assert.equal(copilotDeny.status, 0);
    assert.equal(JSON.parse(copilotDeny.stdout).permissionDecision, 'deny');
    const copilotAgentDeny = spawnHook('copilot', { toolName: 'bash', toolArgs: JSON.stringify({ command: 'npm run ci' }), cwd: root });
    assert.equal(copilotAgentDeny.status, 0);
    assert.equal(JSON.parse(copilotAgentDeny.stdout).permissionDecision, 'deny');
    const copilotOtherTool = spawnHook('copilot', { toolName: 'str_replace_editor', toolArgs: {}, cwd: root });
    assert.equal(copilotOtherTool.status, 0);
    assert.equal(copilotOtherTool.stdout, '', 'a non-shell tool call is out of scope and must pass silently');

    // Windsurf: exit code 2 blocks; the reason reaches the user via show_output.
    const windsurfDeny = spawnHook('windsurf', { agent_action_name: 'pre_run_command', tool_info: { command_line: 'npm run ci', cwd: root } });
    assert.equal(windsurfDeny.status, 2);
    assert.match(windsurfDeny.stdout, /machine memory guard/u);
    const windsurfAllow = spawnHook('windsurf', { agent_action_name: 'pre_run_command', tool_info: { command_line: 'git status --short', cwd: root } });
    assert.equal(windsurfAllow.status, 0);
    assert.equal(windsurfAllow.stdout, '');
  });

  test('uninstalled identity adapters ship with the fleet harness', () => {
    const cursor = (json('.cursor/hooks.json').hooks?.beforeShellExecution ?? [])
      .map((hook) => hook.command ?? '');
    const claude = hookCommands(json('.claude/settings.json').hooks?.PreToolUse);
    const codex = hookCommands(json('.codex/hooks.json').hooks?.PreToolUse);
    for (const [path, commands] of [
      ['.cursor/hooks.json', cursor],
      ['.claude/settings.json', claude],
      ['.codex/hooks.json', codex],
    ]) {
      assert.ok(
        commands.some((command) => (
          command.includes('agent-bot agent-hook')
          && command.includes('AGENT_BOT_UNMANAGED_AUTHORS')
        )),
        `${path} must carry the uninstalled identity adapter (ENG-0128)`,
      );
    }
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

  test('hook scoping follows child directories without leaking subshell cwd', () => {
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'env -C /project npx vitest' }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'env --chdir=/project npm run ci' }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: '(cd /tmp); cd project && npm run ci' }, '/outside/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: "bash -c 'cd /project && npx vitest'" }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: "bash -c $'cd /project && npm run ci'" }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'command env -C /project npx vitest' }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'npm --prefix /project run ci' }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'pnpm --dir /project run ci' }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'target=/project; cd "$target" && npm run ci' }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'pushd /project && npm run ci' }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'ln -s /project /tmp/guard-link; cd /tmp/guard-link && npm run ci' }, '/project', { env }).allow, false);
    assert.equal(evaluateHookInput({ cwd: '/outside', command: 'tar -xf /tmp/link.tar -C /tmp; cd /tmp/link && npm run ci' }, '/project', { env }).allow, false);
  });

  test('executable indirection cannot bypass admission', () => {
    for (const command of [
      'corepack yarn run test:e2e',
      'yarn workspaces foreach -A npm run ci',
      'npm run ci [z-a]',
      'cat <(npx vitest)',
      "watch -n 1 'npx vitest'",
      'printf x | xargs npx vitest',
      '"/usr/bin/npm" run ci',
      'node node_modules/vitest/vitest.mjs run',
      'pnpm run "ci"',
      'env -i PATH=/usr/bin:/bin node tools/agent-guard/run-guarded.mjs --label test:e2e -- npm run test:e2e',
      'env - PATH=/usr/bin:/bin node tools/agent-guard/run-guarded.mjs --label test:e2e -- npm run test:e2e',
      "printf 'ci\\n' | xargs npm run",
      'find . -maxdepth 0 -exec npm run ci \\;',
      'command find . -maxdepth 0 -exec npx vitest \\;',
      "find . -maxdepth 0 -exec sh -c 'npm run ci' \\;",
      'lane=ci; npm run "$lane"',
      'runner=npm; $runner run ci',
      'name=vitest; npx $name',
      'if true; then npm run ci; fi',
      'case x in x) npx vitest;; esac',
      'coproc npx vitest',
      "printf 'npm run ci\\n' | sh",
      'find . -maxdepth 0 -exec bash -c npx\\ vitest \\;',
      'AI_AGENT= node tools/agent-guard/run-guarded.mjs -- npm test',
      'env -uCODEX_THREAD_ID node tools/agent-guard/run-guarded.mjs -- npm test',
      'key=AI_AGENT; env -u "$key" node tools/agent-guard/run-guarded.mjs -- npm test',
      'unset -- AI_AGENT; node tools/agent-guard/run-guarded.mjs -- npm test',
      'printf -v CI 1; export CI; node tools/agent-guard/run-guarded.mjs -- npm test',
      "payload='npm run ci'; bash -c \"$payload\"",
      'npx npm run ci',
      "find . '-exec' npm run ci \\;",
      'printf x | xargs --replace npm run ci',
      'node --require node:path node_modules/vitest/vitest.mjs run',
      "eval -- 'npx vitest'",
      "command bash <<'EOF'\nnpx vitest\nEOF",
      "node -e \"require('node:child_process').execSync('npm run ci')\"",
      "node -pe \"require('node:child_process').execSync('npx vitest')\"",
      "node -e\"require('node:child_process').execSync('npm run ci')\"",
      'script -q /dev/null -c "npm run ci"',
      "script -q /dev/null --command='npx vitest'",
      'cat <<\\EOF\nharmless\nEOF\nnpm run ci',
      "python3 -c \"import os; os.system('npm run ci')\"",
      'rm -rf ~/.cache/agent{-,}-guard/leases',
      'yarn workspace foo npm run ci',
      'taskset -c 0 npm run ci',
      'cat <<E"O"F\nharmless\nEOF\nnpm run ci',
      'cat <<-EOF\nharmless\n\tEOF\nnpm run ci',
      "cat <<$'E\\x4fF'\nharmless\nEOF\nnpm run ci",
      "python3 <<'PY'\nimport os\nos.system('npm run ci')\nPY",
      "node <<< \"require('node:child_process').execSync('npm run ci')\"",
      "node --import='data:text/javascript,export default 1' script.js",
      "php -B 'system(\"npm run ci\");' < /dev/null",
      'unset CLAUDECODE CLAUDE_CODE_ENTRYPOINT AI_AGENT',
      'rm -rf ~/.cache/agent-guard',
      'd=~/.cache/agent-guard; rm -rf "$d/leases"',
      "ba\\sh -c 'npm run ci'",
      "printf 'npm run ci\\n' > /tmp/lane.sh && bash /tmp/lane.sh",
      "printf 'child_process.execSync(\"npm run ci\")' > /tmp/lane.js && node /tmp/lane.js",
      "tar -cf /tmp/a.tar --checkpoint=1 --checkpoint-action=exec='npm run ci' README.md",
      'rm -rf ~/.cache/agent-guard/./leases',
      'rm -rf ~/.cache/agent-guard/foo/../leases',
      'rm -rf ~/.cache/agen[t]-guard/leases',
      "python -qc 'import os; os.system(\"npm run ci\")'",
      'printf x | python -W ignore',
      'printf x | python -X dev',
      'perl -MFile::Spec script.pl',
      'ruby -rbenchmark script.rb',
      'NODE_OPTIONS=--require=/tmp/preload.cjs npm run lint',
      'BASH_ENV=/tmp/preload.sh bash -c true',
      'PATH=/tmp:$PATH npm run lint',
      '. /tmp/lane',
      '"python3" -c \'import os; os.system("npm run ci")\'',
      'ionice npm run ci',
      'parallel npm run ci -- x',
      'rm -rf ~/.cache/agent-guard/lease?',
      'rm -rf ~/.cache/agent-guard/[l]eases',
      'rm -rf ~/.cache/agent-guard/lea{ses,se}',
      // A script generated, marked executable, and dispatched in one shell
      // line never existed when the hook inspected the filesystem (#189).
      "printf 'npx vitest\\n' > lane && chmod +x lane && ./lane",
      "printf 'npx vitest\\n' > lane; chmod +x lane; ./lane",
      "printf 'npx vitest\\n' > 'lane' && chmod +x 'lane' && './lane'",
      "printf 'npx vitest\\n' > /tmp/lane-189 && chmod +x /tmp/lane-189 && /tmp/lane-189",
      'touch ./lane && ./lane',
      // Quoting the deletion target does not make it prose (#198).
      'rm -rf "$XDG_CACHE_HOME/agent-guard"',
      'rm -rf "$HOME/.cache/agent-guard/leases"',
      'rm -rf ~/".cache/agent-guard"',
      'rm -rf "$HOME"/.cache/agent-guard/leases',
      ': > "$HOME/.cache/agent-guard/leases/live.json"',
    ]) {
      assert.equal(evaluateCommand(command, { env }).allow, false, `expected the guard to deny: ${command}`);
    }
  });

  test('unenumerated wrappers cannot hide a heavy lane or test binary', () => {
    // The prefix stripper knows an enumerated wrapper set; anything outside
    // it (flock, sudo, doas, chrt, strace, …) must not become a bypass. The
    // deny-side scans consider every runner-shaped token as a candidate
    // command start, so the wrapper's argument tail is still inspected.
    for (const command of [
      'flock /tmp/agent.lock npm run ci',
      'sudo npx vitest',
      'doas npm run test:e2e',
      'chrt -b 0 node --run ci',
      'flock /tmp/agent.lock pnpm run test:stories:ci',
      'strace -f npx playwright test',
      'sudo -u me npx c8 npm test',
      'flock /tmp/agent.lock npm run test:e2e:inner',
      'flock /tmp/agent.lock npm run $lane',
      // The -c string form runs its payload like `script -c`; a quoted
      // payload is a command, not prose, and is promoted for the same scans.
      "flock /tmp/agent.lock -c 'npm run ci'",
      "flock -n /tmp/agent.lock --command 'npx vitest'",
    ]) {
      assert.equal(evaluateCommand(command, { env }).allow, false, `expected the guard to deny: ${command}`);
    }
    // The canonical wrapper still carries its own sanctioned tail, and a
    // runner-shaped word in argument position is data, not an invocation.
    for (const command of [
      'node tools/agent-guard/run-guarded.mjs --label test:e2e -- npm run test:e2e:inner',
      'brew info npm',
      'git log --oneline -- vitest.config.ts',
      "flock /tmp/agent.lock -c 'npm run lint'",
    ]) {
      assert.equal(evaluateCommand(command, { env }).allow, true, `expected the guard to allow: ${command}`);
    }
  });

  test('the guard denies tampering with its own controls', () => {
    for (const command of ['AGENT_GUARD_FORCE=1 npm run test:dom', 'AGENT_GUARD_ASSUME_HUMAN=1 npm run test:dom', 'node tools/agent-guard/arbiter.mjs grant e2e']) {
      assert.equal(evaluateCommand(command, { env }).allow, false, `expected the guard to deny: ${command}`);
    }
  });

  test('legacy grant minting fails closed even outside an identified agent session', () => {
    const arbiter = path.join(root, 'tools', 'agent-guard', 'arbiter.mjs');
    const run = spawnSync(process.execPath, [arbiter, 'grant', 'e2e', '--minutes', '5'], {
      env: { PATH: process.env.PATH ?? '' },
      encoding: 'utf8',
    });
    assert.equal(run.status, 1);
    assert.match(run.stderr, /legacy grant minting is disabled/u);
  });

  test('a valid nested lease skips duplicate admission only after lane policy (#235)', () => {
    const child = spawn(process.execPath, ['-e', 'setInterval(() => {}, 10_000)'], { detached: true, stdio: 'ignore' });
    child.unref();
    const lease = acquireLease({ env, label: 'outer ordinary lane', estimatedMb: 128 });
    try {
      assert.equal(retargetLease(lease, { pid: child.pid, processGroupId: child.pid }), true);
      const nested = { ...env, AGENT_GUARDED: lease.id };

      const heavy = resolveInvocation({
        options: { label: 'innocuous-wrapper' },
        command: ['npm', 'run', 'test:e2e:inner'],
        env: nested,
        processGroupId: child.pid,
      });
      assert.equal(heavy.action, 'refuse', 'a parent lease is not heavy-lane authorization');
      assert.match(heavy.policy.message, /agents do not run it on this machine/u);

      const ordinary = resolveInvocation({
        options: { label: 'test:dom' },
        command: ['npm', 'run', 'test:dom:inner'],
        env: nested,
        processGroupId: child.pid,
      });
      assert.equal(ordinary.action, 'passthrough', 'an allowed nested lane must not duplicate admission');
    } finally {
      try {
        process.kill(-child.pid, 'SIGKILL');
      } catch {
        // The child may have exited independently.
      }
      releaseLease(lease);
    }

    const staleMarker = resolveInvocation({
      options: { label: 'test:dom' },
      command: ['npm', 'run', 'test:dom:inner'],
      env: { ...env, AGENT_GUARDED: lease.id },
      processGroupId: child.pid,
    });
    assert.equal(staleMarker.action, 'admit', 'a stale marker must not skip admission');
  });

  test('ordinary work is untouched', () => {
    for (const command of ['npm run lint', 'npm run typecheck', 'git status --short', 'node tools/agent-guard/arbiter.mjs status']) {
      assert.equal(evaluateCommand(command, { env }).allow, true, `expected the guard to allow: ${command}`);
    }
    assert.equal(evaluateCommand("cat > /tmp/doc <<'END-OF-FILE'\nnpm run \"$lane\"\nEND-OF-FILE", { env }).allow, true);
    assert.equal(evaluateCommand('cat > /tmp/doc <<.\nnpm run "$lane"\n.', { env }).allow, true);
    assert.equal(evaluateCommand('cat <<FIRST <<SECOND\nnpm run ci\nFIRST\nnpx vitest\nSECOND', { env }).allow, true);
    assert.equal(evaluateCommand('cat agent-health-guard/leases/live.json', { env }).allow, true);
    // Protected-variable text inside quotes is a mention, not an assignment
    // (#192): commit messages, search patterns, and file payloads are data.
    for (const command of [
      'rg "NODE_OPTIONS=" docs',
      'git commit -m "Document PATH=/usr/bin"',
      "printf 'PATH=/tmp\\n' > note.txt",
      "echo 'NODE_OPTIONS=--require ./x'",
      'git log --grep "GIT_SSH_COMMAND=" --oneline',
    ]) {
      assert.equal(evaluateCommand(command, { env }).allow, true, `expected the guard to allow: ${command}`);
    }
    // Relative paths as command *arguments* are not dispatches (#189):
    // creating or naming a file is fine as long as nothing executes it.
    for (const command of [
      "printf 'notes\\n' > lane && git add lane",
      'mkdir -p dist && cp cli.mjs dist/cli.mjs',
      './configure-does-not-exist',
    ]) {
      assert.equal(evaluateCommand(command, { env }).allow, true, `expected the guard to allow: ${command}`);
    }
  });

  test('directly executed text scripts cannot hide protected commands', () => {
    const lane = path.join(scratch, 'lane');
    writeFileSync(lane, '#!/bin/sh\nnpx vitest\n');
    assert.equal(evaluateCommand(lane, { env, cwd: scratch }).allow, false);
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

  test('the dormant admission seam requires an explicit proven peak, not a caller-declared ceiling (#223)', () => {
    const budget = deriveBudget(16384);
    const memory = { totalMb: 16384, availableMb: 8000, swapTotalMb: 2048, swapUsedMb: 1200, pressureLevel: 2 };
    assert.equal(decideAdmission({ budget, memory, requestMb: 256 }).reason, 'memory-pressure');
    assert.equal(decideAdmission({ budget, memory, requestMb: 1280, lanePeakMb: 996 }).granted, true);
  });

  test('Linux admission uses the container limit rather than host memory', () => {
    const files = new Map([
      ['/proc/meminfo', 'MemAvailable:  5784576 kB\nSwapTotal: 0 kB\nSwapFree: 0 kB\n'],
      ['/proc/self/cgroup', '0::/\n'],
      ['/sys/fs/cgroup/memory.max', String(4096 * 1024 * 1024)],
      ['/sys/fs/cgroup/memory.current', String(871 * 1024 * 1024)],
    ]);
    const status = readMemoryStatus({
      platform: 'linux',
      totalMb: 6073,
      readFile: (file) => {
        if (!files.has(file)) throw new Error(`missing fixture: ${file}`);
        return files.get(file);
      },
    });
    assert.equal(status.totalMb, 4096);
    assert.equal(status.availableMb, 3225);
  });

  test('CI markers are informational and never grant a wrapper exemption', () => {
    assert.equal(isCi({ GITHUB_ACTIONS: 'true' }), true);
    assert.equal(isCi({ CI: 'true' }), true);
    assert.equal(isCi({}), false);
    const hosted = {
      CI: 'true',
      GITHUB_ACTIONS: 'true',
      RUNNER_ENVIRONMENT: 'github-hosted',
      GITHUB_WORKSPACE: '/home/runner/work/repo/repo',
      RUNNER_TEMP: '/home/runner/work/_temp',
    };
    assert.equal(isCi(hosted), true);
  });

  test('an inherited CI marker does not exempt an agent process', () => {
    const runner = path.join(root, 'tools/agent-guard/run-guarded.mjs');
    // `AGENT_GUARDED` is reset here, with the CI markers, rather than per
    // spawn: when this suite itself runs nested inside a guarded lane, the
    // inherited value names a live lease and `run-guarded.mjs` passes the
    // inner run straight through, so every case below would exit 0 and assert
    // the opposite of what it means to. Three of the four spawns used to reset
    // it individually and `forgedHosted` did not, which made the whole suite
    // fail under any local `npm test` while CI — which invokes the inner lane
    // unwrapped, with `AGENT_GUARDED` unset — stayed green.
    const localProcessEnv = {
      ...process.env,
      AGENT_GUARDED: '',
      GITHUB_ACTIONS: '',
      RUNNER_ENVIRONMENT: '',
      GITHUB_WORKSPACE: '',
      RUNNER_TEMP: '',
    };
    const result = spawnSync(process.execPath, [runner, '--label', 'test:e2e', '--', process.execPath, '-e', 'process.exit(0)'], {
      cwd: root,
      encoding: 'utf8',
      env: { ...localProcessEnv, AI_AGENT: 'codex', CI: '1' },
    });
    assert.notEqual(result.status, 0);
    assert.match(result.stderr, /agents do not run it on this machine/u);
    const forgedHuman = spawnSync(process.execPath, [runner, '--label', 'test:e2e', '--', process.execPath, '-e', 'process.exit(0)'], {
      cwd: root,
      encoding: 'utf8',
      env: { ...localProcessEnv, AGENT_GUARD_ASSUME_HUMAN: '1', AI_AGENT: 'codex' },
    });
    assert.notEqual(forgedHuman.status, 0);
    const strippedIdentity = spawnSync(process.execPath, [runner, '--label', 'test:e2e', '--', process.execPath, '-e', 'process.exit(0)'], {
      cwd: root,
      encoding: 'utf8',
      env: { HOME: process.env.HOME, PATH: process.env.PATH },
    });
    assert.notEqual(strippedIdentity.status, 0);
    const forgedHosted = spawnSync(process.execPath, [runner, '--label', 'test:e2e', '--', process.execPath, '-e', 'process.exit(0)'], {
      cwd: root,
      encoding: 'utf8',
      env: {
        ...localProcessEnv,
        CI: 'true',
        GITHUB_ACTIONS: 'true',
        RUNNER_ENVIRONMENT: 'github-hosted',
        GITHUB_WORKSPACE: '/home/runner/work/repo/repo',
        RUNNER_TEMP: '/home/runner/work/_temp',
        ACTIONS_ID_TOKEN_REQUEST_URL: 'https://attacker.invalid/token',
        ACTIONS_ID_TOKEN_REQUEST_TOKEN: 'forged',
      },
    });
    assert.notEqual(forgedHosted.status, 0);
    assert.match(forgedHosted.stderr, /agents do not run it on this machine/u);
  });
});
