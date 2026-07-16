// Test fixture only: an adapter that reads the wall clock and the
// monotonic clock during extract-source, proving the host grants no
// ambient clock (both must be fixed, not tracking real time).

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
        use std::time::{Instant, SystemTime, UNIX_EPOCH};

        let wall_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(u128::MAX);

        let start = Instant::now();
        let mut x: u64 = 0;
        for i in 0..1000u64 {
            x = x.wrapping_add(i);
        }
        std::hint::black_box(x);
        let elapsed_nanos = start.elapsed().as_nanos();

        Ok(Extraction {
            nodes: vec![Node {
                id: "clock-probe".to_string(),
                label: "ClockProbe".to_string(),
                props_json: format!(
                    r#"{{"wall_millis":{wall_millis},"elapsed_nanos":{elapsed_nanos}}}"#
                ),
            }],
            edges: vec![],
        })
    }
}

export!(Guest);
