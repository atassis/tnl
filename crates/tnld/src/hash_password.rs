use crate::auth::hash_plaintext;

pub fn run(plaintext: &str) -> anyhow::Result<()> {
    let hash = hash_plaintext(plaintext)?;
    println!("{hash}");
    Ok(())
}
