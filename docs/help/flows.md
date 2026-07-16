# Flow Inspector

Traces every user-action flow the graph supports: screens, HTTP endpoints,
externally published channels, extension contexts, and extension keyboard
commands. Each hop records the tier, confidence, and evidence that resolved
it; a gap truncates the branch explicitly.

- **Verified-only vs best-effort** projects the same flow with or without
  InferredWeak hops; explicit gaps are retained in both.
- Zero flows? The empty state names every anchor kind recovery sought and the
  count found.
- Gap hops open the Resolution Strategy; other hops open evidence.
