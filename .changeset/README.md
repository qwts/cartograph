# Changesets

Changesets record release intent alongside the change that creates it.

Run `npm run changeset`, select `cartograph`, choose the required SemVer bump,
and write a user-facing summary. Behavior/features and breaking changes use a
minor bump while Cartograph is on `0.x`; fixes use patch. Docs, tests, and
internal tooling with no shipped behavior change may omit a changeset.

Do not edit `CHANGELOG.md` or any version field by hand. The version-cut
automation consumes these files into the reviewed **Version packages** PR.
