//! `pyllow llm` — agent-facing operating manual.
//!
//! Emits a single markdown document on stdout describing pyllow's surfaces,
//! how to interpret each finding type, false-positive classes to be aware
//! of, and verification recipes. Intended to be piped into an LLM context
//! window or saved as a file.

use anyhow::Result;

const GUIDE: &str = include_str!("llm_guide.md");

pub fn run() -> Result<()> {
    print!("{GUIDE}");
    Ok(())
}
