//! visualdebug core: trace types, ingestion, focus, summaries, annotations. Deterministic; no LLM.
pub mod focus;
pub mod ingest;
pub mod summarize;
pub mod trace;

pub use trace::*;
