//! AC-0156–0160 / AC-0163: run the production ownership modules and their
//! existing tests on native runners without linking the Tauri application.
//!
//! Keep these module names: the process-exit regression invokes the exact
//! `job_execution::tests::execution_owner_child` test in this same binary.

// JobStore persists this production data type, without running the semantic
// resolver. Import its exact definition and preserve the qualified type path.
extern crate self as semantic;
#[path = "../../../crates/semantic/src/eval_report.rs"]
mod eval_report;
pub use eval_report::EvalReport;

// The small harness omits host callers and the app's public API documentation
// surface. These allowances apply only to this test target; implementation
// errors and Clippy diagnostics retain their normal gates.
#[allow(dead_code, missing_docs, unused_imports)]
#[path = "../../../src-tauri/src/jobs.rs"]
mod jobs;

#[allow(dead_code)]
#[path = "../../../src-tauri/src/job_execution.rs"]
mod job_execution;
