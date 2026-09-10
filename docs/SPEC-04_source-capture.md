# SPEC-04 — Immutable source capture

Status: acquisition/storage core in progress, #387; producer and application
integration remains #385. Decision: [ADR-0022](adr/ADR-0022-immutable-source-capture.md).
This is an H3 prerequisite, not completion of curated context or AC-0108.

## Meaning and identity

The `source-capture` core acquires an explicitly selected set of local files into
immutable byte buffers. A version 1 canonical manifest binds a host-owned source
identity, capture kind and sorted file entries. Entries bind normalized relative
path, raw BLAKE3 digest and byte length; Git entries additionally bind blob OID and
regular-file mode. Git capture kind binds the exact full commit and root tree OID.
The capture ID is a versioned BLAKE3 digest of the complete manifest. Selection
order does not matter; repository identity, kind, membership and every raw byte do.
Timestamps, filesystem locations and decoded strings are not capture content.

Source identity is a bounded, opaque host-assigned repository key, distinct from
the display name and the legacy `local/<basename>` key. The host must register
distinct repositories/worktrees correctly before production adoption. This core
does not infer identity from a basename or fix existing app graph/cache collisions.
It takes trusted host requests, not webview/agent-supplied authority. Uncaptured
paths mean outside the selected input set, not nonexistent in the repository.

`CapturedFile` exposes immutable raw bytes and a capture file reference containing
source ID, capture ID, path, digest and byte length. A range adds nonempty inclusive
start/exclusive end byte offsets. Reading checks every component together before
slicing the same verified buffer. A reference is an integrity binding, not an
authorization token. No source or capture object Debug representation prints raw
bytes. Manifest serialization contains metadata only.

These references deliberately remain separate from `core-prov::EvidenceRef` and
the immutable version 1 proposal-staging wire format. Capture acquisition proves
which bytes were retained. It cannot establish that an existing fact's producer
used them, that every supporting input was captured, or that its semantic claim
is correct. Captures have no fact tier, review decision or eligible-context flag.

## Acquisition and limits

Working-tree capture opens a host-selected root and reads only selected regular
files through rooted directory handles. Each intermediate directory and the final
file must reject symlinks. Opened handles are checked for regular-file type; special
files must not cause a blocking data read. Bounds apply to actual bytes read, not
only a pre-read metadata length. The exact acquired buffer is hashed and retained;
it is never re-read to manufacture the receipt. Concurrent file writes may affect
that acquired sequence: this is a captured byte set, not an atomic filesystem or
commit snapshot. A dirty checkout, non-Git folder or linked worktree follows this
same explicit working-tree contract.

Git-object capture opens an existing local repository and resolves a full 40-digit
SHA-1 commit with the supported libgit2 object format. Each selected path resolves
through its tree to a regular blob, whose bytes are retained directly. Abbreviated
revisions, symbolic refs, symlinks, gitlinks, absent objects and nonregular selected
entries fail. It does not checkout, evaluate filters, execute hooks, use shell Git,
fetch, discover credentials or contact remotes. A checkout's EOL/filter conversion
does not alter blob identity. HEAD movement after acquisition is irrelevant.
Requested commit and intermediate-tree objects are limited to 8 MiB before body
lookup; selected blob headers are checked against remaining capture/file limits.
These limits bound selected decoded object sizes, not libgit2 process memory:
packed-delta decoding and library caches may consume additional memory. Broad
untrusted-repository rollout still needs an isolated execution/resource boundary.

Paths are UTF-8, slash-separated, relative and canonical without empty, `.` or
`..` components, backslashes, colons (including drive/alternate-stream syntax),
UNC syntax or NUL. Paths are case-sensitive
logical manifest keys; the core does not materialize them into a new filesystem
tree or silently collapse case/Unicode-distinct names. Duplicate selected paths
fail. Absolute paths are rejected even if they happen to point under the root.

Hard maxima are 8,192 selected files, 1,024 bytes per path, 256 bytes per source
identity, 16 MiB per file, 128 MiB raw bytes per capture, 8 MiB serialized manifest,
and 256 KiB per evidence span. Callers may request smaller limits for tests or
bounded tasks, never exceed the hard maxima. Empty files may be captured but have
no valid nonempty span; an empty selection remains an explicit empty capture.
Any acquisition or budget error fails the capture; no silent partial success or
claim of repository-wide coverage is returned.

