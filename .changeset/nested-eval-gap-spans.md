---
"cartograph": patch
---

A Gap found inside `eval()` code now cites the eval's string argument once, not once per inner location. It also records how many inner locations could not be mapped back into that string (`unmapped_inner_spans`). Before, its evidence listed the same outer span several times.
