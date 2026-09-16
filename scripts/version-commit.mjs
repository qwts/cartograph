#!/usr/bin/env node

// Writes the Version packages commit through the GitHub git data API instead
// of pushing a runner-authored commit. Commits created this way with an App
// installation token are signed by GitHub and report as verified, which the
// default-branch ruleset (required_signatures) demands before the reviewed
// Version packages PR can merge.

import { execFileSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";

const SCRIPT_PATH = fileURLToPath(import.meta.url);
const API_BASE = "https://api.github.com";
const BLOB_MODES = new Set(["100644", "100755", "120000"]);

function git(args, cwd, encoding = "utf8") {
  return execFileSync("git", args, {
    cwd,
    encoding,
    maxBuffer: 256 * 1024 * 1024,
  });
}

// Lists the files a local commit range changed, with the content and mode the
// head commit records for each. Renames are reported as a delete plus an add.
export function entriesFromGit({ base, head = "HEAD", cwd = process.cwd() }) {
  const raw = git(
    ["diff", "--name-status", "--no-renames", "-z", base, head],
    cwd,
  );
  const fields = raw.split("\0").filter(Boolean);
  const entries = [];
  for (let index = 0; index < fields.length; index += 2) {
    const status = fields[index];
    const filePath = fields[index + 1];
    if (filePath === undefined)
      throw new Error(`git diff produced a status without a path: ${status}`);
    if (status.startsWith("D")) {
      entries.push({ path: filePath, mode: "100644", content: null });
      continue;
    }
    const meta = git(["ls-tree", head, "--", filePath], cwd).trim();
    if (!meta) throw new Error(`could not read ${filePath} from ${head}`);
    const [mode, type] = meta.split(/\s+/u);
    if (type !== "blob" || !BLOB_MODES.has(mode)) {
      throw new Error(
        `${filePath} is a ${type} with mode ${mode}; only regular blobs can be published`,
      );
    }
    entries.push({
      path: filePath,
      mode,
      content: git(["show", `${head}:${filePath}`], cwd, "buffer"),
    });
  }
  return entries;
}

function makeApi({ token, fetchImpl }) {
  return async (route, method = "GET", body) => {
    const response = await fetchImpl(
      `${API_BASE}/${route.replace(/^\/+/u, "")}`,
      {
        method,
        headers: {
          accept: "application/vnd.github+json",
          authorization: `Bearer ${token}`,
          "content-type": "application/json",
          "user-agent": "cartograph-version-cut",
          "x-github-api-version": "2022-11-28",
        },
        body: body === undefined ? undefined : JSON.stringify(body),
      },
    );
    const result = await response.json().catch(() => ({}));
    if (!response.ok) {
      const error = new Error(
        `${method} ${route} -> ${response.status}: ${result.message ?? "unknown error"}`,
      );
      error.status = response.status;
      throw error;
    }
    return result;
  };
}

// Creates one commit on top of `base` containing `entries`, then points
// `branch` at it (force, since the version branch is regenerated on every push
// to main). Returns the new commit's sha and verification state.
export async function publishSignedCommit({
  repo,
  token,
  branch,
  base,
  message,
  entries,
  fetchImpl = fetch,
}) {
  if (!/^[^/\s]+\/[^/\s]+$/u.test(repo))
    throw new Error(`Invalid repository: ${repo}`);
  if (!/^[0-9a-f]{40}$/u.test(base))
    throw new Error(`Base must be a full commit sha: ${base}`);
  if (!branch || branch.startsWith("refs/"))
    throw new Error(`Branch must be a bare name: ${branch}`);
  if (!message?.trim()) throw new Error("Commit message is required");
  if (!Array.isArray(entries) || entries.length === 0)
    throw new Error("Nothing to commit");

  const api = makeApi({ token, fetchImpl });
  const baseCommit = await api(`repos/${repo}/git/commits/${base}`);

  const tree = [];
  for (const entry of entries) {
    if (entry.content === null) {
      tree.push({
        path: entry.path,
        mode: entry.mode,
        type: "blob",
        sha: null,
      });
      continue;
    }
    const blob = await api(`repos/${repo}/git/blobs`, "POST", {
      content: Buffer.from(entry.content).toString("base64"),
      encoding: "base64",
    });
    tree.push({
      path: entry.path,
      mode: entry.mode,
      type: "blob",
      sha: blob.sha,
    });
  }

  const newTree = await api(`repos/${repo}/git/trees`, "POST", {
    base_tree: baseCommit.tree.sha,
    tree,
  });
  const commit = await api(`repos/${repo}/git/commits`, "POST", {
    message,
    tree: newTree.sha,
    parents: [base],
  });

  try {
    await api(`repos/${repo}/git/refs/heads/${branch}`, "PATCH", {
      sha: commit.sha,
      force: true,
    });
  } catch (error) {
    if (error.status !== 404 && error.status !== 422) throw error;
    await api(`repos/${repo}/git/refs`, "POST", {
      ref: `refs/heads/${branch}`,
      sha: commit.sha,
    });
  }

  return {
    sha: commit.sha,
    verified: commit.verification?.verified === true,
    files: tree.length,
  };
}

function parseArgs(argv) {
  const options = {};
  for (let index = 0; index < argv.length; index += 1) {
    const arg = argv[index];
    if (!arg.startsWith("--")) throw new Error(`Unexpected argument: ${arg}`);
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--"))
      throw new Error(`Missing value for ${arg}`);
    options[arg.slice(2)] = value;
    index += 1;
  }
  return options;
}

async function runCli() {
  const [command, ...rest] = process.argv.slice(2);
  if (command !== "publish") {
    console.error(
      "Usage: version-commit.mjs publish --repo owner/name --branch name --base sha --message text",
    );
    process.exit(2);
  }
  const options = parseArgs(rest);
  const token = process.env.GH_TOKEN;
  if (!token) throw new Error("GH_TOKEN is required");
  const base = git([
    "rev-parse",
    "--verify",
    `${options.base}^{commit}`,
  ]).trim();
  const entries = entriesFromGit({ base });
  const result = await publishSignedCommit({
    repo: options.repo,
    token,
    branch: options.branch,
    base,
    message: options.message,
    entries,
  });
  if (!result.verified) {
    throw new Error(
      `${result.sha} was created but GitHub did not report it as verified`,
    );
  }
  console.log(
    `${options.branch} -> ${result.sha} (verified, ${result.files} files)`,
  );
}

if (process.argv[1] && path.resolve(process.argv[1]) === SCRIPT_PATH) {
  runCli().catch((error) => {
    console.error(error.message);
    process.exit(1);
  });
}
