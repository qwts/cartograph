# SPEC-10 — Durable specialist investigations

Status: implementation contract for #404, child of #256/#255. No implementation
or delivery gate is claimed by this document. Extends SPEC-01 and reuses the
source and execution boundaries in SPEC-03 through SPEC-09.
Decision: [ADR-0028](adr/ADR-0028-specialist-investigation-coordinator.md).
Trace: US-0032 / AC-0180–0191 / T-0180–0191.

## User outcome and scope

A developer asks a scoped question, watches a named specialist query recovered
context and read evidence, and retrieves cited findings and the task history.
The same host coordinator owns application actions and future transport adapters.
This is an actual bounded tool loop, not an edge-resolution task disguised as chat.

Ship domain-analyst@2 (Domain analyst) and evidence-auditor@2 (Evidence auditor).
Both are T3, have an InferredWeak ceiling, and share the same enforced tool set.
Their immutable definitions include role/prompt version, prompt fingerprint,
purpose, supported operations and limits. Persist the exact definition and
configured provider/model identity used by each task, plus separately observed
response identity when available. A model tag is not a weight-content attestation.
Do not advertise T0/T1/T2 agent reasoning or implemented architecture metrics.

Version 2 clarifies the host-mediated discovery loop: the first invocation has no
query pages or copied source because acquisition has not started. The specialist
should return a query action, inspect its results and request relevant original
evidence before concluding that support is insufficient. Empty initial input is
not an empty search result. The host still admits a valid insufficient-evidence
finish without inventing tool activity or retrying it; such a run does not satisfy
the controlled query/read/finish acceptance case. The original @1 definitions stay
byte-for-byte reproducible and supported for saved history and explicit old-version
requests. The catalog and new app requests select @2; changing a default never
reinterprets an existing task's definition or prompt fingerprint.

The initial scope is the app's recovered system, optionally restricted to a known
fact neighborhood. It is not a named multi-project/domain isolation implementation.
A task records its host context owner and exact normalized scope; source identities
remain the registered host identities. No caller root or filesystem path grants
access. Domain names must come from evidence or remain labeled interpretations.

Keep implemented-behavior claims, documented-intent claims, inferred
interpretations and proposed future designs distinguishable. Every model-generated
finding remains T3/InferredWeak irrespective of its claim kind or human response.
Missing design/domain/feature evidence means unknown within the searched scope,
not proof that no such design/domain/feature exists. Findings do not mutate the
recovered graph, edge-proposal history, accepted overlays, exports or curated state.

AC-0110 and MT-H4-01 remain unchanged, including their real MCP ingress requirement.
This application/coordinator increment does not pass H4 by itself. Full H2/H3,
MCP/ACP execution and delegation, H6 measurements and H7 pilot remain open.

## Contracts and execution boundaries

Add distinct versioned investigation request, action, input-ledger and result
contracts in agents. Preserve AgentTask, AgentProposal, PreparedAgentTask and
literal staging-v1/v2 identities. Never fabricate a Gap, source/target relation or
confirmed assertion to carry a question or answer.

A request includes a bounded client request nonce, conversation identity (or new
conversation), specialist definition ID, question, normalized scope, provider mode,
limit profile, optional expected graph revision, origin and optional parent/follow-up
identity. The context owner is the existing private state-store execution namespace,
recorded in host metadata and exposed only through an opaque host-derived context
identity; it is not a caller-supplied domain name. The webview cannot select another
private store or supply execution ownership. The first transport origin is app;
reserve typed transport extension points rather than pretending MCP is connected.

Start validates cheap client syntax, then checks the durable origin/nonce and
client-intent hash before mutable availability/capacity checks or allocating a new
conversation. An identical retry returns its original task even if a provider has
become unavailable or capacity is full. Only a new task resolves and freezes host
definitions/provider configuration and commits identity before expensive
graph/source work or model completion. Keep the client-intent hash separate from
the immutable host-resolved request hash. A context revision is
explicitly pending until preparation commits the frozen input. If the caller
supplied an expected revision, preparation must match it or fail. Otherwise the
request explicitly means the current recovered context at preparation time.
The UI shows the actual prepared revision and scope before findings are presented.

The coordinator exposes start, task/status, conversation/history, ordered events,
result, pending-consent, approve-step, cancel and historical-citation reads. These
are transport-independent host operations wrapped by Tauri. Jobs are the execution
lifecycle surface; investigations own questions, definitions, input/result identity,
ordered events, consent state and findings. A live map holds ephemeral execution
payloads and wakeups only; SQLite is the identity/lifecycle authority.

