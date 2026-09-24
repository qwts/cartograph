---
"cartograph": minor
---

Recovery now parses source files in parallel, and Settings adds an **Ingest parallelism** control (Auto, or a fixed number of workers). Auto uses one worker per performance core, minus one, and fewer on machines with little memory. Large repositories recover several times faster without using more memory, and the recovered graph is identical whatever the setting.
