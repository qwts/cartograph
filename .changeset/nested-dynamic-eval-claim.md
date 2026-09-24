---
"cartograph": patch
---

An `eval()` whose code holds a dynamic nested `eval` (for example `eval("eval(x + 1)")`) now keeps its Unsupported finding in Preflight after recovery. Before, the line was marked covered even though the recovered code still held an unsupported dynamic eval. The facts recovered from the outer code are unchanged.
