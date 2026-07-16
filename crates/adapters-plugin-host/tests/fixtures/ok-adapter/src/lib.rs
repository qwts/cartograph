// Test fixture only: a minimal well-behaved adapter proving the host <-> guest
// round trip. Not a real language adapter.

wit_bindgen::generate!({
    path: "../../../wit",
    world: "adapter",
});

struct Guest;

impl exports::cartograph::adapter::extract::Guest for Guest {
    fn extract_source(
        source: Vec<u8>,
        path: String,
        id: exports::cartograph::adapter::extract::SourceId,
    ) -> Result<
        exports::cartograph::adapter::extract::Extraction,
        exports::cartograph::adapter::extract::ExtractError,
    > {
        use exports::cartograph::adapter::extract::{Edge, Extraction, Node};

        let node = Node {
            id: format!("{}:{}", id.repo, path),
            label: "TestNode".to_string(),
            props_json: format!(r#"{{"len":{}}}"#, source.len()),
        };
        let edge = Edge {
            src: node.id.clone(),
            dst: node.id.clone(),
            label: "SELF".to_string(),
            props_json: "{}".to_string(),
        };

        Ok(Extraction {
            nodes: vec![node],
            edges: vec![edge],
        })
    }
}

export!(Guest);
