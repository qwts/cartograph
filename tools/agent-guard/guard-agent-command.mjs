#!/usr/bin/env node

/* eslint-disable max-lines -- This governed command parser is centralized so every harness shares one fail-closed policy. */

// Pre-execution command hook for every agent harness in the fleet.
//
// The wrapper (run-guarded.mjs) is the primary control; this hook exists to
// close the ways around it. It covers Claude Code, Cursor AND Codex, because a
// guard only one harness honours does not solve a problem that Codex sessions
// caused half of.
//
// It denies four things:
//   1. Heavy local suites, for agents — the lanes that
//      actually bricked the machine (`npm run ci`, e2e, storybook, perf, cov).
//   2. Direct test-binary invocations that skip the wrapper entirely.
//   3. Tampering with the guard's own controls: the human escape hatch, the
//      assume-human override, and redirecting the state directory (which would
//      hand the session a private lease namespace and undo machine scoping).
//   4. Legacy grant commands, which cannot authenticate a human when the
//      agent shares the same OS user.
//
// Scoping: only commands that execute inside a guarded checkout are policed;
// cross-repo work from the same session is left alone. Blocked text inside
// quotes or heredocs is a mention (a commit message, a grep pattern), not an
// invocation — except nested shell payloads (`bash -c "…"`), which are
// executable and are unwrapped and scanned.
//
// Fail-open by design: a malformed payload allows the command rather than
// bricking every shell call.
//
// Protocols: --protocol=claude | cursor | codex

import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync, realpathSync } from 'node:fs';
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

import { HEAVY_LANES } from './lib/policy.mjs';

const GUARD_GUIDE = 'https://github.com/qwts/playbook-engineering/blob/main/docs/reference/agent-memory-guard.md';

// Two different blocks need two different next steps, and a refusal whose
// advice does not fit is one an agent argues with instead of following.
const GUIDANCE =
  'Push the branch and let GitHub CI verify — its workflow invokes the underlying CI entrypoint directly. ' +
  `See ${GUARD_GUIDE}.`;

// A direct binary is not necessarily a heavy run — in a tooling repo `node
// --test` is the light, normal path. What is wrong with it is that it skips
// the wrapper, so the fix is the repo's own guarded entrypoint, not CI.
const USE_ENTRYPOINT =
  "Use a repository-documented guarded npm test entrypoint instead (normally `npm test`); it must wrap " +
  'tools/agent-guard/run-guarded.mjs, which derives a ceiling from this machine and checks the machine-wide ' +
  `memory budget first. See ${GUARD_GUIDE}.`;

// Markers that identify a checkout governed by this policy. The second is the
// pre-rollout location, so a repo mid-migration is still policed.
const GUARD_MARKERS = ['tools/agent-guard/run-guarded.mjs', 'scripts/run-guarded.mjs'];

const BLOCKED = [
  {
    // Electron-hosted node:test (image-trail's original incident path).
    pattern: /\belectron\b[^\n;&|]*\s--test(?![\w-])/u,
    what: 'direct `electron --test` invocation',
  },
  {
    pattern: /\bnode\b[^\n;&|]*\s--test(?![\w-])/u,
    what: 'direct `node --test` invocation',
  },
  {
    pattern: /\bnode\b[^\n;&|]*\.test-dist(-dom)?\b/u,
    what: 'direct execution of compiled tests in .test-dist(-dom)',
  },
  {
    pattern: /\bplaywright\s+test\b/u,
    what: 'direct Playwright invocation',
  },
  {
    pattern: /\btest-storybook\b/u,
    what: 'direct Storybook test-runner invocation',
  },
  {
    pattern: /^(?:(?:time|command)\s+|env\s+(?:\w+=\S*\s+)*)*(?:\w+=\S*\s+)*(?:\S*\/)?(?:npx(?:\s+-\S+)*\s+)?(?:\S*\/)?vitest(?:\s|$)/u,
    what: 'direct Vitest invocation',
  },
  {
    pattern: /^(?:(?:time|command)\s+|env\s+(?:\w+=\S*\s+)*)*(?:\w+=\S*\s+)*(?:\S*\/)?(?:npx(?:\s+-\S+)*\s+)?(?:\S*\/)?c8(?:\s|$)/u,
    what: 'direct c8 coverage invocation',
  },
  {
    pattern: /^(?:(?:time|command)\s+|env\s+(?:\w+=\S*\s+)*)*(?:\w+=\S*\s+)*(?:\S*\/)?npm\s+(?:exec|x)\s+(?:-\S+\s+)*(?:\S*\/)?(?:vitest|c8|playwright|test-storybook)(?:\s|$)/u,
    what: 'direct test-binary invocation through npm exec',
  },
  {
    // Headed/interactive runs open GUI windows on the shared desktop.
    pattern: /\bnpm\s+run\s+test:e2e:(ui|headed)(?![\w:-])/u,
    what: 'headed/interactive e2e run',
    reason:
      "Blocked headed/interactive e2e run: GUI windows on the shared desktop steal the owner's focus, " +
      'and each one boots a full Electron app. These scripts are human-only.',
  },
];

// Controls an agent must not touch. Checked before the run-guarded allowlist so
// `AGENT_GUARD_FORCE=1 node tools/agent-guard/run-guarded.mjs …` cannot slip
// through as a sanctioned run.
const TAMPERING = [
  {
    pattern: /(?:^|[\s;&|])(?:CI|GITHUB_ACTIONS|CONTINUOUS_INTEGRATION|BUILDKITE|GITLAB_CI|JENKINS_URL)=/u,
    reason:
      'Blocked a command-local CI marker: CI markers and bearer credentials never exempt this wrapper, and a ' +
      `local command cannot change that policy. Remove the assignment and use the guarded entrypoint. ${GUIDANCE}`,
  },
  {
    pattern: /\bAGENT_GUARD_FORCE=/u,
    reason:
      'Blocked AGENT_GUARD_FORCE: overriding admission control is a human-only escape hatch. A refused run means the ' +
      `machine does not have the memory right now — report the refusal instead of forcing past it. ${GUIDANCE}`,
  },
  {
    pattern: /\bAGENT_GUARD_ASSUME_HUMAN=/u,
    reason:
      'Blocked AGENT_GUARD_ASSUME_HUMAN: this override exists so a human in an editor terminal is not mistaken for an ' +
      `agent. An agent setting it is claiming to be the owner. ${GUIDANCE}`,
  },
  {
    pattern: /\bAGENT_GUARD_STATE_DIR=/u,
    reason:
      'Blocked AGENT_GUARD_STATE_DIR: redirecting the lease directory gives this session a private budget that no other ' +
      'repo or agent can see — which is exactly the per-worktree bug this guard replaced. It is for tests only.',
  },
  {
    pattern: /\b(?:NODE_OPTIONS|BASH_ENV|ENV|ZDOTDIR|PERL5OPT|RUBYOPT|PYTHONPATH|PYTHONHOME|PHPRC|PHP_INI_SCAN_DIR|LD_PRELOAD|DYLD_INSERT_LIBRARIES|GIT_SSH_COMMAND|GIT_CONFIG_COUNT|PATH)=/u,
    reason:
      'Blocked an executable-loading environment override: preload, startup, loader, config, and command-resolution variables execute or select code before the requested command and ' +
      `can dispatch a protected lane outside static classification. Remove the override. ${USE_ENTRYPOINT}`,
  },
  {
    pattern: /\barbiter\.mjs\s+grant\b/u,
    reason:
      'Blocked `arbiter.mjs grant`: same-user local grants cannot authenticate human approval and are disabled. ' +
      'The owner can run the lane directly from their own terminal, or the agent can use GitHub CI.',
  },
  {
    // The wrapper sets this for its own children so nested guarded scripts do
    // not deadlock. Supplied from outside it is a claim to already be inside a
    // guarded run — which would skip the lease, the ceiling and the headroom
    // check entirely. The wrapper independently refuses to honour a value that
    // does not name a live lease; this is the outer half of that pair.
    pattern: /\bAGENT_GUARDED=/u,
    reason:
      'Blocked AGENT_GUARDED: that marker is set by the guard for its own children, and supplying it by hand claims to ' +
      `be inside a guarded run that does not exist — skipping admission entirely. ${GUIDANCE}`,
  },
  {
    pattern:
      /(?:\benv\b[^\n;&|]*(?:\s(?:-(?=\s|$)|-i|--ignore-environment)(?=\s|$)|(?:-u(?:=|\s*)|--unset(?:=|\s+))(?:CLAUDECODE|CLAUDE_CODE_ENTRYPOINT|AI_AGENT|CODEX_\w+|CURSOR_\w+|\w*_AGENT))|\b(?:unset\s+(?:(?:--|-v|-f)\s+)*|export\s+-n\s+)(?:CLAUDECODE|CLAUDE_CODE_ENTRYPOINT|AI_AGENT|CODEX_\w+|CURSOR_\w+|\w*_AGENT))/u,
    reason:
      'Blocked removal of agent identity before run-guarded.mjs: the wrapper must inherit its harness markers so it ' +
      `cannot misclassify an agent as the human owner. ${GUIDANCE}`,
  },
  {
    pattern:
      /(?:^|[\s;&|])(?:CLAUDECODE|CLAUDE_CODE_ENTRYPOINT|AI_AGENT|CODEX_\w+|CURSOR_\w+|\w*_AGENT)=[^\s;&|]*/u,
    reason:
      'Blocked reassignment of agent identity before run-guarded.mjs: clearing or replacing a harness marker can make ' +
      `the wrapper misclassify an agent as the human owner. ${GUIDANCE}`,
  },
];

