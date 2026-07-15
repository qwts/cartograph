---
'cartograph': minor
---

Chrome runtime messaging channels (US-0016, #99): `chrome.runtime.sendMessage`
/ `chrome.tabs.sendMessage` producer sites and explicit handler registrations
(`[MessageType.X]: handler` dispatch tables behind a real
`onMessage.addListener`) stitch into deterministic `chrome-message` channels
with PUBLISHES/SUBSCRIBES edges connecting extension contexts. Message
identities resolve through string literals, repo-wide const-string maps, and
one-hop creator functions; anything runtime-computed stays an explicit Gap,
and shadowed `chrome` bindings or conflicting const maps fail closed.