The same request nonce returns the same task for concurrent or repeated identical
starts, including a lost start response. Reusing it for different request material
fails. Client retry of a transport observation never creates a new task. A follow-up
is a separate explicitly requested task and basis; previous findings supplied as
history remain T3 and retain their original revision/coverage labels.

## Frozen scope and coherent acquisition

Copy one complete graph snapshot on a worker. Build ContextSnapshot once from that
owned graph. A restricted neighborhood derives a frozen scope projection and its
own query identity, while the ledger also retains the original full graph identity.
Tool queries use this frozen projection, not repeated calls to the live
query_context Tauri command. Anchors outside the authorized scope fail. Preserve
paging, explicit selected/returned counts and exact typed fact identities.

Maintain a cumulative ledger of at most 64 distinct selected facts. Before a new
query result or evidence selection can enter a model payload, obtain one coherent
read_source_selection_snapshot for the union of prior and newly selected keys.
Compare its complete graph to the original owned graph, and every prior fact digest
and association to the bound ledger. Acquire sorted source/retention guards outside
application/database mutexes, then authoritatively recheck the same bounded union
before supplying newly copied source. Prior associations cannot be silently rebound.
Unvisited unrelated associations do not invalidate the ledger. Global graph changes,
visited invalid selections and changed prior associations stop acquisition explicitly.

A query page is admitted as a whole or fails its bound; do not silently skip a large
fact in favor of a later fact. Record exact fact digests, present/absent source
associations, query identity and the returned scope/count/continuation information. Query payloads also expose
bounded evidence options with each original role/index and source reference so a
specialist can discover nested occurrences rather than guess them. The complete
context plus inventory envelope must fit the query page byte limit. Source receipt
metadata does not claim that retained bytes are currently available.
Whole-graph copy/comparison is not constant-cost and is separate from the query
response budget. A bounded coherent reader enforces the graph row/byte ceilings,
SQL type and raw-body byte preflight before loading bodies, bounded JSON depth/value
counts and exact bounded canonical serialization in one read transaction. Every
union recheck uses that same bounded reader, so a later oversized live graph cannot
allocate without bounds before its revision is compared. These ceilings limit
admitted structure, not an exact resident-memory promise. Work runs off the UI
thread; even narrowed task scopes pay the full bounded snapshot cost.

The read_evidence action names an already admitted typed fact, receipt range role
and occurrence index. It supplies no path, replacement offsets or arbitrary receipt.
For participating facts, validate the exact selected receipt and original inventory
occurrence and read the strict retained span. Reuse source ownership and capture
validation from SPEC-09, including nested v2 local-definition occurrences. Check
8-KiB original-span limits before reading; no synthetic excerpts. Known missing,
forgotten, corrupt or unsupported participating evidence fails without checkout
fallback. Valid association absence can use the existing bounded legacy reader,
with its explicit working_tree_unverified classification and original cited range.
Legacy reads cannot manufacture retained historical source.

Charge each attempted read and the full captured file length, including repeated
reads of one file. Commit each admitted input-ledger revision and its actual copied
receipt-reference index while acquisition/retention guards remain held; pending
inputs, including uncited source, must affect a concurrent retention preview.
Because selected receipt identities enter the model ledger even before a source
read, the ledger explicitly records registered source/repo/receipt references for
every selected present binding; the retention index includes these too.
Source and retention leases end before every model call or consent wait. Retained bytes may subsequently be forgotten. A finished model
response is admitted against the last owned input ledger, never against a fresh
source read; completed history survives later reingest/forgetting/cancellation.

## Actual bounded tool loop

A model returns exactly one strict JSON action per invocation:

- query_context: a validated bounded query over the frozen authorized scope;
- read_evidence: an admitted fact and original role/index;
- finish: typed findings, citations and limitations.

Reject unknown fields/actions, excess depth/count/string sizes, trailing data,
markdown wrappers and invented references. No execution of paths, commands, URLs,
repository instructions, arbitrary tool names or graph mutations. No delegation
in this contract. This host-interpreted protocol does not claim native provider
function calling or ACP support.

Persist the admitted action identity and safe event metadata before executing its
tool. Tool results are input data, not instructions or permission grants. Every
model step sees the question, role, explicit coverage/budget ledger, admitted
context and evidence, and any bounded prior conversation findings marked as T3.
Keep the full raw tool transcript and copied source transient. There is no hidden
repair/retry model call when JSON is invalid or a provider fails.

