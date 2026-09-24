---
"cartograph": patch
---

Java and Kotlin Spring endpoints now keep a trailing slash written in the source. For example, `@RequestMapping("/api/")` with a bare `@GetMapping` is recorded as `/api/` instead of `/api`, and `/a` and `/a/` are separate endpoints, as they are in Spring 6. Endpoint ids for such routes change on the next ingest.
