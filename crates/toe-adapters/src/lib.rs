//! Framework adapters and output recognizers (ADR 0003, 0013).

use std::path::Path;

use toe_core::config::RunSpec;
use toe_core::engine::{Observer, Opaque};

/// Chooses how a run is observed. Every run gets at least level 0.
pub fn observer_for(_spec: &RunSpec, _root: &Path) -> Box<dyn Observer> {
    Box::new(Opaque)
}
