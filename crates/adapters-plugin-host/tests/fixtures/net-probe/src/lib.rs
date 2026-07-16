// Test fixture only: an adapter that attempts a TCP connect during
// extract-source, proving the host grants no ambient network capability —
// the default WasiCtx allows no sockets, so this must be denied before any
// real connection is attempted.

wit_bindgen::generate!({
    path: "../../../wit",
    world: "adapter",
});

struct Guest;

impl exports::cartograph::adapter::extract::Guest for Guest {
    fn extract_source(
        _source: Vec<u8>,
        _path: String,
        _id: exports::cartograph::adapter::extract::SourceId,
    ) -> Result<
        exports::cartograph::adapter::extract::Extraction,
        exports::cartograph::adapter::extract::ExtractError,
    > {
        use exports::cartograph::adapter::extract::{Extraction, Node};
        use std::net::{SocketAddr, TcpStream};

        let addr = SocketAddr::from(([127, 0, 0, 1], 1));
        let outcome = match TcpStream::connect(addr) {
            Ok(_) => "connected".to_string(),
            Err(e) => format!("denied:{}", e.kind() as i32),
        };

        Ok(Extraction {
            nodes: vec![Node {
                id: "net-probe".to_string(),
                label: "NetProbe".to_string(),
                props_json: format!(r#"{{"outcome":"{outcome}"}}"#),
            }],
            edges: vec![],
        })
    }
}

export!(Guest);
