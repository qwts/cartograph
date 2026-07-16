// Test fixture only: an adapter whose extract-source never returns, proving
// the host's fuel bound and epoch deadline both fail the call closed.

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
        let mut x: u64 = 0;
        loop {
            x = x.wrapping_add(1);
            std::hint::black_box(x);
        }
    }
}

export!(Guest);
