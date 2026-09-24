---
"cartograph": patch
---

A relation cited from several call or import sites now keeps every site's evidence, not just the last one's. The stored edge lists the spans in source order, without duplicates and up to a bound. It states how many spans it omitted beyond that bound. Its tier and confidence are unchanged.
