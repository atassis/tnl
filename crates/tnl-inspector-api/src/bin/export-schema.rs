fn main() -> Result<(), Box<dyn std::error::Error>> {
    serde_json::to_writer_pretty(std::io::stdout().lock(), &tnl_inspector_api::schemas())?;
    Ok(())
}
