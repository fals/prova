//! Framework adapters and output recognizers (ADR 0003, 0013).

pub mod junit;
pub mod results;

use std::path::Path;

use toe_core::config::RunSpec;
use toe_core::engine::Observer;

/// Chooses how a run is observed. Every run reaches at least level 0; any
/// result files written during the run lift it to level 1.
pub fn observer_for(spec: &RunSpec, _root: &Path) -> Box<dyn Observer> {
    Box::new(results::ResultFiles::new(&spec.results))
}