Every finding has a stable result-local identity, claim kind, bounded title and
statement, one or more admitted citation IDs, explicit limitations and host-stamped
Agentic/InferredWeak provenance. Citation IDs resolve to original ledger entries;
equal paths, overlapping spans or later facts cannot substitute for them. Reject
invented or unavailable-to-the-model citations. A finish with no supported findings
may explicitly report insufficient evidence; it cannot fabricate a complete answer.
Execution completion and knowledge completeness are independent fields.

Apply source-redaction/replay admission to model-authored prose against every
supplied source/context string, including uncited items and supplied prior findings.
Visit individual graph string values/keys and copied source items, not only the
outer serialized JSON. Raw source excerpts and supplied prior-finding text retain
the existing 48 non-whitespace scalar window plus complete-short-item rejection.
For the explicit parent-history envelope, inspect individual saved finding titles,
statements and limitations inside its JSON; a serialized wrapper must not weaken
short-item protection. Preserve the parent status and revision as metadata.
Graph metadata uses the 48-scalar window without complete-short-scalar rejection:
short identifiers/literals such as a symbol named a may appear in ordinary prose.
Otherwise one character could prohibit almost every English answer. Secret
redaction still applies to all output; this permits short graph metadata, not raw
source-excerpt copying, and is not semantic or encoded-text declassification.
Required typed
citation/reference fields instead undergo exact membership/shape validation: those
fields must reproduce admitted IDs and are not model prose. No prose field becomes
exempt merely by containing a reference. Validate before durable writes;
never archive unvalidated model responses or private thinking text. Persist accepted
findings, typed references, immutable hashes and bounded safe metadata. User question
and follow-up text has an explicit bounded/redacted history representation; an
original input fingerprint may bind identity without storing the original secret.

## Budgets and provider acquisition

Initial hard ceiling profile (the host may narrow it, never silently enlarge it):

| Work | Ceiling |
|---|---:|
| Model invocations | 8 per task |
| Host tool actions | 8 per task |
| Distinct selected fact keys | 64 per task |
| Query page | 32 facts / 32 KiB serialized |
| Cumulative returned query data, including repeats | 96 KiB |
| Evidence requests | 64 per task |
| Copied evidence items | 12 per task |
| Original span / combined copied source | 8 KiB / 48 KiB |
| Captured full-file validation, including repeats | 128 MiB |
| Question | 2 KiB UTF-8 |
| Supplied prior finding history | 16 KiB |
| Logical completion input before redaction | 128 KiB |
| Serialized request JSON body / complete response body | 512 KiB / 256 KiB |
| Returned model action text | 32 KiB |
| Requested generated tokens | 2,048 per invocation / 16,384 reservations per task |
| Connection / entire request-response timeout | 10 s / 180 s |
| Cumulative active execution time | 600 s |
| One consent wait / total task wall time | 900 s / 3,600 s |
| Finished findings | 12, each with at most 8 citations |
| Serialized admitted result / input manifest | 64 KiB / 128 KiB |
| One durable event / task event count | 16 KiB / 96 |
| One stored row/body (not aggregate task) | 256 KiB |
| History/event page | 50 items / 256 KiB |
| One history summary | 4 KiB |
| Nonterminal tasks per state.db | 2 |
| Retained tasks / conversations per state.db | 128 / 128 |
| Logical reservation per admitted task / total coordinator capacity | 2 MiB / 256 MiB |
| Reserved terminal publication headroom within each task | 256 KiB |
| Frozen canonical graph / combined node and edge rows | 32 MiB / 50,000 |
| Graph JSON depth / cumulative JSON values | 32 / 1,000,000 |

Bounds are independent. Enforce task/conversation counts and the aggregate 2-MiB
per-task reservation in the same IMMEDIATE start transaction. Queued, consent-waiting,
cancellation-pending and failed ownership observations continue to consume one of
the two nonterminal slots; an in-process semaphore alone is insufficient. A duplicate
start returns before capacity rejection. Do not evict history automatically. The
256-MiB cap applies to coordinator logical data/reservations, not all shared SQLite
pages, WAL bytes, captured source, graph storage or filesystem overhead. Ordinary
journal growth cannot spend the reserved terminal 256 KiB; reserve the actual input
manifest/result/event/metadata sizes before publication. A budget failure must
remain recordable. The 256-KiB row/body bound is not an aggregate task budget. A tool result that cannot fit
the next bounded input fails explicitly; no silent source omission or context
replacement. Every invocation begun consumes its invocation and generated-token
reservation even on timeout, cancellation, malformed response or provider failure.
Record provider-reported usage separately, with unavailable values remaining absent.
Input byte limits and requested output-token caps are not a verified monetary cap
or a claim of exact token accounting. Never relabel estimates as measured usage.

