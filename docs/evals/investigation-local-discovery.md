# Local specialist discovery — controlled observation

MT-H4-02 remains **UNEXECUTED as a complete procedure**. One actual-provider
production-module attempt ran and failed its discovery acceptance checks.

The [CI run](https://github.com/qwts/cartograph/actions/runs/34558474540) tested
commit `6068399b18813e067c4cfdb9149f03f75ecdddf9` with Ollama 0.34.0 and Qwen3 8B
Q4_K_M on a four-CPU Linux runner with 16,765,378,560 bytes of RAM. The installed
manifest digest was `500a1f067a9f782620b40bee6f7b0c89e17ae61f686b92c24933e4ca4b2b8b41`.
The runtime used an 8192-token context and had cloud features disabled. The source
was the harness's synthetic captured `rule.ts`; no expected answer entered input.

Domain analyst @1, prompt fingerprint
`ab952c9fe055e1fde7aa5ca3f1d7f60851b402021460fdbd9011a765e20967c3`, returned a valid
`finish` with `insufficient_evidence` and no findings on its first invocation. It
reported that no evidence had been provided instead of asking the host to acquire
context. Recorded counts were one model invocation, zero query/read actions,
zero selected facts, and zero copied evidence. The provider reported 1211 input
tokens and 366 output tokens; active time was 168,213 ms. No request deadline or
host budget was exceeded, and no transport or admission error was recorded.

The coordinator preserved the actual empty result, its ordered events and its
unchanged graph basis through connection reopen. These checks passed, but there
were no source citations to review and the auditor was skipped. Execution status
`completed` describes that persisted finish; it does not establish a successful
investigation. No model task was replayed. The run's
`local-provider-acceptance-6068399b18813e067c4cfdb9149f03f75ecdddf9-1` artifact retains
`setup.json`, `run.json`, the fixture and private coordinator/capture files.

The next revision adds explicit discovery guidance in new @2 specialist
definitions, preserving @1 identities and behavior. This is a change to instructions,
not fabricated query activity or relaxed admission. Its effect requires a separate
actual-provider run on the new source revision. One failed observation is not a
model-wide quality score. Native restart, destructive forgetting, independent
citation review, full H4/H5 interoperability and market gates remain open.
