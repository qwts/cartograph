# Local specialist discovery — controlled observation

MT-H4-02 remains **UNEXECUTED as a complete procedure**. One actual-provider
production-module attempt ran and failed its discovery acceptance checks.

The [CI run](https://github.com/qwts/cartograph/actions/runs/34558474540) tested
commit `6068399b18813e067c4cfdb9149f03f75ecdddf9` with Ollama 0.34.0 and Qwen3 8B
Q4_K_M on a four-CPU Linux runner with 16,765,378,560 bytes of RAM. The installed
manifest digest was `500a1f067a9f782620b40bee6f7b0c89e17ae61f686b92c24933e4ca4b2b8b41`.
The runtime used an 8192-token context and had cloud features disabled. The source
was the harness's synthetic captured `rule.ts`; no expected answer entered input.

Domain analyst @1, prompt fingerprint
`ab952c9fe055e1fde7aa5ca3f1d7f60851b402021460fdbd9011a765e20967c3`, returned a valid
`finish` with `insufficient_evidence` and no findings on its first invocation. It
reported that no evidence had been provided instead of asking the host to acquire
context. Recorded counts were one model invocation, zero query/read actions,
zero selected facts, and zero copied evidence. The provider reported 1211 input
tokens and 366 output tokens; active time was 168,213 ms. No request deadline or
host budget was exceeded, and no transport or admission error was recorded.

The coordinator preserved the actual empty result, its ordered events and its
unchanged graph basis through connection reopen. These checks passed, but there
were no source citations to review and the auditor was skipped. Execution status
`completed` describes that persisted finish; it does not establish a successful
investigation. No model task was replayed. The run's
`local-provider-acceptance-6068399b18813e067c4cfdb9149f03f75ecdddf9-1` artifact retains
`setup.json`, `run.json`, the fixture and private coordinator/capture files.

The next revision adds explicit discovery guidance in new @2 specialist
definitions, preserving @1 identities and behavior. This is a change to instructions,
not fabricated query activity or relaxed admission. Its effect requires a separate
actual-provider run on the new source revision. One failed observation is not a
model-wide quality score. Native restart, destructive forgetting, independent
citation review, full H4/H5 interoperability and market gates remain open.

## Second observation — discovery @2, default local thinking

The [next CI run](https://github.com/qwts/cartograph/actions/runs/34559635946)
tested `18073367b65e6bb854f6ae6ce418a81a17d28284` using the same pinned runtime,
model and context length. Domain analyst @2, fingerprint
`9eb83715f67f8b022ec5a144de89c4a4a86e0c3ee86ab71e545677e0fe3e2b37`, reached the
180-second request boundary on its first call without a durable admitted response.
The task `investigation:211bedb5625dabd23f41b0a191d7ec78` remains
`outcome_unknown`, with 180,029 ms active time, one invocation, a 2048-token
reservation and absent provider-reported usage. No query, read or finish was
admitted. It was not replayed. Its preserved journal and unchanged graph passed
the structural checks; the auditor was skipped.

This observation does not validate or invalidate the guidance: there is no model
action to judge. The prior bounded Ollama request omitted `think`, so supported
models inherited the runtime's default thinking behavior. The next transport
revision requests optional thinking off for JSON actions and records its new
protocol identity. This is a hypothesis about a better fit for bounded action
turns, not a demonstrated cause or a completed acceptance result. Both previous
observations remain part of the record.

## Third observation — admitted discovery, host page-sizing failure

[Run 34561178582](https://github.com/qwts/cartograph/actions/runs/34561178582)
tested `bf5aab1d074e5a261f78eb3c869b2e8f16abc445`, specialist @2 and bounded
transport @2 with the same verified Ollama/Qwen3 pins. The runner had four Xeon
8573C CPUs and the same memory size; the CPU model differs from the earlier runs,
so elapsed time alone does not isolate the transport setting's effect.

Investigation `investigation:01bfacec0c625d8e9ebe43e8c83d7e2c` admitted a complete
`query_context` response after 63,183 ms. Reported usage was 1,348 input tokens
and 42 output tokens. The host reserved one tool attempt, then failed on a work
limit at 63,206 ms without publishing a query page. No source read or finding was
produced. The coordinator retained the failure; graph equality, ordered events
and connection reopen checks passed. The auditor was skipped.

The saved canonical action hash
`ade0d2af2844cbb882a148adfe1dcbae42451f1dfbfcc585a888cac630a881e1` matches the
documented example exactly: all scope, no kind/label filter, 12 facts, 16,384 bytes,
no cursor. A read-only diagnostic using the real retained graph and the production
context query returned 12 facts in 12,637 bytes. Adding the actual receipt and
legacy provenance inventories produces 19,888 bytes. The host sized only the core
page before adding its envelope and inventories, then rejected the complete page.

The correction makes the complete response determine pagination and retains only
the returned prefix in the ledger. It preserves every existing limit and fails
explicitly when one fact plus its inventory cannot fit. This fixes an observed
host defect; successful source investigation still requires a separate actual
provider run. The failed task is not replayed.

## Fourth observation — discovery delivered, second call unknown

[Run 34562301078](https://github.com/qwts/cartograph/actions/runs/34562301078)
tested `f0a6d5477cf7ce9c4f758f683a1771adf007b1ab` with the same runtime/model
pins on four AMD EPYC 9V45 CPU cores and 16,766,414,848 bytes of memory.
Investigation `investigation:5eb33775f867e99ab0ec08984a930638` admitted the same
documented query after 53,050 ms (1,352 reported input tokens, 42 output tokens).
The host delivered eight of twelve facts in 15,533 bytes with a continuation
cursor. The saved ledger and receipt index contain that returned prefix. This
actual-provider observation confirms the page-sizing correction.

The next call reached its 180-second request boundary without a durable admitted
response. The first and second serialized input payloads were 5,923 and 29,360
bytes respectively. The task ended `outcome_unknown` at 233,183 ms of active time,
with two invocations, 4,096 reserved generated tokens and unknown aggregate
provider-reported usage. The first call's measured usage remains in its response
record. No source read or finding was produced; the auditor was skipped. The
ordered journal, unchanged graph and coordinator reopen checks passed.

The larger request and timeout establish a runtime acceptance problem, but do not
identify whether prompt evaluation, generation or context fit caused it; the
second call returned no timing or token counts. This run does not establish model
quality or successful investigation. No replay or budget increase was performed.
Validation on a suitable development runtime and independent citation review are
still required. The native pagination regression and the ordinary code test
steps passed; the explicitly selected actual-provider acceptance failed.
