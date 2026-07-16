# Concepts: tiers, gaps, and honesty

Cartograph recovers a system's design from its code and records **how it knows
every fact**.

- **Tiers** are how a fact was produced: **T0** deterministic parsing, **T1**
  dynamic observation, **T2** semantic inference, **T3** agentic proposal.
  Higher tiers can never overwrite T0/T1 facts (R-INT-1).
- **Confidence** is carried on every fact: Confirmed, InferredStrong,
  InferredWeak, or Gap — always visible, never color-only (R-INT-2).
- **System gap** — evidence exists but could not be resolved statically. A gap
  is never guessed at: it truncates the trace explicitly and can be escalated
  tier by tier. Escalations only *propose*; you accept or reject (R-INT-3/4).
- **Unsupported pattern** — a construct no installed adapter reads. A tool
  limitation, never a system gap; fix it with an adapter, not escalation.
- **Verified-only vs best-effort** — verified-only exports exclude
  InferredWeak facts; best-effort includes them, annotated. Explicit gaps
  survive both projections (R-INT-5).
