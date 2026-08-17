import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const config = await readFile(
  new URL("../.github/ISSUE_TEMPLATE/config.yml", import.meta.url),
  "utf8",
);

test("issue chooser keeps blank issues disabled", () => {
  assert.match(config, /^blank_issues_enabled: false$/m);
});

test("question contact link opens a pre-labeled issue", () => {
  assert.match(config, /^  - name: Question \/ discussion$/m);

  const url = config.match(/^    url: (.+)$/m)?.[1];
  assert.ok(url, "question contact URL is configured");

  const issueRoute = new URL(url);
  assert.equal(issueRoute.origin, "https://github.com");
  assert.equal(issueRoute.pathname, "/qwts/cartograph/issues/new");
  assert.deepEqual(issueRoute.searchParams.getAll("labels"), ["question"]);
});
