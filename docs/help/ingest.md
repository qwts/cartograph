# Ingest: Connect → Preflight → Recover

Recovery is a three-step, local-only flow:

1. **Connect** — point at a GitHub repo, a local folder, or a system manifest
   (multiple repos as one system). Nothing leaves the device.
2. **Preflight** — a pure filesystem scan detects languages, frameworks, and
   adapter coverage, and classifies risky constructs before any recovery:
   potential gaps vs unsupported patterns, never conflated.
3. **Recover** — deterministic extraction runs as a durable, cancellable job;
   the summary reports per-language file/node/edge counts, including zeros.
