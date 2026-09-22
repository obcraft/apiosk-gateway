//! Render an already-saved research snapshot locally. No model or provider calls.
fn main() -> anyhow::Result<()> {
    let args: Vec<_> = std::env::args().collect();
    anyhow::ensure!(
        args.len() == 3,
        "usage: render-research-report snapshot.json output.pdf"
    );
    let bytes = std::fs::read(&args[1])?;
    anyhow::ensure!(bytes.len() <= 64 * 1024 * 1024, "snapshot is too large");
    let document = serde_json::from_slice(&bytes)?;
    let pdf = apiosk_gateway_v2::research_report::render(&document)?;
    std::fs::write(&args[2], &pdf)?;
    println!("Rendered {} bytes", pdf.len());
    Ok(())
}