// Shell segments, so a sanctioned command in one segment cannot vouch for a
// blocked one in the next. Quotes are already blanked by stripInertText, so
// these separators are structural rather than incidental text.
export function splitSegments(command) {
  const REDIRECTION_AMPERSAND = '\0';
  const ESCAPED_SEMICOLON = '\u0001';
  const ESCAPED_AMPERSAND = '\u0002';
  const ESCAPED_PIPE = '\u0003';
  return command
    .replace(/\\;/gu, ESCAPED_SEMICOLON)
    .replace(/\\&/gu, ESCAPED_AMPERSAND)
    .replace(/\\\|/gu, ESCAPED_PIPE)
    .replace(/(\d*>)&(?=\d|-)/gu, `$1${REDIRECTION_AMPERSAND}`)
    .replace(/&(?=>>?)/gu, REDIRECTION_AMPERSAND)
    .split(/\|\||&&|[;\n|&]/u)
    .map((segment) => {
      let normalized = segment
        .replaceAll(REDIRECTION_AMPERSAND, '&')
        .replaceAll(ESCAPED_SEMICOLON, '\\;')
        .replaceAll(ESCAPED_AMPERSAND, '\\&')
        .replaceAll(ESCAPED_PIPE, '\\|')
        .trim();
      // Parentheses that wrap a subshell are control operators, not part of
      // its executable or final argument. Remove balanced outer wrappers so
      // `(npm run ci)` classifies exactly like `npm run ci`.
      while (/^[({]\s*/u.test(normalized)) normalized = normalized.replace(/^[({]\s*/u, '');
      normalized = normalized.replace(/[)}]+(?=\s*(?:$|\d*[<>]))/gu, '').trim();
      return normalized;
    })
    .filter(Boolean);
}

// A segment that IS a wrapper invocation: optional env assignments, then node
// (however it is pathed), then run-guarded.mjs as its script argument. Merely
// mentioning the filename elsewhere in the segment does not qualify.
const ANY_WRAPPER_SEGMENT = /^(?:\w+=\S*\s+)*(?:\S*\/)?node\s+(?:-\S+\s+)*\S*run-guarded\.mjs(?:\s|$)/u;
const WRAPPER_SEGMENT = /^(?:\w+=\S*\s+)*(?:\S*\/)?node\s+(?:-\S+\s+)*(?:\.\/)?(?:tools\/agent-guard|scripts)\/run-guarded\.mjs(?:\s|$)/u;

function tryRealpath(target) {
  try {
    return realpathSync(target);
  } catch {
    return resolve(target);
  }
}

function isWithin(child, parent) {
  const c = tryRealpath(child);
  const p = tryRealpath(parent);
  return c === p || c.startsWith(p + sep);
}

function parseHeredocWord(line, start) {
  let index = start;
  let value = '';
  let quote = null;
  while (index < line.length) {
    const character = line[index];
    if (quote === "'") {
      if (character === "'") quote = null;
      else value += character;
      index += 1;
      continue;
    }
    if (quote === '"') {
      if (character === '"') {
        quote = null;
        index += 1;
      } else if (character === '\\' && /[\\"$`]/u.test(line[index + 1] ?? '')) {
        value += line[index + 1];
        index += 2;
      } else {
        value += character;
        index += 1;
      }
      continue;
    }
    if (/\s|[;&|()<>]/u.test(character)) break;
    if (character === '$' && line[index + 1] === "'") {
      let end = index + 2;
      while (end < line.length) {
        if (line[end] === '\\') end += 2;
        else if (line[end] === "'") break;
        else end += 1;
      }
      if (end < line.length) {
        value += decodeAnsiCWord(line.slice(index, end + 1));
        index = end + 1;
        continue;
      }
    }
    if (character === "'" || character === '"') {
      quote = character;
      index += 1;
      continue;
    }
    if (character === '\\' && index + 1 < line.length) {
      value += line[index + 1];
      index += 2;
      continue;
    }
    value += character;
    index += 1;
  }
  return { end: index, value };
}

// Canonicalize shell-valid heredoc words to identifier sentinels before the
// scanners run. This handles mixed/backslash quoting and `<<-` tab stripping
// without trying to express shell quote removal through a regex backreference.
function normalizeHeredocSyntax(text) {
  const replacements = [];
  let cursor = 0;
  let sequence = 0;
  while (cursor < text.length) {
    const declarationEnd = text.indexOf('\n', cursor);
    const lineEnd = declarationEnd < 0 ? text.length : declarationEnd;
    const line = text.slice(cursor, lineEnd);
    const declarations = [];
    const operator = /<<(-?)(?!<)\s*/gu;
    for (const match of line.matchAll(operator)) {
      const wordStart = match.index + match[0].length;
      const word = parseHeredocWord(line, wordStart);
      if (word.end === wordStart || word.value.length === 0) continue;
      declarations.push({
        delimiter: word.value,
        stripTabs: match[1] === '-',
        token: `AGENT_GUARD_HEREDOC_${sequence++}`,
        wordEnd: cursor + word.end,
        wordStart: cursor + wordStart,
      });
    }
    if (declarations.length === 0 || declarationEnd < 0) {
      cursor = lineEnd + (declarationEnd < 0 ? 0 : 1);
      continue;
    }
    let bodyCursor = declarationEnd + 1;
    for (const declaration of declarations) {
      replacements.push({ start: declaration.wordStart, end: declaration.wordEnd, value: declaration.token });
      let found = false;
      while (bodyCursor <= text.length) {
        const terminatorEnd = text.indexOf('\n', bodyCursor);
        const candidateEnd = terminatorEnd < 0 ? text.length : terminatorEnd;
        const candidate = text.slice(bodyCursor, candidateEnd).replace(/\r$/u, '');
        const normalized = declaration.stripTabs ? candidate.replace(/^\t+/u, '') : candidate;
        if (normalized === declaration.delimiter) {
          replacements.push({ start: bodyCursor, end: candidateEnd, value: declaration.token });
          bodyCursor = candidateEnd + (terminatorEnd < 0 ? 0 : 1);
          found = true;
          break;
        }
        if (terminatorEnd < 0) {
          bodyCursor = text.length;
          break;
        }
        bodyCursor = terminatorEnd + 1;
      }
      if (!found) break;
    }
    cursor = Math.max(bodyCursor, declarationEnd + 1);
  }
  let normalized = text;
  for (const replacement of replacements.sort((left, right) => right.start - left.start)) {
    normalized = normalized.slice(0, replacement.start) + replacement.value + normalized.slice(replacement.end);
  }
  return normalized;
}

function normalizedHeredocs(text) {
  const normalized = normalizeHeredocSyntax(text);
  const heredocs = [];
  let cursor = 0;
  while (cursor < normalized.length) {
    const declarationEnd = normalized.indexOf('\n', cursor);
    if (declarationEnd < 0) break;
    const declarationLine = normalized.slice(cursor, declarationEnd);
    const declarations = [...declarationLine.matchAll(/<<-?\s*(AGENT_GUARD_HEREDOC_\d+)/gu)].map((match) => ({
      operatorStart: cursor + match.index,
      operatorEnd: cursor + match.index + match[0].length,
      token: match[1],
    }));
    if (declarations.length === 0) {
      cursor = declarationEnd + 1;
      continue;
    }
    let bodyStart = declarationEnd + 1;
    for (const declaration of declarations) {
      let terminatorStart = bodyStart;
      let found = false;
      while (terminatorStart <= normalized.length) {
        const terminatorLineEnd = normalized.indexOf('\n', terminatorStart);
        const candidateEnd = terminatorLineEnd < 0 ? normalized.length : terminatorLineEnd;
        if (normalized.slice(terminatorStart, candidateEnd).replace(/^\t+/u, '').replace(/\r$/u, '') === declaration.token) {
          const terminatorEnd = candidateEnd + (terminatorLineEnd < 0 ? 0 : 1);
          heredocs.push({ ...declaration, bodyStart, bodyEnd: terminatorStart, terminatorEnd });
          bodyStart = terminatorEnd;
          found = true;
          break;
        }
        if (terminatorLineEnd < 0) break;
        terminatorStart = terminatorLineEnd + 1;
      }
      if (!found) break;
    }
    cursor = Math.max(bodyStart, declarationEnd + 1);
  }
  return { normalized, heredocs };
}

function transformHeredocs(text, mode) {
  const { normalized, heredocs } = normalizedHeredocs(text);
  const edits = [];
  const blank = (value) => value.replace(/[^\n]/gu, ' ');
  for (const heredoc of heredocs) {
    const body = normalized.slice(heredoc.bodyStart, heredoc.bodyEnd);
    const isShell = shellHeredocBody(normalized, heredoc.operatorStart, body) !== ' ';
    if (mode === 'non-shell' && isShell) continue;
    edits.push({ start: heredoc.operatorStart, end: heredoc.operatorEnd, value: blank(normalized.slice(heredoc.operatorStart, heredoc.operatorEnd)) });
    edits.push({
      start: heredoc.bodyStart,
      end: heredoc.terminatorEnd,
      value: mode === 'strip' && isShell ? `\n${body}\n` : blank(normalized.slice(heredoc.bodyStart, heredoc.terminatorEnd)),
    });
  }
  let transformed = normalized;
  for (const edit of edits.sort((left, right) => right.start - left.start)) {
    transformed = transformed.slice(0, edit.start) + edit.value + transformed.slice(edit.end);
  }
  return transformed;
}

function maskHeredocBodies(text) {
  return transformHeredocs(text, 'all');
}

function scopeWords(text) {
  return [...text.matchAll(/\$'(?:[^'\\]|\\.)*'|'([^']*)'|"((?:[^"\\]|\\.)*)"|([^\s]+)/gu)].map((match) => ({
    index: match.index,
    value: match[1] ?? match[2]?.replace(/\\(["\\$`])/gu, '$1') ?? match[3] ?? decodeAnsiCWord(match[0]),
  }));
}

function prefixReaches(words, index) {
  const marker = '__agent_guard_scope_command__';
  return commandAfterPrefixes([...words.slice(0, index).map((word) => word.value), marker].join(' ')) === marker;
}

function directoryOptionTargets(segment) {
  const words = scopeWords(segment);
  const targets = [];
  for (let index = 0; index < words.length; index += 1) {
    const executable = words[index].value.split('/').at(-1);
    const corepackProxy = index > 0 && words[index - 1].value.split('/').at(-1) === 'corepack' && prefixReaches(words, index - 1);
    if (executable === 'env' && prefixReaches(words, index)) {
      let envTarget;
      for (let optionAt = index + 1; optionAt < words.length; optionAt += 1) {
        const option = words[optionAt].value;
        if (option === '-C' || option === '--chdir') {
          if (words[optionAt + 1]) envTarget = words[++optionAt].value;
        } else if (option.startsWith('--chdir=')) {
          envTarget = option.slice('--chdir='.length);
        } else if (/^-C.+/u.test(option)) {
          envTarget = option.slice(2);
        } else if (/^(?:-u|--unset|-P|--path|-S|--split-string)$/u.test(option)) {
          optionAt += 1;
        } else if (option === '--') {
          break;
        } else if (!option.startsWith('-') && !/^\w+=/u.test(option)) {
          break;
        }
      }
      if (envTarget) targets.push({ index: words[index].index, target: envTarget });
    }
    if (!['npm', 'pnpm', 'yarn', 'bun'].includes(executable) || (!prefixReaches(words, index) && !corepackProxy)) continue;
    const options =
      executable === 'npm'
        ? { equals: ['--prefix='], operands: new Set(['--prefix', '-C']) }
        : executable === 'pnpm'
          ? { equals: ['--dir='], operands: new Set(['--dir', '-C']) }
          : { equals: ['--cwd='], operands: new Set(['--cwd']) };
    let packageTarget;
    for (let optionAt = index + 1; optionAt < words.length; optionAt += 1) {
      const option = words[optionAt].value;
      if (option === '--') break;
      if (options.operands.has(option)) {
        if (words[optionAt + 1]) packageTarget = words[++optionAt].value;
        continue;
      }
      const equals = options.equals.find((prefix) => option.startsWith(prefix));
      if (equals) packageTarget = option.slice(equals.length);
      if ((executable === 'npm' || executable === 'pnpm') && /^-C.+/u.test(option)) {
        packageTarget = option.slice(2);
      }
    }
    if (packageTarget) targets.push({ index: words[index].index, target: packageTarget });
  }
  return targets;
}

// Every directory a command may execute in. Retaining the reported cwd is
// deliberate: a `cd` inside `( ... )` does not change its parent shell, while
// a later top-level transition does. For hook scoping, the safe answer is the
// union of observed scopes rather than guessing one final directory.
export function resolveExecutionDirs(cwd, command) {
  if (typeof cwd !== 'string' || cwd.length === 0) return [];
  if (typeof command !== 'string') return [cwd];
  const scopedCommand = maskHeredocBodies(command);
  const directories = [cwd];
  let current = cwd;
  const parentScopes = [];
  const events = [];

  // A subshell inherits its parent's cwd but cannot change it. Record both the
  // child scopes and the restored parent scope so a later relative `cd` is
  // resolved from the directory the shell will actually use.
  const shellSyntax = new Uint8Array(scopedCommand.length);
  const contexts = [{ mode: 'shell', closesSubshell: false }];
  for (let index = 0; index < scopedCommand.length; index += 1) {
    const character = scopedCommand[index];
    const context = contexts.at(-1);
    if (context.mode === 'single-quote') {
      if (character === "'") contexts.pop();
      continue;
    }
    if (context.mode === 'double-quote') {
      if (character === '\\') {
        index += 1;
        continue;
      }
      if (character === '"') {
        contexts.pop();
        continue;
      }
      if (character === '$' && scopedCommand[index + 1] === '(') {
        events.push({ index: index + 1, type: 'subshell-open' });
        contexts.push({ mode: 'shell', closesSubshell: true });
        index += 1;
      }
      continue;
    }
    shellSyntax[index] = 1;
    if (character === '\\') {
      index += 1;
      continue;
    }
    if (character === "'") {
      contexts.push({ mode: 'single-quote' });
      continue;
    }
    if (character === '"') {
      contexts.push({ mode: 'double-quote' });
      continue;
    }
    if (character === '(') {
      events.push({ index, type: 'subshell-open' });
      contexts.push({ mode: 'shell', closesSubshell: true });
      continue;
    }
    if (character === ')' && context.closesSubshell) {
      events.push({ index, type: 'subshell-close' });
      contexts.pop();
    }
  }

  const transitions = /(?:^|\|\||&&|[;\n|&(){}])\s*(?:(?:command|builtin)\s+)?(cd|pushd)\s+(?:(?:-[LPe@]+|--)\s+)*(?:"([^"]+)"|'([^']+)'|([^\s;&|(){}]+))/gu;
  for (const match of scopedCommand.matchAll(transitions)) {
    const index = match.index + match[0].indexOf(match[1]);
    if (shellSyntax[index]) events.push({ index, target: match[2] ?? match[3] ?? match[4], type: 'cd' });
  }

  // GNU/POSIX-compatible env implementations may change directory for the
  // child command with `-C` or `--chdir`. This scope does not persist in the
  // parent shell, so record it without updating `current`.
  const envCommands = /(?:^|\|\||&&|[;\n|&(){}])\s*(?:\S*\/)?env(?=\s|$)([^;\n|&(){}]*)/gu;
  for (const match of scopedCommand.matchAll(envCommands)) {
    const envIndex = match.index + match[0].indexOf('env');
    if (!shellSyntax[envIndex]) continue;
    const words = [...match[1].matchAll(/"([^"]*)"|'([^']*)'|([^\s]+)/gu)].map((word) => word[1] ?? word[2] ?? word[3]);
    let chdirTarget;
    for (let index = 0; index < words.length; index += 1) {
      const word = words[index];
      if (word === '-C' || word === '--chdir') {
        chdirTarget = words[index + 1];
        index += 1;
      } else if (word.startsWith('--chdir=')) {
        chdirTarget = word.slice('--chdir='.length);
      } else if (/^-C.+/u.test(word)) {
        chdirTarget = word.slice(2);
      } else if (/^(?:-u|--unset|-P|--path)$/u.test(word)) {
        index += 1;
      } else if (word === '-S' || word === '--split-string') {
        const splitWords = [...(words[index + 1] ?? '').matchAll(/"([^"]*)"|'([^']*)'|([^\s]+)/gu)].map((part) => part[1] ?? part[2] ?? part[3]);
        words.splice(index, 2, ...splitWords);
        index -= 1;
      } else if (/^(?:-S|--split-string)=/u.test(word)) {
        const splitWords = [...word.slice(word.indexOf('=') + 1).matchAll(/"([^"]*)"|'([^']*)'|([^\s]+)/gu)].map((part) => part[1] ?? part[2] ?? part[3]);
        words.splice(index, 1, ...splitWords);
        index -= 1;
      } else if (word === '--') {
        break;
      } else if (!word.startsWith('-') && !/^\w+=/u.test(word)) {
        break;
      }
    }
    if (chdirTarget) events.push({ index: envIndex, target: chdirTarget, type: 'env-chdir' });
  }

  // Normalize supported execution prefixes before looking for directory
  // options, while retaining quoted path operands and their source offsets.
  const segments = /(?:^|[;\n|&(){}])([^;\n|&(){}]+)/gu;
  for (const match of scopedCommand.matchAll(segments)) {
    const segmentOffset = match.index + match[0].indexOf(match[1]);
    const firstWord = scopeWords(match[1])[0];
    if (!firstWord || !shellSyntax[segmentOffset + firstWord.index]) continue;
    for (const option of directoryOptionTargets(match[1])) {
      events.push({ index: segmentOffset + option.index, target: option.target, type: 'env-chdir' });
    }
  }

  const resolveTarget = (target, base) => {
    if (target.startsWith('~')) {
      const home = process.env.HOME;
      if (!home) return null;
      target = home + target.slice(1);
    }
    return isAbsolute(target) ? target : resolve(base, target);
  };

  const order = { 'subshell-open': 0, cd: 1, 'env-chdir': 1, 'subshell-close': 2 };
  const uniqueEvents = [...new Map(events.map((event) => [`${event.index}:${event.type}:${event.target ?? ''}`, event])).values()];
  uniqueEvents.sort((left, right) => left.index - right.index || order[left.type] - order[right.type]);
  for (const event of uniqueEvents) {
    if (event.type === 'subshell-open') {
      parentScopes.push(current);
      continue;
    }
    if (event.type === 'subshell-close') {
      current = parentScopes.pop() ?? current;
      continue;
    }
    const target = resolveTarget(event.target, current);
    if (!target) continue;
    directories.push(target);
    if (event.type === 'cd') current = target;
  }
  const effective = stripInertText(command);
  const hasShellCommandString = /(?:^|[;\n|&(){}])[^;\n|&(){}]*\b(?:ba|da|z)?sh\b[^;\n|&(){}]*\s-[A-Za-z]*c[A-Za-z]*\s+(?:\$)?["']/u.test(command);
  if (hasShellCommandString && effective !== command) {
    for (const nested of resolveExecutionDirs(cwd, effective).slice(1)) {
      if (!directories.includes(nested)) directories.push(nested);
    }
  }
  return directories;
}

export function resolveExecutionDir(cwd, command) {
  return resolveExecutionDirs(cwd, command).at(-1) ?? null;
}

const QUOTED = /\$'(?:[^'\\]|\\.)*'|'[^']*'|"(?:[^"\\]|\\.)*"/u;