Raw digests precede decoding. Text span reads use strict UTF-8 for the requested
span only; invalid bytes or split multibyte boundaries fail. Raw buffers may retain
invalid UTF-8 for a byte-aware producer, without calling them verified model text.
BOM and CRLF bytes are neither normalized nor stripped. Existing sanitization,
source-disclosure limits and per-tier consent remain mandatory when a later
consumer constructs a model payload or export.

## Persistence and retention

An explicit host-owned `CaptureStore` stores manifests and deduplicated raw objects
in a separate SQLite/WAL file. The caller supplies its private application storage
location. A transaction publishes a manifest and all its byte objects together;
failure or capacity rejection publishes neither. Repeated persistence of identical
content is idempotent. The effective `max_span_bytes` acquisition policy is retained
separately from the canonical manifest and capture identity, including for empty
captures. Re-persisting identical content atomically retains the stricter of the
stored and incoming caps; it never widens future store reads. Reload and store
span reads enforce that retained cap. Tightening does not revoke immutable buffers
already returned to callers. This is a read bound, not an authorization token.

The store schema is version 2; the capture manifest remains version 1, and capture
file/range references are unchanged. Prototype version 1 stores lack recoverable
read policy and are rejected explicitly, without a default cap or implicit
migration. Missing, mistyped or out-of-range policy values fail closed before
loading source payloads. Existing conflicting or corrupt rows fail rather than being
repaired from new caller content. Stored schema versions, manifest identity,
membership, object digests and actual lengths are revalidated on load, with limits
checked before loading large payloads. Evidence comes from the validated buffer;
there is no fallback to a checkout, Git repository or network. The initial store
span reader validates and loads the complete bounded capture (up to 128 MiB)
before slicing; its 256 KiB output limit does not bound that input cost. A validated
single-object read optimization precedes broad agent exposure.

The store retains captures independently of jobs, disposable graph state and
proposal review. There is no automatic eviction or deletion API in this first
slice. Logical stored content is capped at 2 GiB including object bytes and manifest
bytes, and 10,000 captures; capacity exhaustion is an explicit error that preserves
old readable captures. SQLite pages, indexes, WAL and temporary files add overhead,
so this is a logical-data bound, not a physical disk quota. The production adoption
slice must provide project ownership, reference-aware retention/deletion and
capacity UX before exposing capture broadly. Removing the host storage externally
makes old evidence unavailable; it never changes what historical references mean.

This raw local source cache is an explicit new data store, not a proposal payload.
Only selected files are retained. It is never copied into `proposals.sqlite`, graph
properties, default diagnostic output or exports. It is not encrypted by this core;
the production host must apply its private local-storage permissions and existing
source-access policy. No new egress is authorized by retention.

## Delivery and verification

This core is not yet wired into production ingestion, source viewing or task
assembly. Existing citations and staged proposals remain unverified with their
original IDs and review state. An integration fixture passes a captured buffer to
the real TypeScript parser and reads its captured span after restart and source
mutation; this validates the reusable seam, not production adoption.

Follow-on #385 must bind actual producer inputs, including configurations and
other supporting files; migrate cache reuse without false revision relabeling;
fix host repository ownership; represent unavailable/legacy evidence explicitly;
and connect captured reads to task assembly and the source viewer. A single
captured TS file cannot certify directory resolution, event configuration, ADR
relinking, toolchain, IaC state, traces or plugin inputs. Human acceptance still
cannot certify freshness or upgrade producing tier. Full H3 additionally requires
directed-slot proof, conflict handling and a shared curated projection.

Tests exercise canonical membership, raw changes outside AST spans, distinct source
identities, bounded and symlink-safe acquisition, exact Git objects versus checkout
bytes, missing objects, strict encoding/ranges, atomic capacity failures, corrupt
storage and restart, retained caller span caps after reopen and duplicate
persistence in both policy orders, plus the real parser seam. Test IDs T-0132–0136 bind these
contracts; test counts do not imply full #385 or H3 completion.