Active time includes acquisition, validation and calls; explicit human consent
waiting is separate and bounded. Waiting cannot reset any spent budget. Check
ownership, cancellation and remaining budgets before each new operation and after
bounded work. Apply the minimum of call cap and remaining active/wall deadline to
the actual HTTP request. Time limits bound awaited local work; they do not claim
remote inference or operating-system resolver work has been killed. Long existing
graph/OS work is cooperative; report timeout without claiming preemption.

Add a separate bounded completion/firewall API. Legacy completion methods, payloads
and preview hashes remain compatible; the new method must not default to the old
unbounded complete implementation. Bound input before redaction, request
serialization before growing its buffer, and actual HTTP bytes before JSON decode,
including chunked/no-Content-Length and oversized ignored provider fields. Body
counters exclude HTTP headers, framing and TLS overhead. Disable automatic content
decoding/compression and reject unsupported Content-Encoding. Disable redirects
and transport retries for bounded calls. Use explicit generation limits
and accept only supported complete stop conditions; truncation/refusal/unknown
states are not usable protocol actions. Provider errors are fixed and source-free.
Keep configured model, observed response model and request identity distinct.

Verify HTTPS support explicitly: the current reqwest configuration lacks a TLS
backend. Enable a supported TLS feature and retain certificate/hostname validation.
Use controlled HTTPS fixtures as well as HTTP fixtures. Reuse application-lifetime
clients; do not create/drop a blocking client runtime for each limited invocation.
Keep Ollama local/no-proxy restrictions. Do not silently change provider or model.

## Per-step consent and cancellation

Each bounded preview has a separate versioned hash domain binding provider/model
profile, exact redacted payload, action/step identity, protocol and declared limits.
Existing ConsentGrant can carry that hash; a legacy grant cannot authorize it.
Each tool result changes the transcript and therefore the next cloud action.
A consent wait stores only safe identity/hash/limit metadata durably; the exact
preview/input stays with the live worker. Approval must match task revision, step,
provider and payload, and is consumed atomically with invocation start and budget
reservation. Old, reused, changed-provider or changed-payload grants perform no call.
Declining a step does not switch to a local provider. Hiding the dialog is not consent.

Cancellation is a durable request separate from execution/result state. Acknowledge
it promptly and wake a consent wait. Jobs-originated cancellation uses this same
coordinator transition, rather than changing only the Job row. Check cancellation
before every new tool or invocation.
The bounded synchronous provider may finish an already issued request or time out;
the UI must state that its outcome is pending, not claim remote termination. Retain
the real execution lease while that work is outstanding. A complete valid finish
response is preserved against its original input before finalizing the job, even
when cancellation or job cleanup happened during the call. After cancellation,
never execute a returned tool action or begin another model call.

## Durable ownership, storage and restart

Use coordinator-owned versioned tables in the existing state.db with explicit WAL
and synchronous=FULL durability. JobStore provides one transaction boundary for
request deduplication, task lifecycle, ordered journal, result publication and
indexed receipt-reference rows. Insert the initial unclaimed Job and investigation
in the same IMMEDIATE transaction. Claiming execution likewise records the private
namespace/job/generation/owner and CASes the investigation into preparing in the
same transaction as the Job claim; there is no crash window between claiming a
Job and attaching its execution to the investigation. No cascading foreign key to
Jobs may erase history. Do not introduce a second database or rely on ATTACH for
cross-WAL crash atomicity.

Every transition checks task revision, expected phase and exact private execution
identity. Before network dispatch, atomically journal invocation start, consume the
matching approval and reserve budgets. Persist admitted response/action/result and
its receipt references together before job finalization. A failure to stage is not
a successful answer. Events have monotonic sequence IDs and closed kinds; progress
comes from actual events, never invented elapsed-time percentages or model reasoning.

Retain a private immutable job namespace/id/generation/owner reference independently
of deletable Jobs rows. Startup scans the coordinator's own pending/nonterminal
records, including cancellation-requested tasks. Generic Job recovery alone is
insufficient: it ignores cancelled rows and cleanup can remove their attempt rows.
Probe the exact existing execution lock before declaring owner death. A live owner
is not interrupted merely because its UI disconnected. A missing/substituted lock
or failed ownership observation is operational failure, not abandonment.

