---
"cartograph": patch
---

Java Spring mappings written with array syntax (`@GetMapping({ "/vets" })`) now recover their real routes instead of collapsing to `/`. Mappings whose path is a constant or expression are reported as explicit route gaps instead of fabricated Confirmed endpoints.
