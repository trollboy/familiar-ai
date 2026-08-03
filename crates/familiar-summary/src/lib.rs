//! Heuristic file summary generation for Familiar.
//!
//! Detects language by extension, extracts top-level symbols via regex,
//! captures the first docblock, and synthesizes a short prose summary.
//! No tree-sitter, no LLM — that's a future PRD.

pub mod extractor;
pub mod generator;
pub mod language;

pub use extractor::{extract, ExtractedFile};
pub use generator::{GeneratedSummary, SummaryGenerator};
pub use language::{detect_language, Language};
