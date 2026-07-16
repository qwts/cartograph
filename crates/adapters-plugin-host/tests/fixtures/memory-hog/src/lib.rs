// Test fixture only: an adapter whose extract-source grows memory without
// bound, proving the host's ResourceLimiter memory cap fails the call
// closed instead of letting the guest exhaust host memory.

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
        let mut chunks: Vec<Vec<u8>> = Vec::new();
        loop {
            chunks.push(vec![0u8; 1_000_000]);
            std::hint::black_box(&chunks);
        }
    }
}

export!(Guest);
