//! Text-only processing command (no audio).

use tracing::instrument;

use crate::config::PipelineConfig;

// ── Text: text processor chain ──────────────────────────────

#[instrument(skip_all)]
pub async fn run_text(input: Option<String>, cfg: &PipelineConfig) -> Result<(), anyhow::Error> {
    let input_text = if let Some(t) = input {
        t
    } else {
        use std::io::Read;
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf.trim().to_string()
    };
    let text_processors = crate::config::build_text_processors(cfg, None, None)?;
    tracing::info!(
        chars = input_text.chars().count(),
        chain_len = text_processors.len(),
        "processing text"
    );
    let processed_text = crate::text::run_chain(&text_processors, &input_text).await?;
    println!("\n>> {processed_text}");
    Ok(())
}
