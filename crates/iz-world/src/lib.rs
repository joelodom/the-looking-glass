//! World persistence, the pipeline/engine and the mock services.
//!
//! The app needs only the constructors:
//! - [`RealEngine::new`] with the real describer and painter;
//! - [`mock_engine`] for `IZ_MOCK=1` (no network, no cost).

pub mod mock;
pub mod pipeline;
pub mod store;

pub use mock::{mock_engine, MockDescriber, MockPainter};
pub use pipeline::RealEngine;

#[cfg(test)]
pub(crate) mod test_support {
    use iz_types::log::{CallLog, LogEvent};
    use std::path::{Path, PathBuf};
    use std::sync::OnceLock;

    /// Points `IZ_DATA_DIR` at a fresh temp dir, once per test binary, before
    /// any path is resolved. Every test calls this first.
    pub fn data_dir() -> &'static Path {
        static DIR: OnceLock<PathBuf> = OnceLock::new();
        DIR.get_or_init(|| {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let dir = std::env::temp_dir().join(format!("iz-world-unit-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create test data dir");
            std::env::set_var(iz_types::env::DATA_DIR, &dir);
            dir
        })
    }

    pub struct NullLog;

    impl CallLog for NullLog {
        fn event(&self, _event: &LogEvent) {}
    }
}
