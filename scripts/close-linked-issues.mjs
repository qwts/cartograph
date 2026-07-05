#!/usr/bin/env node

import { readFile } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const CLOSING_REFERENCE_RE =
  /\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\b\s+(?<references>(?:(?:[a-z0-9_.-]+\/[a-z0-9_.-]+)?#\d+)(?:\s*(?:,|and)\s*(?:(?:[a-z0-9_.-]+\/[a-z0-9_.-]+)?#\d+))*)/giu;
const ISSUE_REFERENCE_RE = /(?:(?<owner>[a-z0-9_.-]+)\/(?<repo>[a-z0-9_.-]+))?#(?<number>\d+)/giu;

// Must match the `branches:`/`base.ref` filter in .github/workflows/close-linked-issues.yml.
export const TARGET_BASE_REFS = ['main'];

export function shouldProcessMergedPullRequest(pullRequest) {
  return pullRequest?.merged === true && TARGET_BASE_REFS.includes(pullRequest.base?.ref);
}

export function extractClosingIssueNumbers(body, repository) {
  const issues = [];
  const seen = new Set();
  for (const keywordMatch of body.matchAll(CLOSING_REFERENCE_RE)) {
    const references = keywordMatch.groups?.references ?? '';
    for (const referenceMatch of references.matchAll(ISSUE_REFERENCE_RE)) {
      const number = Number(referenceMatch.groups?.number);
      if (!Number.isSafeInteger(number) || number <= 0) continue;
      const owner = referenceMatch.groups?.owner;
      const repo = referenceMatch.groups?.repo;
      if (owner && repo && `${owner}/${repo}`.toLowerCase() !== repository.toLowerCase()) continue;
      if (seen.has(number)) continue;
      seen.add(number);
      issues.push(number);
    }
  }
  return issues;
}

export function removeWipPrefix(title) {
  return title.replace(/^\[WIP\]\s*/iu, '');
}

export function closeCommentForPullRequest(prNumber, baseRef) {
  return `Closed by merged PR #${prNumber} into ${baseRef}.`;
}

function gh(args, options = {}) {
  const output = execFileSync('gh', args, {
    encoding: 'utf8',
    stdio: options.stdio ?? ['ignore', 'pipe', 'inherit'],
  });
  return typeof output === 'string' ? output.trim() : '';
}

async function eventPayload() {
  const eventPath = process.env.GITHUB_EVENT_PATH;
  if (!eventPath) throw new Error('GITHUB_EVENT_PATH is required.');
  return JSON.parse(await readFile(eventPath, 'utf8'));
}

async function main() {
  const event = await eventPayload();
  const pullRequest = event.pull_request;
  const repository = process.env.GITHUB_REPOSITORY;
  if (!repository) throw new Error('GITHUB_REPOSITORY is required.');
  if (!shouldProcessMergedPullRequest(pullRequest)) {
    console.log('No issue close-out required for this pull request event.');
    return;
  }

  const issueNumbers = extractClosingIssueNumbers(pullRequest.body ?? '', repository);
  if (issueNumbers.length === 0) {
    console.log(`No closing issue references found in PR #${pullRequest.number}.`);
    return;
  }

  for (const issueNumber of issueNumbers) {
    const issue = JSON.parse(gh(['issue', 'view', String(issueNumber), '--repo', repository, '--json', 'state,title']));
    const title = removeWipPrefix(issue.title);
    if (title !== issue.title) {
      gh(['issue', 'edit', String(issueNumber), '--repo', repository, '--title', title], { stdio: 'inherit' });
    }
    if (issue.state === 'CLOSED') {
      console.log(`Issue #${issueNumber} is already closed.`);
      continue;
    }
    gh(
      [
        'issue',
        'close',
        String(issueNumber),
        '--repo',
        repository,
        '--comment',
        closeCommentForPullRequest(pullRequest.number, pullRequest.base.ref),
      ],
      { stdio: 'inherit' },
    );
  }
}

if (import.meta.url === pathToFileURL(process.argv[1]).href) {
  main().catch((error) => {
    console.error(error);
    process.exitCode = 1;
  });
}
