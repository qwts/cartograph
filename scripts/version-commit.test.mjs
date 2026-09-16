import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import test from "node:test";

import { entriesFromGit, publishSignedCommit } from "./version-commit.mjs";

const BASE = "a".repeat(40);

function fakeGitHub({ refExists = true } = {}) {
  const calls = [];
  const fetchImpl = async (url, init) => {
    const route = url.replace("https://api.github.com/", "");
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({
      method: init.method,
      route,
      body,
      auth: init.headers.authorization,
    });
    const respond = (status, payload) => ({
      ok: status < 400,
      status,
      json: async () => payload,
    });
    if (route === `repos/qwts/cartograph/git/commits/${BASE}`)
      return respond(200, { tree: { sha: "basetree" } });
    if (route === "repos/qwts/cartograph/git/blobs")
      return respond(201, { sha: `blob-${calls.length}` });
    if (route === "repos/qwts/cartograph/git/trees")
      return respond(201, { sha: "newtree" });
    if (route === "repos/qwts/cartograph/git/commits") {
      return respond(201, {
        sha: "b".repeat(40),
        verification: { verified: true, reason: "valid" },
      });
    }
    if (
      route === "repos/qwts/cartograph/git/refs/heads/changeset-release/main"
    ) {
      return refExists
        ? respond(200, {})
        : respond(422, { message: "Reference does not exist" });
    }
    if (route === "repos/qwts/cartograph/git/refs") return respond(201, {});
    return respond(404, { message: `unexpected ${route}` });
  };
  return { calls, fetchImpl };
}

const entries = [
  {
    path: "package.json",
    mode: "100644",
    content: Buffer.from('{"version":"0.15.0"}\n'),
  },
  { path: ".changeset/old-note.md", mode: "100644", content: null },
];

test("publishes blobs, a tree on the base commit, a single-parent commit, and force-updates the branch", async () => {
  const { calls, fetchImpl } = fakeGitHub();
  const result = await publishSignedCommit({
    repo: "qwts/cartograph",
    token: "ghs_test",
    branch: "changeset-release/main",
    base: BASE,
    message: "Version packages",
    entries,
    fetchImpl,
  });

  assert.deepEqual(result, { sha: "b".repeat(40), verified: true, files: 2 });
  assert.ok(calls.every((call) => call.auth === "Bearer ghs_test"));

  const blob = calls.find(
    (call) => call.route === "repos/qwts/cartograph/git/blobs",
  );
  assert.deepEqual(blob.body, {
    content: Buffer.from('{"version":"0.15.0"}\n').toString("base64"),
    encoding: "base64",
  });

  const tree = calls.find(
    (call) => call.route === "repos/qwts/cartograph/git/trees",
  );
  assert.equal(tree.body.base_tree, "basetree");
  assert.deepEqual(tree.body.tree, [
    { path: "package.json", mode: "100644", type: "blob", sha: "blob-2" },
    { path: ".changeset/old-note.md", mode: "100644", type: "blob", sha: null },
  ]);

  const commit = calls.find(
    (call) =>
      call.method === "POST" &&
      call.route === "repos/qwts/cartograph/git/commits",
  );
  assert.deepEqual(commit.body, {
    message: "Version packages",
    tree: "newtree",
    parents: [BASE],
  });

  const ref = calls.at(-1);
  assert.equal(ref.method, "PATCH");
  assert.equal(
    ref.route,
    "repos/qwts/cartograph/git/refs/heads/changeset-release/main",
  );
  assert.deepEqual(ref.body, { sha: "b".repeat(40), force: true });
});

test("creates the branch when it does not exist yet", async () => {
  const { calls, fetchImpl } = fakeGitHub({ refExists: false });
  await publishSignedCommit({
    repo: "qwts/cartograph",
    token: "ghs_test",
    branch: "changeset-release/main",
    base: BASE,
    message: "Version packages",
    entries,
    fetchImpl,
  });
  const created = calls.at(-1);
  assert.equal(created.method, "POST");
  assert.equal(created.route, "repos/qwts/cartograph/git/refs");
  assert.deepEqual(created.body, {
    ref: "refs/heads/changeset-release/main",
    sha: "b".repeat(40),
  });
});

test("rejects malformed inputs before touching the API", async () => {
  const { calls, fetchImpl } = fakeGitHub();
  const publish = (overrides) =>
    publishSignedCommit({
      repo: "qwts/cartograph",
      token: "ghs_test",
      branch: "changeset-release/main",
      base: BASE,
      message: "Version packages",
      entries,
      fetchImpl,
      ...overrides,
    });
  await assert.rejects(publish({ base: "main" }), /full commit sha/u);
  await assert.rejects(publish({ branch: "refs/heads/x" }), /bare name/u);
  await assert.rejects(publish({ entries: [] }), /Nothing to commit/u);
  await assert.rejects(publish({ message: " " }), /message is required/u);
  assert.equal(calls.length, 0);
});

test("surfaces API failures with the route and GitHub message", async () => {
  const fetchImpl = async () => ({
    ok: false,
    status: 403,
    json: async () => ({ message: "Resource not accessible" }),
  });
  await assert.rejects(
    publishSignedCommit({
      repo: "qwts/cartograph",
      token: "ghs_test",
      branch: "changeset-release/main",
      base: BASE,
      message: "Version packages",
      entries,
      fetchImpl,
    }),
    /GET repos\/qwts\/cartograph\/git\/commits\/a{40} -> 403: Resource not accessible/u,
  );
});

test("entriesFromGit reports adds, edits, and deletes from a local commit range", () => {
  const root = mkdtempSync(
    path.join(os.tmpdir(), "cartograph-version-commit-"),
  );
  const run = (...args) =>
    execFileSync("git", args, {
      cwd: root,
      encoding: "utf8",
      env: {
        ...process.env,
        GIT_AUTHOR_NAME: "t",
        GIT_AUTHOR_EMAIL: "t@example",
        GIT_COMMITTER_NAME: "t",
        GIT_COMMITTER_EMAIL: "t@example",
      },
    });
  try {
    run("init", "-q", "-b", "main");
    writeFileSync(path.join(root, "package.json"), '{"version":"0.14.0"}\n');
    writeFileSync(path.join(root, "note.md"), "pending\n");
    run("add", ".");
    run("-c", "commit.gpgsign=false", "commit", "-q", "-m", "base");
    const base = run("rev-parse", "HEAD").trim();
    writeFileSync(path.join(root, "package.json"), '{"version":"0.15.0"}\n');
    writeFileSync(path.join(root, "CHANGELOG.md"), "## 0.15.0\n");
    rmSync(path.join(root, "note.md"));
    run("add", "-A");
    run("-c", "commit.gpgsign=false", "commit", "-q", "-m", "Version packages");

    const found = entriesFromGit({ base, cwd: root }).map((entry) => ({
      path: entry.path,
      mode: entry.mode,
      content: entry.content === null ? null : entry.content.toString("utf8"),
    }));
    assert.deepEqual(found, [
      { path: "CHANGELOG.md", mode: "100644", content: "## 0.15.0\n" },
      { path: "note.md", mode: "100644", content: null },
      {
        path: "package.json",
        mode: "100644",
        content: '{"version":"0.15.0"}\n',
      },
    ]);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
