# Specialist guard fixture — independent review oracle

For MT-H4-02 only. Keep this file outside all model inputs. The acceptance harness
captures only its embedded `rule.ts`; it does not ingest this repository or read
this oracle. Reviewers compare the retained fixture hash and exact cited ranges
before judging generated findings.

The fixture exports `enabled(ok: boolean)`. Its local `limited` value is true
exactly when the argument is the Boolean value false. When that guard is true,
the function returns false. Otherwise it falls through without an explicit return,
so ordinary JavaScript execution returns undefined. The annotation alone does not
enforce runtime argument types. The source establishes no authorization policy,
caller contract, larger feature behavior, or rationale for the design.

A model may report less when it inspected only a narrow span. The local initializer
alone supports the comparison but not the containing function's full return
behavior. A guard span alone does not establish how the local value was computed.
Credit only claims supported by the actual ledger and supplied source occurrences;
absence of a claim is not a failure when coverage was explicitly narrower.

Review each finding for supported behavior, invented intent, omitted qualifications,
correct original citations and explicit scope limits. Check the auditor's claims
against its own evidence and the parent's saved status and uncertainty. Agreement
between two models is not independent validation. Record reviewer identity, task
and result IDs, unsupported claims, omissions and the final disposition on #404.
