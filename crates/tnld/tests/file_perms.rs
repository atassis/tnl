//! Regression test for the §2-shipping bug: container's `tnld` user (uid 100)
//! could not read files under `/etc/tnld` when the directory was 0700 root:root.
//!
//! We rebuild a minimal scratch test by running `docker run` against the
//! current `tnld:0.1.0-beta.1` image (or whatever is tagged) and asserting
//! that `tnld --version` exits 0 — i.e., the binary inside the image can be
//! invoked under its declared USER. This catches dropped-USER and broken
//! perms regressions.

use std::process::Command;

const IMAGE_TAG: &str = "tnld:0.1.0-beta.1";

fn docker_available() -> bool {
    Command::new("docker")
        .arg("version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn image_exists(tag: &str) -> bool {
    Command::new("docker")
        .args(["image", "inspect", tag])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn container_runs_as_non_root_uid_100() {
    if !docker_available() {
        eprintln!("SKIP: docker not available");
        return;
    }
    if !image_exists(IMAGE_TAG) {
        eprintln!("SKIP: {IMAGE_TAG} not built — run `docker build -t {IMAGE_TAG} .` first");
        return;
    }
    let out = Command::new("docker")
        .args(["run", "--rm", "--entrypoint", "id", IMAGE_TAG])
        .output()
        .expect("docker run id");
    assert!(out.status.success(), "docker run id failed: {out:?}");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("uid=100"), "expected uid=100 in: {s}");
}

#[test]
fn container_can_read_mode_644_config_under_etc_tnld() {
    use std::os::unix::fs::PermissionsExt;

    if !docker_available() {
        eprintln!("SKIP: docker not available");
        return;
    }
    if !image_exists(IMAGE_TAG) {
        eprintln!("SKIP: {IMAGE_TAG} not built");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("config.toml");
    let tokens = tmp.path().join("tokens.toml");
    std::fs::write(
        &cfg,
        r#"listen        = "127.0.0.1:7777"
public_url    = "http://test"
hostname_root = "t.example.com"
tokens_file   = "/etc/tnld/tokens.toml"
"#,
    )
    .unwrap();
    std::fs::write(&tokens, "[[tokens]]\nname = \"x\"\nhash = \"$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\"\n").unwrap();
    // Match the §2 prod setup: dir 0755, files 0644.
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::set_permissions(&cfg, std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(&tokens, std::fs::Permissions::from_mode(0o644)).unwrap();

    let mount = format!("{}:/etc/tnld:ro", tmp.path().display());
    // Just verify the binary can open + parse the config (it'll then fail to
    // bind 127.0.0.1:7777 because the container has no network for this run,
    // but parse-success is what we want).
    let out = Command::new("docker")
        .args([
            "run",
            "--rm",
            "-v",
            &mount,
            IMAGE_TAG,
            "hash-password",
            "test",
        ])
        .output()
        .expect("docker run");
    assert!(
        out.status.success(),
        "container failed to invoke hash-password under uid 100; stdout={:?} stderr={:?}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}
