//! Reproducible source-only observation run. The caller supplies a staged input
//! tree and its verified revision; this tool never installs or executes it.

use adapters_lang_ts::{SourceId, extract_dir};
use context_hub::ContextSnapshot;
use core_graph::{GraphStore, SqliteGraphStore};
use spec::{ExportMode, compile_spec};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let [input, repo, commit, output] = args.as_slice() else {
        return Err("usage: recover_source_rules <staged-source-root> <repo> <verified-commit> <new-output-dir>".into());
    };
    let input = Path::new(input);
    let output = Path::new(output);
    if output.exists() {
        return Err("output directory already exists".into());
    }
    let extracted = extract_dir(input, &SourceId { repo, commit })?;
    let mut graph = SqliteGraphStore::open_in_memory()?;
    for node in &extracted.nodes {
        graph.put_node(node)?;
    }
    for edge in &extracted.edges {
        graph.put_edge(edge)?;
    }
    let nodes = graph.all_nodes()?;
    let edges = graph.all_edges()?;
    let snapshot = ContextSnapshot::new(nodes.clone(), edges.clone())?;
    let bundle = compile_spec(
        &nodes,
        &edges,
        &[],
        ExportMode::VerifiedOnly,
        &BTreeSet::new(),
    );
    let input_files: BTreeMap<_, _> = nodes
        .iter()
        .filter(|node| node.label == "File")
        .filter_map(|node| node.props["path"].as_str())
        .map(|path| {
            std::fs::read(input.join(path))
                .map(|bytes| (path.to_owned(), core_prov::content_hash(&bytes)))
        })
        .collect::<Result<_, _>>()?;
    let mut gap_reasons = BTreeMap::<String, usize>::new();
    for node in &nodes {
        if let Some(reason) = node.props["reason_code"].as_str() {
            *gap_reasons.entry(reason.into()).or_default() += 1;
        }
    }
    let metadata = serde_json::json!({
        "schema_version": 1,
        "repo": repo,
        "source_commit_supplied_by_caller": commit,
        "input_files": input_files,
        "snapshot_id": snapshot.id(),
        "nodes": nodes.len(),
        "edges": edges.len(),
        "guarded_exit_observations": nodes.iter().filter(|node| node.label == "BusinessRule").count(),
        "gap_reasons": gap_reasons,
        "scope": "source-only TypeScript adapter and stored-fact spec projection; no full ingest, flow inference, target execution or acceptance scoring",
    });
    std::fs::create_dir_all(output)?;
    std::fs::write(
        output.join("metadata.json"),
        serde_json::to_vec_pretty(&metadata)?,
    )?;
    std::fs::write(
        output.join("graph.json"),
        serde_json::to_vec(&serde_json::json!({"nodes": nodes, "edges": edges}))?,
    )?;
    std::fs::write(output.join("bundle.json"), serde_json::to_vec(&bundle)?)?;
    let inventory = bundle
        .artifacts
        .iter()
        .find(|artifact| artifact.file_name == "rule-evidence.md")
        .ok_or("rule artifact missing")?;
    std::fs::write(output.join("rule-evidence.md"), &inventory.content)?;
    println!("{}", serde_json::to_string(&metadata)?);
    Ok(())
}