function endsWithShellC(scanned) {
  const rawSegment = scanned.split(/\|\||&&|[;\n|&]/u).at(-1).trim();
  const segment = commandAfterPrefixes(rawSegment);
  const tokens = segment.split(/\s+/u).filter(Boolean);
  let i = 0;
  if (!/(?:^|\/)(?:ba|da|z)?sh$/u.test(tokens[i] ?? '')) return false;
  i += 1;
  while (i < tokens.length) {
    const token = tokens[i];
    if (/^-[A-Za-z]*c[A-Za-z]*$/u.test(token)) return i === tokens.length - 1;
    if (/^(?:-[A-Za-z]*[oO]|--(?:option|shopt))$/u.test(token)) {
      if (i + 1 >= tokens.length) return false;
      i += 2;
      continue;
    }
    if (!token.startsWith('-')) return false;
    i += 1;
  }
  return false;
}

function shellHeredocBody(command, offset, body) {
  const prefix = command.slice(0, offset);
  const segment = prefix.split(/\|\||&&|[;\n|&]/u).at(-1).trim();
  const executable = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean)[0];
  const declarationEnd = command.indexOf('\n', offset);
  const declaration = command.slice(offset, declarationEnd < 0 ? command.length : declarationEnd);
  const pipelineSink = commandAfterPrefixes(declaration.split(/(?<![\\|])\|(?!\|)/u).at(-1).trim()).split(/\s+/u).filter(Boolean)[0];
  return /(?:^|\/)(?:ba|da|z)?sh$/u.test(executable ?? '') || /(?:^|\/)(?:ba|da|z)?sh$/u.test(pipelineSink ?? '')
    ? `\n${body}\n`
    : ' ';
}

function maskNonShellHeredocs(command) {
  return transformHeredocs(command, 'non-shell');
}

function commandSubstitutionBodies(text, { processSubstitutions = false } = {}) {
  const bodies = [];
  for (let i = 0; i < text.length - 1; i += 1) {
    if (text[i] === '\\') {
      i += 1;
      continue;
    }
    const commandSubstitution = text[i] === '$' && text[i + 1] === '(';
    const processSubstitution = processSubstitutions && (text[i] === '<' || text[i] === '>') && text[i + 1] === '(';
    if (commandSubstitution || processSubstitution) {
      let depth = 1;
      let j = i + 2;
      let quote = null;
      for (; j < text.length && depth > 0; j += 1) {
        if (text[j] === '\\') {
          j += 1;
        } else if (quote !== null) {
          if (text[j] === quote) quote = null;
        } else if (text[j] === "'" || text[j] === '"' || text[j] === '`') {
          quote = text[j];
        } else if (text[j] === '$' && text[j + 1] === '(') {
          depth += 1;
          j += 1;
        } else if (text[j] === '(') {
          depth += 1;
        } else if (text[j] === ')') {
          depth -= 1;
        }
      }
      if (depth === 0) {
        bodies.push(text.slice(i + 2, j - 1));
        i = j - 1;
      }
      continue;
    }
    if (text[i] === '`') {
      let j = i + 1;
      while (j < text.length) {
        if (text[j] === '\\') {
          j += 2;
          continue;
        }
        if (text[j] === '`') break;
        j += 1;
      }
      if (j < text.length) {
        bodies.push(text.slice(i + 1, j));
        i = j;
      }
    }
  }
  return bodies;
}

function decodeAnsiCWord(quoted) {
  const text = quoted.slice(2, -1);
  let decoded = '';
  const simple = new Map([
    ['a', '\x07'],
    ['b', '\b'],
    ['e', '\x1b'],
    ['E', '\x1b'],
    ['f', '\f'],
    ['n', '\n'],
    ['r', '\r'],
    ['t', '\t'],
    ['v', '\v'],
    ['\\', '\\'],
    ["'", "'"],
    ['"', '"'],
    ['?', '?'],
  ]);
  for (let i = 0; i < text.length; i += 1) {
    if (text[i] !== '\\' || i + 1 >= text.length) {
      decoded += text[i];
      continue;
    }
    const escaped = text[++i];
    if (simple.has(escaped)) {
      decoded += simple.get(escaped);
      continue;
    }
    if (/[0-7]/u.test(escaped)) {
      let digits = escaped;
      while (digits.length < 3 && /[0-7]/u.test(text[i + 1] ?? '')) digits += text[++i];
      decoded += String.fromCodePoint(Number.parseInt(digits, 8));
      continue;
    }
    const widths = { x: 2, u: 4, U: 8 };
    const width = widths[escaped];
    if (width !== undefined) {
      let digits = '';
      while (digits.length < width && /[0-9A-Fa-f]/u.test(text[i + 1] ?? '')) digits += text[++i];
      const point = Number.parseInt(digits, 16);
      decoded += digits.length > 0 && Number.isSafeInteger(point) && point <= 0x10ffff ? String.fromCodePoint(point) : escaped;
      continue;
    }
    if (escaped === 'c' && i + 1 < text.length) {
      decoded += String.fromCodePoint(text[++i].toUpperCase().codePointAt(0) & 0x1f);
      continue;
    }
    if (escaped !== '\n') decoded += `\\${escaped}`;
  }
  return decoded;
}

