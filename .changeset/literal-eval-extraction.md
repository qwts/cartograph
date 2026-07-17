---
'cartograph': minor
---

Literal eval() and new Function() sites give up their facts instead of
dead-ending: when the code argument is compile-time-known (a string
literal, a substitution-free template, or a same-file const proven by
binding), the TS adapter parses the string with the same extractor and
emits its Symbols, CALLS, and sites as Confirmed T0 — marked `via:
"eval"`, cited at the argument's span at the eval site — and the
enclosing symbol calls into the extracted code so flows cross the eval
boundary. Preflight's inline-eval finding now reconciles with that
proof: fully proven sites are covered and close on the next scan,
const-shaped-but-unproven arguments downgrade to explicit potential
Gaps, and everything dynamic stays an Unsupported finding — never a
guess.