The frozen graph, raw tool payloads and pending exact preview are RAM-only. After
proved owner death, pre-dispatch/lost-input phases become interrupted; a
started invocation with no durable outcome becomes outcome_unknown. Do not rebuild
an old run or preview from current sources, replay a possibly executed call, or
turn a timestamp/connection loss into proof that execution stopped. Known completed
results and conversations remain queryable. Unknown outcome stays explicit; the
application offers no generic Retry/Resume for these jobs. Any later recovery or
fresh follow-up must be an explicit coordinator action with the prior uncertainty
preserved; it is never an automatic replay or a declaration of remote nonexecution.

History pages return bounded summaries, with detail and result fetched separately
by ID. Reserve envelope headroom before adding an item and use a stable cursor;
a valid maximum-sized detail must not become unreachable through a page budget.

Strict version-dispatched SQL decoding preflights type/length, depth/container
counts and references before allocation. Bound history and total storage admission;
capacity errors are explicit, never silent eviction. A staged-reference index must
be validated against immutable investigation content and published transactionally.
Retention previews include actual investigation receipt references in their counts
and fingerprint. Forgetting alters historical availability, never historical meaning.
No raw source is stored to make restart/resume convenient.

## Application and historical inspection

Add an Investigations workspace subview, a Workspace entry, and a palette route
without changing the eight existing rail shortcut assignments. Jobs link to the
investigation and use coordinator-supported actions instead of generic retry.
Show specialist/provider identity, question, scope/revision, real ordered events,
consumed limits, consent state and durable findings/history. Keep this orchestration
in a dedicated UI store with task ID/request-generation/revision guards. Fetch
missing event sequences after reconnect; duplicate/conflicting/late responses
cannot replace a newer selection or silently reorder history.

Reuse the exact egress payload renderer with investigation-specific decline and
waiting copy. Do not display Keep local as the consequence of denying a cloud
step. Reading or accepting a finding must not claim curated activation.

Historical citation actions take investigation/citation IDs only. Captured source
inspection verifies the saved receipt occurrence, original fact digest, text hash
and range under a tagged retained-source lease; it must not call the current-fact
source viewer or silently read a later checkout. Graph-only historical citations
show their retained metadata/digest and the limitation that raw old graph properties
were not archived. Unverified working-tree input is not retrospectively retained.
Unavailable/invalid/operational failure remain separate observations. Every new
component has a story, including complete App navigation and stale-response cases.

## Verification before delivery claims

Tests must exercise real captured TypeScript production receipts and source access,
not solely fabricate a valid DTO. A scripted provider queries a non-Gap scope, reads
a v2 initializer occurrence, then finishes with cited findings; delete the checkout
before acquisition and prove original bytes reached the actual authorized payload.
Interleave graph and equal-fact receipt changes, unavailable/forgotten source,
cancellation, storage failure, duplicate starts, reconnect and owner death at each
critical phase. A completed result survives cleanup/restart with its original basis.

Provider HTTP/HTTPS fixtures inspect full framed requests and enforce actual body
caps, chunked overflow, slow headers/body under one deadline, no redirects/retries,
TLS validation, complete stop conditions, exact generated-token caps and source-free
errors. Use barriers for races. Preserve existing legacy completion/consent tests.

Provisioning a reachable loopback runtime/model is an acceptance prerequisite;
do not describe the workflow as verified while it is unavailable. A lightweight
harness must call the real transport-independent coordinator, source reader and
bounded provider rather than copy their orchestration. Record that evidence as
coordinator/local-provider acceptance with separately CI-tested UI integration;
it is not a native-app or MCP end-to-end claim.

Run an actual controlled local-provider investigation with pinned input, question,
role/prompt/model identity, tools, output, runtime and usage/coverage record. Review
answer citations independently and keep the expected answers out of its inputs.
No source data is sent to a cloud provider without its exact app consent flow.
A fake provider or green unit suite does not prove this user workflow. Retain real
limits/failures; do not replace the Vendure oracle or claim T3 findings are complete
confirmed rules. Full H4/H5 manual procedures stay unexecuted until actually run.

Run focused local gates under the shared execution policy and complete exact-SHA
CI before ready status. Record the solution as built, validation and remaining
parent gates on #404 before closeout. Do not mark #256 or the market goal complete
from this increment alone.