function quotedWord(quoted) {
  const inner = quoted.startsWith("$'")
    ? decodeAnsiCWord(quoted)
    : quoted.startsWith("'")
      ? quoted.slice(1, -1)
      : quoted.slice(1, -1).replace(/\\(["\\$`])/gu, '$1');
  return /\s|[;&|]/u.test(inner) ? null : inner;
}

function normalizeUnquotedEscapes(text) {
  const ESCAPED_SPACE = '\u0004';
  let normalized = '';
  for (let i = 0; i < text.length; i += 1) {
    if (text[i] !== '\\' || i + 1 >= text.length) {
      normalized += text[i];
      continue;
    }
    const next = text[i + 1];
    if (next === '\n') {
      i += 1;
    } else if (next === ' ' || next === '\t') {
      // An escaped blank stays inside one shell word. A command-string
      // consumer (-c/eval/--call) restores it before scanning the payload.
      normalized += ESCAPED_SPACE;
      i += 1;
    } else if (/[;&|$`]/u.test(next)) {
      // Keep escaped shell structure visibly escaped; splitSegments masks it.
      normalized += `\\${next}`;
      i += 1;
    } else {
      normalized += next;
      i += 1;
    }
  }
  return normalized;
}

function endsWithExecutableString(scanned) {
  if (endsWithShellC(scanned)) return true;
  if (dispatcherCommandPayloads(scanned).some((payload) => endsWithShellC(payload))) return true;
  if (endsWithWatchCommandString(scanned)) return true;
  const segment = scanned.split(/\|\||&&|[;\n|&]/u).at(-1).trim();
  const rawTokens = segment.split(/\s+/u).filter(Boolean);
  const envAt = rawTokens.findLastIndex((token) => token.split('/').at(-1) === 'env');
  if (envAt >= 0 && /^(?:-S|--split-string)=?$/u.test(rawTokens.at(-1) ?? '')) return true;
  const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
  const command = tokens[0]?.split('/').at(-1);
  if (command === 'node' && /^(?:-[A-Za-z]*[ep][A-Za-z]*|--eval|--print)$/u.test(tokens.at(-1) ?? '')) return true;
  if (command === 'script' && /^(?:-c|--command)=?$/u.test(tokens.at(-1) ?? '')) return true;
  if (tokens[0]?.split('/').at(-1) === 'eval' && (tokens.length === 1 || (tokens.length === 2 && tokens[1] === '--'))) return true;
  const npmAt = tokens.findIndex((token) => token.split('/').at(-1) === 'npm');
  if (npmAt < 0) return false;
  const execAt = tokens.findIndex((token, index) => index > npmAt && (token === 'exec' || token === 'x'));
  return execAt >= 0 && /^(?:-c|--call)=?$/u.test(tokens.at(-1) ?? '');
}

function endsWithWatchCommandString(scanned) {
  const segment = scanned.split(/\|\||&&|[;\n|&]/u).at(-1).trim();
  const tokens = segment.split(/\s+/u).filter(Boolean);
  return tokens.some((token) => token.split('/').at(-1) === 'watch') && commandAfterPrefixes(segment).length === 0;
}

function commandStringPayloads(command) {
  const ESCAPED_SPACE = '\u0004';
  const restore = (value) => value?.replaceAll(ESCAPED_SPACE, ' ');
  const payloads = [];
  for (const segment of splitSegments(command)) {
    const executable = commandAfterPrefixes(segment);
    const tokens = executable.split(/\s+/u).filter(Boolean);
    const rawTokens = segment.split(/\s+/u).filter(Boolean);
    const commandName = tokens[0]?.split('/').at(-1);
    if (commandName === 'eval' && tokens.length > 1) {
      const payloadAt = tokens[1] === '--' ? 2 : 1;
      if (payloadAt < tokens.length) payloads.push(restore(tokens.slice(payloadAt).join(' ')));
    }
    if (commandName === 'script') {
      for (let i = 1; i < tokens.length; i += 1) {
        if (tokens[i] === '-c' || tokens[i] === '--command') {
          if (tokens[i + 1] !== undefined) payloads.push(restore(tokens.slice(i + 1).join(' ')));
          break;
        }
        if (/^(?:-c|--command)=/u.test(tokens[i])) {
          payloads.push(restore(tokens[i].slice(tokens[i].indexOf('=') + 1)));
          break;
        }
      }
    }
    if (/(?:^|\/)(?:ba|da|z)?sh$/u.test(tokens[0] ?? '')) {
      for (let i = 1; i < tokens.length - 1; i += 1) {
        if (tokens[i] === '-c' || /^-[A-Za-z]*c[A-Za-z]*$/u.test(tokens[i])) {
          payloads.push(restore(tokens[i + 1]));
          break;
        }
        if (/^(?:-[A-Za-z]*[oO]|--(?:option|shopt))$/u.test(tokens[i])) i += 1;
      }
    }
    const npmAt = tokens.findIndex((token) => token.split('/').at(-1) === 'npm');
    const execAt = tokens.findIndex((token, index) => index > npmAt && (token === 'exec' || token === 'x'));
    if (npmAt >= 0 && execAt >= 0) {
      for (let i = execAt + 1; i < tokens.length; i += 1) {
        if (tokens[i] === '-c' || tokens[i] === '--call') {
          if (tokens[i + 1] !== undefined) payloads.push(restore(tokens[i + 1]));
          break;
        }
        if (/^(?:-c|--call)=/u.test(tokens[i])) {
          payloads.push(restore(tokens[i].slice(tokens[i].indexOf('=') + 1)));
          break;
        }
      }
    }
    const envAt = rawTokens.findIndex((token) => token.split('/').at(-1) === 'env');
    if (envAt >= 0) {
      for (let i = envAt + 1; i < rawTokens.length; i += 1) {
        if (rawTokens[i] === '-S' || rawTokens[i] === '--split-string') {
          if (rawTokens[i + 1] !== undefined) payloads.push(restore(rawTokens[i + 1]));
          break;
        }
        if (/^(?:-S|--split-string)=/u.test(rawTokens[i])) {
          payloads.push(restore(rawTokens[i].slice(rawTokens[i].indexOf('=') + 1)));
          break;
        }
      }
    }
    if (tokens[0]?.split('/').at(-1) === 'yarn') {
      const command = otherPackageCommandStart('yarn', tokens.slice(1));
      if ((command.foreach || command.workspace) && command.index < tokens.length - 1) payloads.push(tokens.slice(command.index + 1).join(' '));
    }
  }
  return payloads.filter(Boolean);
}

function dispatcherCommandPayloads(command) {
  const payloads = [];
  for (const segment of splitSegments(command)) {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    if (tokens[0]?.split('/').at(-1) !== 'find') continue;
    for (let index = 1; index < tokens.length; index += 1) {
      if (!/^-exec(?:dir)?$|^-ok(?:dir)?$/u.test(tokens[index])) continue;
      const start = index + 1;
      let end = start;
      while (end < tokens.length && !/^(?:\\;|;|\+)$/u.test(tokens[end])) end += 1;
      if (end > start) payloads.push(tokens.slice(start, end).filter((token) => token !== '{}').join(' '));
      index = end;
    }
  }
  return payloads;
}

function packageExecPayloads(command) {
  const payloads = [];
  for (const segment of splitSegments(command)) {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    const executable = tokens[0]?.split('/').at(-1);
    if (executable === 'npx' || executable === 'bunx') {
      const targetAt = skipCliOptions(tokens, 1, EXEC_OPTIONS_WITH_OPERANDS);
      if (targetAt < tokens.length) payloads.push(tokens.slice(targetAt).join(' '));
      continue;
    }
    if (executable === 'npm') {
      const verbAt = skipCliOptions(tokens, 1, NPM_OPTIONS_WITH_OPERANDS);
      if (tokens[verbAt] === 'exec' || tokens[verbAt] === 'x') {
        const targetAt = skipCliOptions(tokens, verbAt + 1, EXEC_OPTIONS_WITH_OPERANDS);
        if (targetAt < tokens.length) payloads.push(tokens.slice(targetAt).join(' '));
      }
      continue;
    }
    if (OTHER_PACKAGE_MANAGERS.has(executable)) {
      const rest = tokens.slice(1);
      const packageCommand = otherPackageCommandStart(executable, rest);
      if (/^(?:exec|x|dlx)$/u.test(rest[packageCommand.index] ?? '')) {
        const targetAt = skipCliOptions(rest, packageCommand.index + 1, EXEC_OPTIONS_WITH_OPERANDS);
        if (targetAt < rest.length) payloads.push(rest.slice(targetAt).join(' '));
      }
    }
  }
  return payloads;
}

function hasShellStdinProgram(command) {
  const pipelineSegments = command.split(/(?<![\\|])\|(?!\|)/u);
  for (let index = 0; index < pipelineSegments.length; index += 1) {
    const executable = commandAfterPrefixes(pipelineSegments[index]);
    const shell = executable.split(/\s+/u).filter(Boolean)[0];
    if (!/(?:^|\/)(?:ba|da|z)?sh$/u.test(shell ?? '')) continue;
    if (index > 0 || /(?:^|\s)(?:<<<|<(?![<(]))/u.test(executable)) return true;
  }
  return false;
}

const RUNTIME_OPTION_OPERANDS = {
  shell: new Set(['-c', '-o', '+o', '-O', '+O', '--rcfile', '--init-file']),
  node: new Set(['-C', '--conditions', '-e', '--eval', '-p', '--print', '-r', '--require', '--import', '--loader', '--experimental-loader', '--run']),
  python: new Set(['-W', '-X', '-c', '-m', '--check-hash-based-pycs']),
  perl: new Set(['-I', '-M', '-m', '-e', '-E']),
  ruby: new Set(['-C', '-E', '-F', '-I', '-K', '-e', '-r']),
  php: new Set(['-B', '-R', '-c', '-d', '-F', '-r', '-z', '--php-ini', '--define', '--process-begin', '--process-code', '--process-end', '--file', '--run']),
};

function runtimeProgram(segment) {
  const executable = commandAfterPrefixes(segment);
  const tokens = executable.split(/\s+/u).filter(Boolean);
  const runtime = tokens[0]?.split('/').at(-1) ?? '';
  const family = /^(?:ba|da|z)?sh$/u.test(runtime) ? 'shell' : /^python(?:\d+(?:\.\d+)*)?$/u.test(runtime) ? 'python' : runtime;
  if (!Object.hasOwn(RUNTIME_OPTION_OPERANDS, family)) return null;
  const operandOptions = RUNTIME_OPTION_OPERANDS[family];
  for (let index = 1; index < tokens.length; index += 1) {
    const token = tokens[index];
    if (/^(?:<<<|<<-?|<(?![<(]))/u.test(token)) return { executable, family, kind: 'stdin', token };
    if (token === '-') return { executable, family, kind: 'stdin', token };
    if (token === '--') {
      const script = tokens[index + 1];
      return script === undefined || /^(?:<<<|<<-?|<(?![<(]))/u.test(script)
        ? { executable, family, kind: 'stdin', token: script }
        : { executable, family, kind: 'file', token: script };
    }
    if (operandOptions.has(token)) {
      index += 1;
      continue;
    }
    if (token.startsWith('-')) continue;
    return { executable, family, kind: 'file', token };
  }
  return { executable, family, kind: 'none', token: null };
}

function hasRuntimeStdinProgram(command) {
  const pipelineSegments = command.split(/(?<![\\|])\|(?!\|)/u);
  for (let index = 0; index < pipelineSegments.length; index += 1) {
    const program = runtimeProgram(pipelineSegments[index]);
    if (program?.kind === 'stdin') return true;
    if (program?.kind === 'none' && (index > 0 || /(?:^|\s)(?:<<<|<<-?|<(?![<(]))/u.test(program.executable))) return true;
  }
  return false;
}

function git(cwd, ...args) {
  return execFileSync('git', ['-C', cwd, ...args], {
    encoding: 'utf8',
    stdio: ['ignore', 'pipe', 'ignore'],
  }).trim();
}

// The trust anchor for checked-in helpers: the remote default branch, which
// agents cannot move locally (pushes are review-gated). No local fallback —
// HEAD moves with any `git commit`, so a checkout without fetched origin
// refs has no reviewed base and the allowance fails closed.
function reviewedBaseRef(cwd) {
  for (const ref of ['origin/HEAD', 'origin/main', 'origin/master']) {
    try {
      git(cwd, 'rev-parse', '--verify', '--quiet', `${ref}^{commit}`);
      return ref;
    } catch {
      /* try the next anchor */
    }
  }
  return null;
}

// A Node script is a reviewed checked-in helper when its on-disk content is
// byte-identical to the blob at the reviewed base ref. Content provenance —
// not a filename allowlist an agent could extend (#141's fake-wrapper
// defect), and not mere trackedness an agent could satisfy with `git add`
// or a local commit. Any git failure, path escape, or mismatch fails closed.
function isReviewedCheckedInScript(token, cwd) {
  let file = resolve(cwd, token);
  if (!existsSync(file)) return false;
  // git prints physical paths; resolve symlinked segments (macOS /var → /private/var)
  // so the toplevel-relative computation compares like with like.
  try {
    file = realpathSync(file);
  } catch {
    return false;
  }
  try {
    const toplevel = git(cwd, 'rev-parse', '--show-toplevel');
    const relPath = relative(toplevel, file);
    if (relPath.startsWith('..') || isAbsolute(relPath)) return false;
    const base = reviewedBaseRef(cwd);
    if (base === null) return false;
    const reviewedBlob = git(cwd, 'rev-parse', '--verify', '--quiet', `${base}:${relPath.split(sep).join('/')}`);
    const onDiskBlob = git(cwd, 'hash-object', '--', file);
    return reviewedBlob === onDiskBlob;
  } catch {
    return false;
  }
}

function hasRuntimeScriptFile(command, cwd = process.cwd()) {
  const segments = splitSegments(command);
  for (let index = 0; index < segments.length; index += 1) {
    const program = runtimeProgram(segments[index]);
    if (program?.kind !== 'file') continue;
    const normalized = program.token.replaceAll('\u0004', ' ');
    if (program.family === 'node' && /(?:^|\/)tools\/agent-guard\/(?:run-guarded|arbiter)\.mjs$/u.test(normalized)) continue;
    // Checked-in Node helpers whose content matches the reviewed base ref
    // are the repository's own documented tooling (#191). Provenance only
    // vouches for bytes nothing can touch between hashing and execution:
    // another segment may cd away from the hook's cwd or rewrite the
    // file, and a substitution runs before the runtime does. Segment
    // order cannot prove safety — stripInertText promotes quoted
    // substitutions to *trailing* segments even though they execute
    // first — so only a single-segment, substitution-free command
    // qualifies. Other runtimes, modified scripts, and dynamic paths
    // stay denied.
    if (
      program.family === 'node' &&
      segments.length === 1 &&
      !/\$\(|`|[<>]\(/u.test(segments[index]) &&
      isReviewedCheckedInScript(normalized, cwd)
    )
      continue;
    return true;
  }
  return false;
}

function hasDirectScriptDispatch(command, cwd = process.cwd()) {
  const segments = splitSegments(command);
  for (let index = 0; index < segments.length; index += 1) {
    const tokens = commandAfterPrefixes(segments[index]).split(/\s+/u).filter(Boolean);
    if (tokens.length === 0) continue;
    const command = tokens[0];
    if (command === '.' || command === 'source') return tokens.length > 1;
    // A fully-quoted command word containing whitespace blanks to a bare
    // quote pair before this scan, so `'./la ne'` arrives as `''`. That is
    // a dispatch this hook cannot resolve or inspect — in a compound line
    // (where an earlier write can have created the file) it fails closed.
    if ((command === "''" || command === '""') && (segments.length > 1 || /\$\(|`|[<>]\(/u.test(segments[index]))) return true;
    const candidate = command.replaceAll('\u0004', ' ');
    const file = resolve(cwd, candidate);
    if (!existsSync(file)) {
      // A path-shaped dispatch target absent at hook time can still exist
      // when the shell reaches it: another segment — or a substitution in
      // this one — can create it and mark it executable in the same line
      // (`printf … > lane && chmod +x lane && ./lane`, #189). Segment
      // order cannot prove safety, because stripInertText promotes quoted
      // substitutions to *trailing* segments even though they execute
      // first. So any multi-segment line, and any remaining substitution
      // marker, fails closed. A lone dispatch of a missing path stays
      // allowed: nothing can have created it, and the shell fails on its
      // own. Bare words are PATH lookups, not dispatches.
      if (candidate.includes('/') && (segments.length > 1 || /\$\(|`|[<>]\(/u.test(segments[index]))) return true;
      continue;
    }
    try {
      const prefix = readFileSync(file).subarray(0, 4096);
      if (!prefix.includes(0)) return true;
    } catch {
      return true;
    }
  }
  return false;
}

function isWordCharacter(character) {
  return character !== undefined && !/[\s;&|]/u.test(character);
}

function followsEnvCommand(scanned) {
  const tokens = scanned
    .split(/\|\||&&|[;\n|&]/u)
    .at(-1)
    .trim()
    .split(/\s+/u)
    .filter(Boolean);
  const envAt = tokens.findLastIndex((token) => /(?:^|\/)env$/u.test(token));
  if (envAt < 0) return false;
  return tokens.slice(envAt + 1).every((token) => token.startsWith('-') || /^\w+=\S*$/u.test(token));
}

const DESTRUCTIVE_TARGET_COMMANDS = new Set(['rm', 'rmdir', 'unlink', 'shred', 'srm', 'mv', 'cp', 'install', 'dd', 'truncate', 'tee', 'ln', 'chmod', 'chown', 'chflags']);

// The executable a segment dispatches once wrapper commands (sudo, env,
// timeout, …) and their option operands are skipped — the same walk that
// hasProtectedEnvironmentAssignment does for assignments. `sudo rm` deletes
// what `rm` deletes.
function unwrappedCommandName(tokens) {
  let wrapper = null;
  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    const name = token.split('/').at(-1);
    if (ASSIGNMENT_TOKEN.test(token)) continue;
    if (ASSIGNMENT_PREFIX_COMMAND.test(name)) {
      wrapper = name;
      continue;
    }
    if (token.startsWith('-')) {
      if (WRAPPER_OPTION_OPERANDS[wrapper]?.has(token)) index += 1;
      continue;
    }
    if (wrapper === 'timeout' && /^\d+(?:\.\d+)?[smhd]?$/u.test(token)) continue;
    return name;
  }
  return null;
}

// A quoted word naming the filesystem target of a destructive or redirecting
// command is not inert prose: `rm -rf "$HOME/.cache/agent-guard"` deletes
// exactly what its unquoted spelling deletes (#198). Preserve such words so
// the guard-state scan sees them. quotedWord already rejects words with
// whitespace or separators, so prose (`git commit -m "rm the cache"`) and
// multi-word payloads can never be promoted through this path.
function isDestructiveTargetQuotedWord(scanned, word) {
  if (word === null) return false;
  const segment = scanned.split(/\|\||&&|[;\n|&]/u).at(-1).trim();
  const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
  if (tokens.length === 0) return false;
  if (/(?:^|\d)>>?$/u.test(tokens.at(-1))) return true;
  return DESTRUCTIVE_TARGET_COMMANDS.has(unwrappedCommandName(tokens));
}

// Quoting an argv word does not make it inert: `npm run "ci"` and
// `npx "vitest"` execute exactly the same programs as their unquoted forms.
// Preserve only words occupying a command or script slot; quoted prose passed
// to `git commit -m` or `gh pr create --body` remains blanked below.
function isExecutableQuotedWord(scanned, word) {
  if (word === null) return false;
  const segment = scanned.split(/\|\||&&|[;\n|&]/u).at(-1).trim();
  const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
  if (tokens.length === 0) {
    if (/^\w+=\S*$/u.test(word) && followsEnvCommand(scanned)) return true;
    return true;
  }
  if (tokens[0]?.split('/').at(-1) === 'corepack' && /^(?:pnpm|yarn)(?:@.+)?$/u.test(word)) return true;
  if (tokens[0]?.split('/').at(-1) === 'find') {
    if (/^-exec(?:dir)?$|^-ok(?:dir)?$/u.test(word)) return true;
    const actionAt = tokens.findLastIndex((token) => /^-exec(?:dir)?$|^-ok(?:dir)?$/u.test(token));
    if (actionAt >= 0 && !tokens.slice(actionAt + 1).some((token) => /^(?:\\;|;|\+)$/u.test(token))) return true;
  }

  let npmAt = -1;
  for (let i = tokens.length - 1; i >= 0; i -= 1) {
    if (/(?:^|\/)npm$/u.test(tokens[i])) {
      npmAt = i;
      break;
    }
  }
  if (npmAt >= 0) {
    const rest = tokens.slice(npmAt + 1);
    const aliasAt = rest.findIndex((token) => NPM_RUN_ALIASES.has(token));
    const candidates = aliasAt >= 0 ? rest.slice(aliasAt + 1) : rest;
    if (firstNpmScriptToken(candidates) === undefined) return true;
  }

  const manager = tokens[0]?.split('/').at(-1);
  if (OTHER_PACKAGE_MANAGERS.has(manager)) {
    const rest = tokens.slice(1);
    const command = otherPackageCommandStart(manager, rest);
    if (command.foreach) return isExecutableQuotedWord(rest.slice(command.index).join(' '), word);
    if (otherPackageScriptToken(manager, rest) === undefined) return true;
  }

  const last = tokens.at(-1);
  if (/^\w+=\S*$/u.test(word) && followsEnvCommand(scanned)) return true;
  if (last === 'npx' && /^(vitest|playwright|test-storybook)$/u.test(word)) return true;
  if (last === '--run' && tokens.some((token) => /(?:^|\/)node$/u.test(token))) return true;
  if (tokens.some((token) => /(?:^|\/)node$/u.test(token)) && /(?:^|\/)vitest(?:\/vitest)?\.mjs$/u.test(word)) return true;
  return /(?:^|\/)(?:node|electron)$/u.test(last ?? '') && word === '--test';
}

// Quotes are processed left to right: shell-wrapper payloads are unwrapped so
// the patterns can see them, executable argv words are preserved, and ordinary
// quoted text is blanked. Order matters — a commit message that merely mentions
// `bash -c "npm run test:e2e"` is blanked before its inner text is inspected.
export function stripInertText(command) {
  let scanned = '';
  let rest = transformHeredocs(command, 'strip');
  for (;;) {
    const match = QUOTED.exec(rest);
    if (!match) break;
    const quoted = match[0];
    scanned += rest.slice(0, match.index);
    rest = rest.slice(match.index + quoted.length);
    if (endsWithExecutableString(scanned)) {
      const inner = quoted.startsWith("$'")
        ? decodeAnsiCWord(quoted)
        : quoted.startsWith("'")
          ? quoted.slice(1, -1)
          : quoted.slice(1, -1).replace(/\\(["\\$`])/gu, '$1');
      rest = `${inner}${rest}`;
      scanned += '\n';
    } else {
      const substitutions = quoted.startsWith('"') ? commandSubstitutionBodies(quoted.slice(1, -1)) : [];
      if (substitutions.length > 0) {
        rest = `${substitutions.join('\n')}${rest}`;
        scanned += '""\n';
      } else if (isWordCharacter(scanned.at(-1)) || isWordCharacter(rest[0])) {
        // Shell quote removal concatenates adjacent fragments into one argv
        // word: c""i and "c"i both become ci. Preserve such fragments rather
        // than leaving quote bytes that hide executable or script names.
        const word = quotedWord(quoted);
        scanned += word ?? (quoted.startsWith("'") ? "''" : '""');
      } else if (isExecutableQuotedWord(scanned, quotedWord(quoted))) {
        scanned += quotedWord(quoted);
      } else if (isDestructiveTargetQuotedWord(scanned, quotedWord(quoted))) {
        scanned += quotedWord(quoted);
      } else {
        scanned += quoted.startsWith("'") ? "''" : '""';
      }
    }
  }
  const effective = normalizeUnquotedEscapes(scanned + rest);
  const substitutions = commandSubstitutionBodies(effective, { processSubstitutions: true });
  const payloads = commandStringPayloads(effective);
  const dispatchers = dispatcherCommandPayloads(effective);
  const dispatcherStrings = dispatchers.flatMap((payload) => commandStringPayloads(payload));
  const packageExecs = packageExecPayloads(effective);
  const promoted = [...substitutions, ...payloads, ...dispatchers, ...dispatcherStrings, ...packageExecs];
  return promoted.length > 0 ? `${effective}\n${promoted.join('\n')}` : effective;
}

// Codex's shell tool submits argv arrays; the patterns match command text.
export function normalizeCommand(command) {
  if (Array.isArray(command) && command.every((part) => typeof part === 'string')) {
    return command
      .map((part) => (/^[A-Za-z0-9_./:=+-]+$/u.test(part) ? part : `"${part.replace(/\\/gu, '\\\\').replace(/"/gu, '\\"').replace(/[$`]/gu, '\\$&')}"`))
      .join(' ');
  }
  return command;
}

// npm's documented spellings for running a script. `npm run-script test:e2e`
// is the same run as `npm run test:e2e`, and a matcher that only knows `run`
// blocks one and waves the other through.
const NPM_RUN_ALIASES = new Set(['run', 'run-script', 'rum', 'urn']);
const NPM_OPTIONS_WITH_OPERANDS = new Set(['-w', '--workspace', '-C', '--prefix', '--userconfig', '--cache', '--registry', '--scope', '--tag', '--otp']);
const NPM_IMPLICIT_SCRIPTS = new Set(['test', 'start', 'stop', 'restart']);

function firstNpmScriptToken(tokens) {
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token === '--') continue;
    if (NPM_OPTIONS_WITH_OPERANDS.has(token)) {
      i += 1;
      continue;
    }
    if (token.startsWith('-')) continue;
    return token;
  }
  return undefined;
}

/**
 * The script names an npm invocation would run, per shell segment.
 *
 * Tokenized rather than pattern-matched because npm accepts its own options
 * before and after the alias (`npm --silent run test:e2e`,
 * `npm --workspace foo run test:e2e`), and a regex that grabs the token
 * immediately after `npm` reads an option or the alias itself as the script.
 * The alias, when present, is the reliable anchor: the script is the first
 * non-option token after it. Without one, the first non-option token is the
 * script (`npm test`, `npm ci`).
 */
export function npmScriptNames(command) {
  const names = [];
  for (const segment of splitSegments(command)) {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    if (!/(?:^|\/)npm$/u.test(tokens[0] ?? '')) continue;
    const rest = tokens.slice(1);
    const aliasAt = rest.findIndex((token) => NPM_RUN_ALIASES.has(token));
    const candidates = aliasAt >= 0 ? rest.slice(aliasAt + 1) : rest;
    const script = firstNpmScriptToken(candidates);
    if (aliasAt < 0 && !NPM_IMPLICIT_SCRIPTS.has(script)) continue;
    if (script !== undefined) names.push(script);
  }
  return names;
}

const OTHER_PACKAGE_MANAGERS = new Set(['pnpm', 'yarn', 'bun']);
const OTHER_PACKAGE_OPTIONS_WITH_OPERANDS = new Set([
  '-C',
  '-F',
  '--cwd',
  '--dir',
  '--filter',
  '--workspace',
  '-w',
]);

function firstOtherPackageScriptToken(tokens) {
  for (let i = 0; i < tokens.length; i += 1) {
    const token = tokens[i];
    if (token === '--') continue;
    if (OTHER_PACKAGE_OPTIONS_WITH_OPERANDS.has(token)) {
      i += 1;
      continue;
    }
    if (token.startsWith('-')) continue;
    return token;
  }
  return undefined;
}

function otherPackageCommandStart(manager, tokens) {
  let index = 0;
  while (index < tokens.length) {
    const token = tokens[index];
    if (token === '--') {
      index += 1;
      break;
    }
    if (OTHER_PACKAGE_OPTIONS_WITH_OPERANDS.has(token)) {
      index += 2;
      continue;
    }
    if (token.startsWith('-')) {
      index += 1;
      continue;
    }
    break;
  }
  let workspace = false;
  if (manager === 'yarn' && tokens[index] === 'workspace') {
    workspace = true;
    index += 2; // selector plus workspace name
    while (tokens[index]?.startsWith('-')) index += 1;
  }
  if (manager === 'yarn' && tokens[index] === 'workspaces' && tokens[index + 1] === 'foreach') {
    index += 2;
    const optionsWithOperands = new Set(['--from', '--include', '--exclude', '--jobs', '-j']);
    while (tokens[index]?.startsWith('-')) {
      const option = tokens[index++];
      if (optionsWithOperands.has(option) && index < tokens.length) index += 1;
    }
    return { foreach: true, index, workspace: false };
  }
  return { foreach: false, index, workspace };
}

function otherPackageScriptToken(manager, tokens) {
  let { index } = otherPackageCommandStart(manager, tokens);
  if (tokens[index] === 'run' || tokens[index] === 'run-script') index += 1;
  return firstOtherPackageScriptToken(tokens.slice(index));
}

// pnpm, Yarn and Bun all expose package scripts as `run <script>` and also
// accept a direct script spelling. They share the same heavy-lane policy as
// npm; otherwise changing package manager would silently remove admission.
export function otherPackageScriptNames(command) {
  const names = [];
  for (const segment of splitSegments(command)) {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    const manager = tokens[0]?.split('/').at(-1);
    if (!OTHER_PACKAGE_MANAGERS.has(manager)) continue;
    const rest = tokens.slice(1);
    const script = otherPackageScriptToken(manager, rest);
    if (script !== undefined) names.push(script);
  }
  return names;
}

const TEST_BINARIES = new Set(['vitest', 'c8', 'playwright', 'test-storybook']);
const EXEC_OPTIONS_WITH_OPERANDS = new Set([...NPM_OPTIONS_WITH_OPERANDS, ...OTHER_PACKAGE_OPTIONS_WITH_OPERANDS, '--package', '-p']);
const NODE_OPTIONS_WITH_OPERANDS = new Set([
  '-r',
  '--require',
  '--import',
  '--loader',
  '--experimental-loader',
  '--conditions',
  '-C',
  '--input-type',
  '--inspect-port',
  '--diagnostic-dir',
  '--redirect-warnings',
  '--report-directory',
  '--report-filename',
  '--openssl-config',
  '--title',
  '--icu-data-dir',
  '--experimental-policy',
  '--policy-integrity',
  '--env-file',
  '--env-file-if-exists',
]);

function skipCliOptions(tokens, start, optionsWithOperands) {
  let index = start;
  while (index < tokens.length) {
    const token = tokens[index];
    if (token === '--') return index + 1;
    if (optionsWithOperands.has(token)) {
      index += 2;
      continue;
    }
    if (token.startsWith('-')) {
      index += 1;
      continue;
    }
    break;
  }
  return index;
}

function directTestBinaryThroughExec(segment) {
  const tokens = segment.split(/\s+/u).filter(Boolean);
  const command = tokens[0]?.split('/').at(-1);
  if (command === 'npx' || command === 'bunx') {
    const binaryAt = skipCliOptions(tokens, 1, EXEC_OPTIONS_WITH_OPERANDS);
    return TEST_BINARIES.has(tokens[binaryAt]?.split('/').at(-1));
  }
  if (!['npm', 'pnpm', 'yarn', 'bun'].includes(command)) return false;
  const options = EXEC_OPTIONS_WITH_OPERANDS;
  const verbAt = skipCliOptions(tokens, 1, options);
  if (!['exec', 'x', 'dlx'].includes(tokens[verbAt])) return false;
  const binaryAt = skipCliOptions(tokens, verbAt + 1, options);
  return TEST_BINARIES.has(tokens[binaryAt]?.split('/').at(-1));
}

function directVitestNodeEntry(segment) {
  const tokens = segment.split(/\s+/u).filter(Boolean);
  if (tokens[0]?.split('/').at(-1) !== 'node') return false;
  let moduleAt = 1;
  while (moduleAt < tokens.length) {
    const option = tokens[moduleAt];
    if (option === '--') {
      moduleAt += 1;
      break;
    }
    if (NODE_OPTIONS_WITH_OPERANDS.has(option)) {
      moduleAt += 2;
      continue;
    }
    if (option.startsWith('-')) {
      moduleAt += 1;
      continue;
    }
    break;
  }
  return /(?:^|\/)vitest(?:\/vitest)?\.mjs$/u.test(tokens[moduleAt] ?? '');
}

function incompleteXargsCommand(segment, executableSegment) {
  if (!/(?:^|\s)(?:\S*\/)?xargs(?:\s|$)/u.test(segment)) return false;
  const tokens = executableSegment.split(/\s+/u).filter(Boolean);
  const command = tokens[0]?.split('/').at(-1);
  if (command === 'npx' || command === 'bunx') return skipCliOptions(tokens, 1, EXEC_OPTIONS_WITH_OPERANDS) >= tokens.length;
  if (command === 'npm') return tokens.length === 1 || NPM_RUN_ALIASES.has(tokens.at(-1));
  if (OTHER_PACKAGE_MANAGERS.has(command)) return tokens.length === 1 || /^(?:run|run-script|exec|x|dlx)$/u.test(tokens.at(-1));
  return /^(?:node|electron|(?:ba|da|z)?sh)$/u.test(command ?? '') && tokens.length === 1;
}

function packageScriptNames(command) {
  return [...npmScriptNames(command), ...nodeRunScriptNames(command), ...otherPackageScriptNames(command)];
}

function hasRuntimeShellExpansion(word) {
  let quote = null;
  for (let index = 0; index < word.length; index += 1) {
    const character = word[index];
    if (character === '\\' && quote !== "'") {
      index += 1;
      continue;
    }
    if (quote === "'") {
      if (character === "'") quote = null;
      continue;
    }
    if (character === "'") {
      quote = "'";
      continue;
    }
    if (character === '"') {
      quote = quote === '"' ? null : '"';
      continue;
    }
    if (character === '`') return true;
    if (character === '$' && /[({A-Za-z0-9_@*#?!$-]/u.test(word[index + 1] ?? '')) return true;
    if (quote === null && '*?[{'.includes(character)) return true;
  }
  return false;
}

function hasDynamicPackageScript(command) {
  return packageScriptNames(command).some((script) => hasRuntimeShellExpansion(script));
}

function hasDynamicExecutionPosition(command) {
  for (const segment of splitSegments(command)) {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    if (tokens.length === 0) continue;
    if (hasRuntimeShellExpansion(tokens[0])) return true;
    const executable = tokens[0].split('/').at(-1);
    if (executable === 'npx' || executable === 'bunx') {
      const targetAt = skipCliOptions(tokens, 1, EXEC_OPTIONS_WITH_OPERANDS);
      if (hasRuntimeShellExpansion(tokens[targetAt] ?? '')) return true;
    }
    if (executable === 'npm') {
      const verbAt = skipCliOptions(tokens, 1, NPM_OPTIONS_WITH_OPERANDS);
      if (hasRuntimeShellExpansion(tokens[verbAt] ?? '')) return true;
      if (tokens[verbAt] === 'exec' || tokens[verbAt] === 'x') {
        const targetAt = skipCliOptions(tokens, verbAt + 1, EXEC_OPTIONS_WITH_OPERANDS);
        if (hasRuntimeShellExpansion(tokens[targetAt] ?? '')) return true;
      }
    }
    if (OTHER_PACKAGE_MANAGERS.has(executable)) {
      const rest = tokens.slice(1);
      const command = otherPackageCommandStart(executable, rest);
      if (hasRuntimeShellExpansion(rest[command.index] ?? '')) return true;
      if (/^(?:exec|x|dlx)$/u.test(rest[command.index] ?? '')) {
        const targetAt = skipCliOptions(rest, command.index + 1, EXEC_OPTIONS_WITH_OPERANDS);
        if (hasRuntimeShellExpansion(rest[targetAt] ?? '')) return true;
      }
    }
    if (executable === 'node') {
      for (let index = 1; index < tokens.length; index += 1) {
        if (tokens[index] === '--run' && hasRuntimeShellExpansion(tokens[index + 1] ?? '')) return true;
        if (tokens[index].startsWith('--run=') && hasRuntimeShellExpansion(tokens[index].slice('--run='.length))) return true;
      }
    }
    if (/^(?:ba|da|z)?sh$/u.test(executable ?? '')) {
      for (let index = 1; index < tokens.length - 1; index += 1) {
        if (tokens[index] === '-c' || /^-[A-Za-z]*c[A-Za-z]*$/u.test(tokens[index])) {
          if (hasRuntimeShellExpansion(tokens[index + 1])) return true;
          break;
        }
        if (/^(?:-[A-Za-z]*[oO]|--(?:option|shopt))$/u.test(tokens[index])) index += 1;
      }
    }
    if (executable === 'eval') {
      const payloadAt = tokens[1] === '--' ? 2 : 1;
      if (hasRuntimeShellExpansion(tokens[payloadAt] ?? '')) return true;
    }
  }
  return false;
}

function hasDynamicIdentityRemoval(command) {
  for (const match of command.matchAll(/(?:^|\s)(?:-u([^\s;&|]+)|(?:-u|--unset)(?:=|\s+)([^\s;&|]+))/gu)) {
    if (hasRuntimeShellExpansion(match[1] ?? match[2] ?? '')) return true;
  }
  for (const segment of splitSegments(command)) {
    const tokens = segment.split(/\s+/u).filter(Boolean);
    const executable = tokens[0]?.split('/').at(-1);
    if (executable !== 'unset' && executable !== 'export') continue;
    let operandAt = 1;
    while (/^(?:--|-v|-f|-n)$/u.test(tokens[operandAt] ?? '')) operandAt += 1;
    if (hasRuntimeShellExpansion(tokens[operandAt] ?? '')) return true;
  }
  return false;
}

function isProtectedEnvironmentName(name) {
  return /^(?:CI|GITHUB_ACTIONS|CONTINUOUS_INTEGRATION|BUILDKITE|GITLAB_CI|JENKINS_URL|NODE_OPTIONS|BASH_ENV|ENV|ZDOTDIR|PERL5OPT|RUBYOPT|PYTHONPATH|PYTHONHOME|PHPRC|PHP_INI_SCAN_DIR|LD_PRELOAD|DYLD_INSERT_LIBRARIES|GIT_SSH_COMMAND|GIT_CONFIG_COUNT|PATH|AGENT_GUARD_FORCE|AGENT_GUARD_ASSUME_HUMAN|AGENT_GUARD_STATE_DIR|AGENT_GUARDED|CLAUDECODE|CLAUDE_CODE_ENTRYPOINT|AI_AGENT|CODEX_\w+|CURSOR_\w+|\w*_AGENT)$/u.test(name);
}

function hasProtectedEnvironmentMutation(command) {
  for (const segment of splitSegments(command)) {
    const tokens = segment.split(/\s+/u).filter(Boolean);
    const executable = tokens[0]?.split('/').at(-1);
    if (executable === 'printf') {
      const variableAt = tokens.findIndex((token) => token === '-v');
      if (variableAt >= 0) {
        const target = tokens[variableAt + 1] ?? '';
        if (hasRuntimeShellExpansion(target) || isProtectedEnvironmentName(target.replace(/^["']|["']$/gu, ''))) return true;
      }
    }
    if (/^(?:export|declare|typeset|readonly|local|read)$/u.test(executable ?? '')) {
      for (const token of tokens.slice(1)) {
        if (token.startsWith('-')) continue;
        const target = token.replace(/^["']|["']$/gu, '').split('=')[0];
        if (hasRuntimeShellExpansion(token) || isProtectedEnvironmentName(target)) return true;
      }
    }
  }
  return false;
}

function hasDynamicDirectoryTarget(command) {
  for (const segment of splitSegments(command)) {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    const executable = tokens[0]?.split('/').at(-1);
    if (executable === 'cd' || executable === 'pushd') {
      let targetAt = 1;
      while (/^(?:-[LPe@n]+|--|[+-]\d+)$/u.test(tokens[targetAt] ?? '')) targetAt += 1;
      if (hasRuntimeShellExpansion(tokens[targetAt] ?? '')) return true;
    }
    if (directoryOptionTargets(segment).some(({ target }) => hasRuntimeShellExpansion(target))) return true;
  }
  return false;
}

function expandSimpleBraces(value, limit = 32) {
  const variants = [value];
  for (let index = 0; index < variants.length && variants.length <= limit; index += 1) {
    const match = /\{([^{}]*)\}/u.exec(variants[index]);
    if (!match || !match[1].includes(',')) continue;
    variants.splice(index, 1, ...match[1].split(',').map((part) => variants[index].slice(0, match.index) + part + variants[index].slice(match.index + match[0].length)));
    index -= 1;
  }
  return variants.slice(0, limit);
}

function shellPatternCanMatch(component, target) {
  for (const variant of expandSimpleBraces(component.replace(/\\(.)/gu, '$1'))) {
    let source = '^';
    for (let index = 0; index < variant.length; index += 1) {
      const character = variant[index];
      if (character === '*') source += '.*';
      else if (character === '?') source += '.';
      else if (character === '[') {
        const close = variant.indexOf(']', index + 1);
        if (close < 0) source += '\\[';
        else {
          const members = variant.slice(index + 1, close).replace(/^!/u, '^').replace(/\\/gu, '\\\\');
          source += `[${members}]`;
          index = close;
        }
      }
      else if (character === '$') {
        if (variant[index + 1] === '{') {
          const close = variant.indexOf('}', index + 2);
          index = close < 0 ? variant.length : close;
        } else {
          while (/[A-Za-z0-9_]/u.test(variant[index + 1] ?? '')) index += 1;
        }
        source += '.*';
      } else {
        source += character.replace(/[\\^$.*+?()[\]{}|]/gu, '\\$&');
      }
    }
    try {
      if (new RegExp(`${source}$`, 'u').test(target)) return true;
    } catch (error) {
      if (!(error instanceof SyntaxError)) throw error;
    }
  }
  return false;
}

function referencesGuardState(command) {
  for (const rawWord of command.match(/[^\s<>;&|]+/gu) ?? []) {
    const word = rawWord.slice(rawWord.lastIndexOf('=') + 1).replace(/^['"]|['"]$/gu, '');
    const components = word.split('/');
    // Every component that can match the store name is a candidate anchor: a
    // variable component matches anything, so stopping at the first match
    // lets `$XDG_CACHE_HOME/agent-guard` shift the real component into child
    // position and walk past the checks below.
    for (let guardAt = 0; guardAt < components.length; guardAt += 1) {
      if (!shellPatternCanMatch(components[guardAt], 'agent-guard')) continue;
      const tail = [];
      for (const component of components.slice(guardAt + 1)) {
        if (component === '' || component === '.') continue;
        if (component === '..') tail.pop();
        else tail.push(component);
      }
      const child = tail[0];
      if (child !== undefined && child !== '') {
        if (['leases', 'admission.lock', 'machine-token', 'lane-peaks'].some((target) => shellPatternCanMatch(child, target))) return true;
        continue;
      }
      // No sensitive descendant named: this is the whole-store case
      // (`rm -rf ~/.cache/agent-guard`). It is a state-store reference only
      // when the leading chain can reach a machine cache root: a cache-named
      // component anywhere before it (glob-aware, so `.c[a]che` still
      // counts), or a variable expansion as the immediate parent. A bare
      // word (`rg agent-guard docs`, `echo agent-guard`) or a repo source
      // path (`ls tools/agent-guard`, absolute or home-anchored) is a
      // mention, not the store (#190).
      const prefixComponents = components.slice(0, guardAt);
      // A pure variable expansion matches any target under
      // shellPatternCanMatch, which would turn $PWD/tools/agent-guard into a
      // cache path. Variables count only as the immediate parent (below);
      // the cache-name scan needs literal or glob evidence.
      const cacheNamed = prefixComponents.some(
        (component) =>
          !/^\$\{?[A-Za-z_][A-Za-z0-9_]*\}?$/u.test(component) &&
          ['.cache', 'Caches', 'caches', 'cache'].some((target) => shellPatternCanMatch(component, target)),
      );
      if (cacheNamed || /[$]/u.test(prefixComponents.at(-1) ?? '')) return true;
    }
  }
  return false;
}

// Node >=22 exposes package.json scripts through `node --run <script>` and
// `node --run=<script>`. Those spellings have the same admission policy as npm.
export function nodeRunScriptNames(command) {
  const names = [];
  for (const segment of splitSegments(command)) {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    if (!/(?:^|\/)node$/u.test(tokens[0] ?? '')) continue;
    const rest = tokens.slice(1);
    for (let i = 0; i < rest.length; i += 1) {
      const token = rest[i];
      if (token.startsWith('--run=')) {
        const script = token.slice('--run='.length);
        if (script) names.push(script);
        break;
      }
      if (token === '--run' && rest[i + 1] !== undefined) {
        names.push(rest[i + 1]);
        break;
      }
    }
  }
  return names;
}

function isUnguardedInnerScript(command) {
  return packageScriptNames(command).some((script) => /:(?:run|inner)$/u.test(script));
}

/**
 * Heavy-lane detection for a raw command line.
 *
 * Narrower than lib/policy.mjs's label matching on purpose: a hook sees every
 * shell command an agent runs, so matching the bare word "perf" anywhere would
 * deny `grep perf src/`. Only npm script invocations and the test binaries
 * themselves count here.
 */
export function heavyLaneFor(command) {
  for (const script of packageScriptNames(command)) {
    const lane = HEAVY_LANES.find((entry) => entry.pattern.test(script));
    if (lane) return lane;
  }
  for (const segment of splitSegments(command)) {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    const executable = tokens[0]?.split('/').at(-1);
    if (executable === 'playwright' && tokens[1] === 'test') return HEAVY_LANES.find((entry) => entry.id === 'e2e');
    if (executable === 'test-storybook') return HEAVY_LANES.find((entry) => entry.id === 'stories');
    if (executable === 'npx' || executable === 'bunx') {
      const binaryAt = skipCliOptions(tokens, 1, EXEC_OPTIONS_WITH_OPERANDS);
      const binary = tokens[binaryAt]?.split('/').at(-1);
      if (binary === 'playwright' && tokens[binaryAt + 1] === 'test') return HEAVY_LANES.find((entry) => entry.id === 'e2e');
      if (binary === 'test-storybook') return HEAVY_LANES.find((entry) => entry.id === 'stories');
    }
  }
  return null;
}

function commandAfterPrefixes(segment) {
  const tokens = normalizeUnquotedEscapes(segment).split(/\s+/u).filter(Boolean);
  let index = 0;
  while (index < tokens.length) {
    while (/^\w+=\S*$/u.test(tokens[index] ?? '')) index += 1;
    if (/^(?:then|do|else|elif|if|while|until|coproc|!)$/u.test(tokens[index] ?? '')) {
      index += 1;
      continue;
    }
    if (/^[A-Za-z_][A-Za-z0-9_]*\(\)\{$/u.test(tokens[index] ?? '')) {
      index += 1;
      continue;
    }
    if (/^[A-Za-z_][A-Za-z0-9_]*\(\)$/u.test(tokens[index] ?? '') && tokens[index + 1] === '{') {
      index += 2;
      continue;
    }
    if (/^[A-Za-z_][A-Za-z0-9_]*$/u.test(tokens[index] ?? '') && tokens[index + 1] === '()' && tokens[index + 2] === '{') {
      index += 3;
      continue;
    }
    if (tokens[index] === 'case') {
      const patternAt = tokens.findIndex((token, tokenAt) => tokenAt > index && token.endsWith(')'));
      if (patternAt >= 0) {
        index = patternAt + 1;
        continue;
      }
    }
    if (tokens[index] === 'function' && tokens[index + 1]) {
      index += 2;
      if (tokens[index] === '{') index += 1;
      continue;
    }
    const command = tokens[index]?.split('/').at(-1);
    if (command === 'command' || command === 'builtin') {
      if (tokens.slice(index + 1).some((token) => token === '-v' || token === '-V')) break;
      index += 1;
      while (tokens[index]?.startsWith('-')) index += 1;
      continue;
    }
    if (command === 'time' || command === 'nohup') {
      index += 1;
      while (tokens[index]?.startsWith('-')) index += 1;
      continue;
    }
    if (command === 'corepack') {
      const proxy = /^(?:pnpm|yarn)(?:@.+)?$/u.exec(tokens[index + 1]?.split('/').at(-1) ?? '')?.[0];
      if (!proxy) break;
      tokens[index + 1] = proxy.split('@')[0];
      index += 1;
      continue;
    }
    if (command === 'nice') {
      index += 1;
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if ((option === '-n' || option === '--adjustment') && index < tokens.length) index += 1;
      }
      continue;
    }
    if (command === 'ionice') {
      index += 1;
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if (/^(?:-c|--class|-n|--classdata)$/u.test(option) && index < tokens.length) index += 1;
      }
      continue;
    }
    if (command === 'parallel') {
      index += 1;
      const optionsWithOperands = new Set([
        '-a', '--arg-file', '-j', '--jobs', '-S', '--sshlogin', '--sshloginfile', '--workdir', '--results', '--joblog', '--tmpdir',
      ]);
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if (option === '--') break;
        if (optionsWithOperands.has(option) && index < tokens.length) index += 1;
      }
      continue;
    }
    if (command === 'timeout') {
      index += 1;
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if (/^(?:-k|--kill-after|-s|--signal)$/u.test(option) && index < tokens.length) index += 1;
      }
      if (index < tokens.length) index += 1; // duration
      continue;
    }
    if (command === 'watch') {
      index += 1;
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if (/^(?:-n|--interval|--equexit)$/u.test(option) && index < tokens.length) index += 1;
      }
      continue;
    }
    if (command === 'xargs') {
      index += 1;
      const optionsWithOperands = new Set([
        '-a',
        '--arg-file',
        '-d',
        '--delimiter',
        '-E',
        '-I',
        '-J',
        '-L',
        '--max-lines',
        '-n',
        '--max-args',
        '-P',
        '--max-procs',
        '--process-slot-var',
        '-R',
        '-S',
        '-s',
        '--max-chars',
      ]);
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if (option === '--') break;
        if (optionsWithOperands.has(option) && index < tokens.length) index += 1;
      }
      continue;
    }
    if (command === 'setsid') {
      index += 1;
      while (tokens[index]?.startsWith('-')) index += 1;
      continue;
    }
    if (command === 'taskset') {
      index += 1;
      let affinityConsumed = false;
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if (option === '--') break;
        if (option === '-c' || option === '--cpu-list') {
          if (index < tokens.length) index += 1;
          affinityConsumed = true;
        } else if (/^(?:-c.+|--cpu-list=.+)$/u.test(option)) {
          affinityConsumed = true;
        }
      }
      if (!affinityConsumed && index < tokens.length) index += 1; // affinity mask
      continue;
    }
    if (command === 'stdbuf') {
      index += 1;
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if (/^-[ioe]$/u.test(option) && index < tokens.length) index += 1;
      }
      continue;
    }
    if (command === 'exec') {
      index += 1;
      while (tokens[index]?.startsWith('-')) {
        const option = tokens[index++];
        if (option === '-a' && index < tokens.length) index += 1;
      }
      continue;
    }
    if (command === 'env') {
      index += 1;
      while (index < tokens.length) {
        const token = tokens[index];
        if (/^\w+=\S*$/u.test(token)) {
          index += 1;
          continue;
        }
        if (token.startsWith('-')) {
          index += 1;
          if (/^(?:-u|--unset|--chdir|-C|-S|--split-string)$/u.test(token) && index < tokens.length) index += 1;
          continue;
        }
        break;
      }
      continue;
    }
    break;
  }
  return tokens.slice(index).join(' ');
}

const PROTECTED_ENV_ASSIGNMENT = /^["']?(?:NODE_OPTIONS|BASH_ENV|ENV|ZDOTDIR|PERL5OPT|RUBYOPT|PYTHONPATH|PYTHONHOME|PHPRC|PHP_INI_SCAN_DIR|LD_PRELOAD|DYLD_INSERT_LIBRARIES|GIT_SSH_COMMAND|GIT_CONFIG_COUNT|PATH)=/u;
const ASSIGNMENT_TOKEN = /^["']?[A-Za-z_][A-Za-z0-9_]*=/u;
const ASSIGNMENT_PREFIX_COMMAND = /^(?:command|builtin|env|exec|time|nice|nohup|timeout|setsid|stdbuf|sudo|doas)$/u;
// Options of the wrappers above whose operand is a separate following word.
// An operand left in place would read as the command and end the assignment
// scan early (`sudo -u root 'NODE_OPTIONS=…' npm run lint`).
const WRAPPER_OPTION_OPERANDS = {
  sudo: new Set(['-u', '--user', '-g', '--group', '-h', '--host', '-p', '--prompt', '-C', '--close-from', '-D', '--chdir', '-R', '--chroot', '-T', '--command-timeout', '-U', '--other-user']),
  doas: new Set(['-u']),
  env: new Set(['-C', '--chdir', '-u', '--unset', '-S', '--split-string']),
  timeout: new Set(['-k', '--kill-after', '-s', '--signal']),
  nice: new Set(['-n', '--adjustment']),
  stdbuf: new Set(['-i', '-o', '-e']),
};
const EMPTY_OPERAND_OPTIONS = new Set();

// splitSegments is quote-unaware, so a separator inside a quoted argument
// would open a phantom segment whose first token looks like an assignment.
// Blank separators inside quotes (the quoted word survives as one token).
function maskQuotedSeparators(command) {
  let scanned = '';
  let rest = command;
  for (;;) {
    const match = QUOTED.exec(rest);
    if (!match) break;
    scanned += rest.slice(0, match.index) + match[0].replace(/[;\n|&]/gu, ' ');
    rest = rest.slice(match.index + match[0].length);
  }
  return scanned + rest;
}

// A protected VAR=… is an override only where a shell or env-style wrapper
// applies it to a command's environment: the assignment prefix of a segment,
// or the argument list of env/sudo/timeout/…, where quoting does not defuse
// it (`env 'NODE_OPTIONS=…' npm run lint` sets the variable all the same).
export function hasProtectedEnvironmentAssignment(command) {
  for (const segment of splitSegments(maskQuotedSeparators(command))) {
    const tokens = segment.split(/\s+/u).filter(Boolean);
    let index = 0;
    while (index < tokens.length) {
      const token = tokens[index];
      if (ASSIGNMENT_TOKEN.test(token)) {
        if (PROTECTED_ENV_ASSIGNMENT.test(token)) return true;
        index += 1;
        continue;
      }
      const name = token.replace(/^["']+|["']+$/gu, '').split('/').at(-1);
      if (ASSIGNMENT_PREFIX_COMMAND.test(name)) {
        index += 1;
        // Skip option words together with their separate operands, so an
        // operand never masquerades as the command and ends the scan while a
        // quoted assignment still follows: sudo's run form is
        // `sudo … [-u user] [VAR=value] … [command]`.
        const operandOptions = WRAPPER_OPTION_OPERANDS[name] ?? EMPTY_OPERAND_OPTIONS;
        while (tokens[index]?.startsWith('-')) {
          const option = tokens[index];
          index += 1;
          if (operandOptions.has(option)) index += 1;
        }
        if ((name === 'timeout' || name === 'nice') && /^\d/u.test(tokens[index] ?? '')) index += 1;
        continue;
      }
      break;
    }
  }
  return false;
}

export function evaluateCommand(command, { cwd = process.cwd() } = {}) {
  if (typeof command !== 'string' || command.length === 0) return { allow: true };
  const dynamicCommand = maskNonShellHeredocs(command);
  // Executable-loading environment overrides are denied only in positions
  // that reach a command's environment: leading VAR=… prefixes and the
  // argument list of env-style wrappers — where a quoted assignment is still
  // an assignment. Elsewhere, quoted protected-variable text is a mention (a
  // commit message, a grep pattern, a printf payload), not an override; the
  // TAMPERING rule still scans the stripped text as the unquoted backstop.
  if (hasProtectedEnvironmentAssignment(dynamicCommand)) {
    return {
      allow: false,
      reason:
        'Blocked an executable-loading environment override: preload, startup, loader, config, and command-resolution variables execute or select code before the requested command and ' +
        `can dispatch a protected lane outside static classification. Remove the override. ${USE_ENTRYPOINT}`,
    };
  }
  if (hasProtectedEnvironmentMutation(dynamicCommand)) {
    return {
      allow: false,
      reason: `Blocked a protected environment mutation before run-guarded.mjs: shell builtins cannot manufacture a CI exemption or alter guard identity. ${GUIDANCE}`,
    };
  }
  if (hasDynamicIdentityRemoval(dynamicCommand)) {
    return {
      allow: false,
      reason: `Blocked a runtime-computed identity removal before run-guarded.mjs: it could erase the active harness marker. ${GUIDANCE}`,
    };
  }
  if (hasDynamicPackageScript(dynamicCommand) || hasDynamicExecutionPosition(dynamicCommand)) {
    return {
      allow: false,
      reason:
        'Blocked a runtime-computed executable, package command, or script: shell expansion can resolve to a protected ' +
        `lane after static admission checks. Use the guarded entrypoint with literal command slots. ${USE_ENTRYPOINT}`,
    };
  }
  if (hasRuntimeStdinProgram(command)) {
    return {
      allow: false,
      reason: `Blocked a runtime program supplied through stdin: its payload can dispatch a protected lane after static admission checks. ${USE_ENTRYPOINT}`,
    };
  }
  const effective = stripInertText(command);

  if (/(?:^|\s)--checkpoint-action(?:=|\s+)exec(?:=|\s|$)/u.test(dynamicCommand)) {
    return {
      allow: false,
      reason: `Blocked a tar checkpoint exec action: it runs an arbitrary shell command outside guarded classification. ${USE_ENTRYPOINT}`,
    };
  }

  if (hasRuntimeScriptFile(effective, cwd)) {
    return {
      allow: false,
      reason: `Blocked direct runtime script-file dispatch: a script can launch a protected lane after static shell checks. Run it through the repository's guarded entrypoint. ${USE_ENTRYPOINT}`,
    };
  }

  if (hasDirectScriptDispatch(effective, cwd)) {
    return {
      allow: false,
      reason: `Blocked direct script execution or shell sourcing: script contents can dispatch a protected lane after static command checks. ${USE_ENTRYPOINT}`,
    };
  }

  // Inline runtime programs can synchronously dispatch arbitrary child
  // commands whose strings are not shell syntax. Static shell classification
  // cannot authenticate their contents, so agent commands must use checked-in
  // scripts rather than executable-program options.
  if (splitSegments(effective).some((segment) => {
    const tokens = commandAfterPrefixes(segment).split(/\s+/u).filter(Boolean);
    const runtime = tokens[0]?.split('/').at(-1) ?? '';
    const options = tokens.slice(1);
    if (runtime === 'node') {
      return options.some((token) => /^(?:-[A-Za-z]*[ep]|--eval(?:=|$)|--print(?:=|$)|-r(?:=|$)|--require(?:=|$)|--import(?:=|$)|--loader(?:=|$)|--experimental-loader(?:=|$))/u.test(token));
    }
    if (/^python(?:\d+(?:\.\d+)*)?$/u.test(runtime)) return options.some((token) => /^-[bBdiIOqRsSuvVx]*c/u.test(token));
    if (runtime === 'perl') return options.some((token) => /^(?:-[wWT]*[eE]$|-[eE].+)/u.test(token));
    if (runtime === 'ruby') return options.some((token) => /^(?:-[wWd]*e$|-e.+)/u.test(token));
    if (runtime === 'php') return options.some((token) => /^(?:-[rBRE]|--(?:run|process-begin|process-code|process-end)(?:=|$))/u.test(token));
    if (/^(?:[gmn]?awk)$/u.test(runtime)) {
      if (options.some((token) => /^(?:-f|--file)(?:=|$)/u.test(token))) return false;
      return !options.every((token) => /^(?:--help|--version|-W(?:help|version))$/u.test(token));
    }
    return false;
  })) {
    return {
      allow: false,
      reason: `Blocked an inline runtime program: it can dispatch a protected lane after static admission checks. Use a checked-in script through the guarded entrypoint. ${USE_ENTRYPOINT}`,
    };
  }

  if (referencesGuardState(effective)) {
    return {
      allow: false,
      reason:
        'Blocked direct access to the machine-wide agent-guard lease store: deleting or changing authoritative lease ' +
        `state can admit overlapping runs. Use the arbiter status command for diagnostics. ${GUIDANCE}`,
    };
  }

  if (hasShellStdinProgram(effective)) {
    return {
      allow: false,
      reason: `Blocked a shell program supplied through stdin: its runtime payload cannot bypass guarded command classification. ${USE_ENTRYPOINT}`,
    };
  }

  for (const { pattern, reason } of TAMPERING) {
    if (pattern.test(effective)) return { allow: false, reason };
  }

  const lane = heavyLaneFor(effective);
  if (lane) {
    return {
      allow: false,
      reason:
        `Blocked the "${lane.id}" lane: ${lane.why}, and on a small machine several of these in parallel across repos and ` +
        `agents is what exhausts memory. Agents do not run it locally by default. ${GUIDANCE} ` +
        'If a local run is genuinely required, the owner can run it directly from their own terminal; agent sessions cannot receive forgeable local grants.',
    };
  }

  // A wrapper invocation is the sanctioned path even when the command it wraps
  // matches a blocked pattern — but only for ITS OWN segment. Vouching for the
  // whole line let `echo run-guarded.mjs; node --test …` through, and equally
  // `node run-guarded.mjs -- npm run lint && node --test …`: the sanctioned
  // call is real, and the blocked binary rides along beside it.
  for (const segment of splitSegments(effective)) {
    const executableSegment = commandAfterPrefixes(segment);
    if (ANY_WRAPPER_SEGMENT.test(executableSegment) && !WRAPPER_SEGMENT.test(executableSegment)) {
      return {
        allow: false,
        reason: `Blocked a non-canonical run-guarded.mjs path: only the repository guard may claim the wrapper exemption. ${USE_ENTRYPOINT}`,
      };
    }
    if (WRAPPER_SEGMENT.test(executableSegment)) continue;
    if (incompleteXargsCommand(segment, executableSegment)) {
      return {
        allow: false,
        reason: `Blocked an incomplete package or test command dispatched by xargs: stdin could supply the guarded lane or binary. ${USE_ENTRYPOINT}`,
      };
    }
    if (directVitestNodeEntry(executableSegment)) {
      return {
        allow: false,
        reason: `Blocked direct execution of the Vitest Node entry module: it bypasses the machine-scoped memory guard. ${USE_ENTRYPOINT}`,
      };
    }
    if (directTestBinaryThroughExec(executableSegment)) {
      return {
        allow: false,
        reason: `Blocked direct test-binary invocation through a package-manager exec shim: it bypasses the machine-scoped memory guard. ${USE_ENTRYPOINT}`,
      };
    }
    if (isUnguardedInnerScript(segment)) {
      return {
        allow: false,
        reason: `Blocked unguarded inner package script: it bypasses the machine-scoped memory guard. ${USE_ENTRYPOINT}`,
      };
    }
    for (const { pattern, what, reason } of BLOCKED) {
      if (pattern.test(executableSegment)) {
        return {
          allow: false,
          reason: reason ?? `Blocked ${what}: it bypasses the machine-scoped memory guard. ${USE_ENTRYPOINT}`,
        };
      }
    }
  }
  return { allow: true };
}

// A directory is inside a guarded checkout when a marker exists there or in any
// ancestor — commands routinely run from subdirectories.
function inGuardedCheckout(dir) {
  let current = tryRealpath(dir);
  for (;;) {
    if (GUARD_MARKERS.some((marker) => existsSync(resolve(current, marker)))) return true;
    const parent = dirname(current);
    if (parent === current) return false;
    current = parent;
  }
}

export function evaluateHookInput({ command, cwd }, projectDir, options = {}) {
  const executionDirs = resolveExecutionDirs(cwd, command);
  if (executionDirs.length > 0 && projectDir) {
    const inScope = executionDirs.some((executionDir) => isWithin(executionDir, projectDir) || inGuardedCheckout(executionDir));
    const scopedCommand = maskNonShellHeredocs(command);
    // A path created or replaced earlier in the same shell line cannot be
    // resolved against the pre-execution filesystem. In particular, `ln -s`
    // can make a later out-of-scope `cd` enter this checkout. Keep admission
    // enabled for such compound commands instead of trusting stale realpaths.
    const mayCreateDirectoryAlias = /(?:^|\|\||&&|[;\n|&(){}])\s*(?:(?:command|builtin|env|time|nice|nohup|timeout|setsid|stdbuf|exec)\s+)*(?:\S*\/)?(?:ln|mv|cp|install|mkdir|rm|tar|bsdtar|unzip|cpio)(?=\s|$)/u.test(scopedCommand);
    if (!inScope && !hasDynamicDirectoryTarget(scopedCommand) && !mayCreateDirectoryAlias) return { allow: true };
  }
  return evaluateCommand(command, { ...options, cwd });
}

async function readStdin() {
  const chunks = [];
  for await (const chunk of process.stdin) chunks.push(chunk);
  return Buffer.concat(chunks).toString('utf8');
}

function respond(protocol, verdict) {
  if (protocol === 'cursor') {
    const body = verdict.allow
      ? { permission: 'allow' }
      : {
          permission: 'deny',
          agentMessage: verdict.reason,
          userMessage: `Blocked by the machine memory guard (see ${GUARD_GUIDE}).`,
        };
    process.stdout.write(`${JSON.stringify(body)}\n`);
    return;
  }
  if (!verdict.allow) {
    process.stdout.write(
      `${JSON.stringify({
        hookSpecificOutput: {
          hookEventName: 'PreToolUse',
          permissionDecision: 'deny',
          permissionDecisionReason: verdict.reason,
        },
      })}\n`,
    );
  }
}

async function main() {
  const protocol = process.argv.includes('--protocol=cursor') ? 'cursor' : process.argv.includes('--protocol=codex') ? 'codex' : 'claude';
  // This script lives in the checkout it protects, so its own location is the
  // authoritative project dir (CLAUDE_PROJECT_DIR matches for Claude Code;
  // Cursor and Codex set no equivalent).
  const projectDir = process.env.CLAUDE_PROJECT_DIR ?? dirname(dirname(dirname(fileURLToPath(import.meta.url))));
  let verdict = { allow: true };
  try {
    const input = JSON.parse(await readStdin());
    const command = protocol === 'cursor' ? input.command : normalizeCommand(input.tool_input?.command);
    verdict = evaluateHookInput({ command, cwd: input.cwd }, projectDir);
  } catch {
    // Fail open (see header).
  }
  respond(protocol, verdict);
}

const invokedDirectly = process.argv[1] && tryRealpath(resolve(process.argv[1])) === tryRealpath(fileURLToPath(import.meta.url));
if (invokedDirectly) await main();
