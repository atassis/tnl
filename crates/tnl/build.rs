use anyhow::{bail, Context, Result};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::{
    env, fs,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Rules {
    files: Vec<String>,
    directories: Vec<String>,
    exclude: Vec<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    sha256: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Asset {
    path: String,
    mime: String,
    size: usize,
    sha256: String,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u8,
    inputs: Vec<Input>,
    assets: Vec<Asset>,
}

fn safe_path(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.contains('\\') {
        bail!("noncanonical path");
    }
    let mut path = root.to_path_buf();
    for part in Path::new(relative).components() {
        let Component::Normal(part) = part else {
            bail!("unsafe relative path");
        };
        path.push(part);
        if fs::symlink_metadata(&path)?.file_type().is_symlink() {
            bail!("symlink forbidden");
        }
    }
    Ok(path)
}
fn walk(root: &Path, relative: &str, exclude: &[String], files: &mut Vec<String>) -> Result<()> {
    if exclude.iter().any(|path| path == relative) {
        return Ok(());
    }
    let path = safe_path(root, relative)?;
    let metadata = fs::symlink_metadata(&path)?;
    if metadata.is_file() {
        files.push(relative.to_owned());
        return Ok(());
    }
    if !metadata.is_dir() {
        bail!("unsupported source file");
    }
    for entry in fs::read_dir(path)? {
        let name = entry?
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non-UTF8 input path"))?;
        let child = if relative.is_empty() {
            name
        } else {
            format!("{relative}/{name}")
        };
        walk(root, &child, exclude, files)?;
    }
    Ok(())
}
fn hash(path: &Path) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(fs::read(path)?)))
}
fn mime(path: &str) -> Option<&'static str> {
    if path == "index.html" {
        return Some("text/html; charset=utf-8");
    }
    let name = path.strip_prefix("assets/")?;
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty()
        || !stem
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return None;
    }
    match extension {
        "js" => Some("text/javascript; charset=utf-8"),
        "css" => Some("text/css; charset=utf-8"),
        _ => None,
    }
}
fn run() -> Result<()> {
    let client = PathBuf::from(
        env::var_os("CARGO_MANIFEST_DIR").context("client manifest directory missing")?,
    );
    let root = client
        .parent()
        .and_then(Path::parent)
        .context("repository root missing")?;
    let output = client.join("src/inspection/assets");
    let manifest_path = safe_path(&client, "src/inspection/assets/manifest.json")?;
    if fs::metadata(&manifest_path)?.len() > 65536 {
        bail!("manifest is too large");
    }
    let manifest: Manifest = serde_json::from_slice(&fs::read(manifest_path)?)?;
    if manifest.format_version != 1 {
        bail!("unsupported inspector manifest version");
    }
    let rules: Rules = serde_json::from_str(include_str!("../../tools/inspector-inputs.json"))?;
    let mut sources = Vec::new();
    for path in rules.files.iter().chain(&rules.directories) {
        // Directory watches detect added/removed inputs; hash comparison excludes
        // generated files and validates content rather than trusting timestamps.
        println!("cargo:rerun-if-changed={}", root.join(path).display());
        walk(root, path, &rules.exclude, &mut sources)?;
    }
    sources.sort();
    sources.dedup();
    if sources.len() != manifest.inputs.len() {
        bail!("inspector source set changed");
    }
    for (path, input) in sources.iter().zip(&manifest.inputs) {
        if path != &input.path || hash(&safe_path(root, path)?)? != input.sha256 {
            bail!("inspector source changed: {path}");
        }
    }
    println!("cargo:rerun-if-changed={}", output.display());
    let mut asset_paths = Vec::new();
    walk(&output, "", &[], &mut asset_paths)?;
    asset_paths.retain(|path| path != "manifest.json");
    asset_paths.sort();
    if asset_paths.len() != manifest.assets.len() {
        bail!("inspector asset set changed");
    }
    let mut js = 0usize;
    let mut css = 0usize;
    let mut total = 0usize;
    let mut generated = String::from("pub(super) static ASSETS: &[Asset] = &[\n");
    let mut html = false;
    for (path, asset) in asset_paths.iter().zip(&manifest.assets) {
        let expected_mime = mime(path).context("unsupported embedded asset path")?;
        let file = safe_path(&output, path)?;
        let size = usize::try_from(fs::metadata(&file)?.len())?;
        if path != &asset.path
            || expected_mime != asset.mime
            || size != asset.size
            || hash(&file)? != asset.sha256
        {
            bail!("inspector asset modified: {path}");
        }
        total = total.checked_add(size).context("asset size overflow")?;
        match expected_mime {
            "text/javascript; charset=utf-8" => {
                js = js.checked_add(size).context("JS size overflow")?;
            }
            "text/css; charset=utf-8" => {
                css = css.checked_add(size).context("CSS size overflow")?;
            }
            _ => {}
        }
        html |= path == "index.html";
        let route = if path == "index.html" {
            "/".into()
        } else {
            format!("/{path}")
        };
        writeln!(
            generated,
            "Asset {{ path: {route:?}, mime: {expected_mime:?}, bytes: include_bytes!({:?}) }},",
            file.to_str().context("non-UTF8 asset path")?
        )?;
    }
    if !html || js == 0 || css == 0 || js > 512 * 1024 || css > 128 * 1024 || total > 1024 * 1024 {
        bail!("embedded asset budget/entry point invalid");
    }
    generated.push_str("];\n");
    let destination =
        PathBuf::from(env::var_os("OUT_DIR").context("Cargo output directory missing")?);
    fs::write(destination.join("inspector_assets.rs"), generated)?;
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("Inspector assets unavailable, stale or invalid: {error:#}\nRun corepack pnpm ui:prepare before building the client.");
        std::process::exit(1);
    }
}
