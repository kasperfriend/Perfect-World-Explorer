//! A `diagnostics` stub for the standalone diagnostic binaries.
//!
//! Several binaries in `src/bin` build library modules by `#[path]` — the water
//! reader, the far-tree forest, the grass loader — and those modules report
//! through `crate::diag!`. A `#[path]` module set has its own crate root, so
//! without this the macro is unresolved and the binary stops compiling. The
//! stub keeps the call sites intact and discards the lines, which is what a
//! diagnostic binary wants anyway: it prints its own report.
//!
//! Usage in a binary:
//!
//! ```ignore
//! #[macro_use]
//! #[path = "support/diag_stub.rs"]
//! mod diag_stub;
//! pub use diag_stub::diagnostics;
//! ```
//!
//! It lives in a subdirectory so cargo does not treat it as a binary target.

pub mod diagnostics {
    /// The channels these stubs are asked about. Only the ones a `#[path]`
    /// module actually names need to exist here.
    #[allow(dead_code)]
    #[derive(Clone, Copy)]
    pub enum Channel {
        SceneWater,
        TimingLoad,
    }

    pub fn enabled(_channel: Channel) -> bool {
        false
    }

    pub fn emit(_channel: Channel, _line: String) {}

    pub fn emit_keyed(_channel: Channel, _key: &str, _slow_ms: u64, _line: String) {}
}

#[macro_export]
macro_rules! diag {
    ($channel:ident, $($arg:tt)*) => {{
        let _ = format!($($arg)*);
    }};
}

#[macro_export]
macro_rules! diag_keyed {
    ($channel:ident, $key:expr, $slow:expr, $($arg:tt)*) => {{
        let _ = ($key, $slow, format!($($arg)*));
    }};
}
