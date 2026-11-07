//! Phase1 parser/input helpers, not the executable's protected handshake.
use argon2::{password_hash::PasswordHash, Argon2, PasswordVerifier};
use clap::Parser;
use std::{fs, sync::Arc};
use tnl::access::AccessArgs;

#[derive(Parser)]
struct Input {
    #[command(flatten)]
    access: AccessArgs,
}

fn parse(args: &[&str]) -> AccessArgs {
    Input::try_parse_from(std::iter::once("tnl").chain(args.iter().copied()))
        .unwrap()
        .access
}

#[test]
fn public_default_needs_no_password_or_tls_and_has_no_policy() {
    let policy =
        parse(&[]).prepare_with_prompt("http://localhost:8000", || panic!("public prompt"));
    assert!(policy.unwrap().is_none());
}

#[test]
fn controls_are_independent_and_burst_defaults_only_when_rate_present() {
    for args in [
        vec!["--allow", "127.0.0.1"],
        vec!["--max-concurrent", "1"],
        vec!["--rate-limit", "2"],
    ] {
        let policy = parse(&args)
            .prepare_with_prompt("https://example.com", || panic!("no Basic"))
            .unwrap()
            .unwrap();
        assert!(policy.basic().is_none());
        if let Some(rate) = policy.rate_limit() {
            assert_eq!(rate.requests_per_second, 2);
            assert_eq!(rate.burst, 2);
        }
    }
}

#[test]
fn parser_rejects_dependencies_duplicates_bounds_and_plaintext_options() {
    for args in [
        vec!["--basic-auth-password-file", "secret"],
        vec!["--rate-burst", "1"],
        vec!["--max-concurrent", "0"],
        vec!["--rate-limit", "10001"],
        vec!["--rate-limit", "1", "--rate-limit", "2"],
        vec!["--basic-auth", "a", "--basic-auth", "b"],
        vec!["--basic-auth-password", "SECRET_SENTINEL"],
        vec!["--password-hash", "hash"],
        vec!["--allow"],
    ] {
        assert!(Input::try_parse_from(std::iter::once("tnl").chain(args)).is_err());
    }
}

#[test]
fn invalid_endpoint_and_options_are_rejected_before_secret_input() {
    for endpoint in [
        "http://example.com",
        "wss://example.com",
        "https://",
        "https://user:pw@example.com",
        "https://example.com/?query=1",
    ] {
        let err = parse(&["--basic-auth", "user"])
            .prepare_with_prompt(endpoint, || panic!("must not prompt"))
            .unwrap_err();
        assert!(!format!("{err:?} {err}").contains("pw"));
    }
    for args in [
        vec!["--basic-auth", "user:pass"],
        vec!["--basic-auth", "user", "--allow", "hostname"],
        vec!["--basic-auth", "user", "--allow", "::ffff:192.0.2.1/95"],
    ] {
        assert!(parse(&args)
            .prepare_with_prompt("https://example.com", || panic!("must validate first"))
            .is_err());
    }
}

#[test]
fn basic_only_hashes_locally_and_retains_exact_verifier_for_run() {
    let policy = parse(&["--basic-auth", "user"])
        .prepare_with_prompt("https://example.com", || {
            Ok(" secret:with spaces ".to_owned().into())
        })
        .unwrap()
        .unwrap();
    assert!(policy.rate_limit().is_none());
    assert!(policy.max_concurrent().is_none());
    assert!(policy.allow_ips().is_empty());
    let fingerprint = policy.fingerprint().unwrap();
    let retained = Arc::clone(&policy);
    assert_eq!(retained.fingerprint().unwrap(), fingerprint);
    let basic = retained.basic().unwrap();
    let hash = basic.password_hash().expose_for_verification();
    Argon2::default()
        .verify_password(b" secret:with spaces ", &PasswordHash::new(hash).unwrap())
        .unwrap();
    assert!(!format!("{policy:?}").contains(hash));
    assert!(!format!("{policy:?}").contains("secret:with spaces"));
}

#[cfg(unix)]
fn write_password(path: &std::path::Path, value: &[u8], mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    if path.exists() {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    fs::write(path, value).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
#[test]
fn password_file_bounds_mode_type_newlines_and_no_reread() {
    use tnl::access::read_password_file;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("secret");
    for value in [b" :secret \n".as_slice(), b" :secret \r\n", b" :secret "] {
        write_password(&path, value, 0o600);
        assert_eq!(&**read_password_file(&path).unwrap(), " :secret ");
    }
    for value in [
        vec![],
        vec![b'x'; 1025],
        b"secret\n\n".to_vec(),
        b"secret\0".to_vec(),
        vec![255],
    ] {
        write_password(&path, &value, 0o600);
        let err = read_password_file(&path).unwrap_err();
        assert!(!format!("{err:?} {err}").contains("secret"));
    }
    write_password(&path, &vec![b'x'; 1024], 0o400);
    assert_eq!(read_password_file(&path).unwrap().len(), 1024);
    write_password(&path, b"SECRET_SENTINEL", 0o640);
    assert!(read_password_file(&path).is_err());
    assert!(read_password_file(dir.path()).is_err());
    let link = dir.path().join("link");
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(read_password_file(&link).is_err());
    write_password(&path, b"SECRET_SENTINEL", 0o600);
    assert!(read_password_file(&link).is_ok());
    let args = parse(&[
        "--basic-auth",
        "user",
        "--basic-auth-password-file",
        path.to_str().unwrap(),
    ]);
    let policy = args
        .prepare_with_prompt("https://example.com", || panic!("file selected"))
        .unwrap()
        .unwrap();
    let fingerprint = policy.fingerprint().unwrap();
    fs::remove_file(path).unwrap();
    assert_eq!(Arc::clone(&policy).fingerprint().unwrap(), fingerprint);
}

#[test]
fn invalid_prompt_passwords_never_disclose_input() {
    for value in [
        String::new(),
        "SECRET_SENTINEL\n".to_owned(),
        "SECRET_SENTINEL".repeat(100),
    ] {
        let err = parse(&["--basic-auth", "user"])
            .prepare_with_prompt("https://example.com", || Ok(value.into()))
            .unwrap_err();
        assert!(!format!("{err:?} {err}").contains("SECRET_SENTINEL"));
    }
}

#[test]
fn prompt_requires_a_terminal_and_does_not_echo_read_stdin() {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        let err = parse(&["--basic-auth", "user"])
            .prepare("https://example.com")
            .unwrap_err();
        assert!(matches!(err, tnl::access::AccessInputError::Terminal));
    }
}

#[cfg(unix)]
#[test]
fn password_file_exact_byte_limit_with_final_crlf_and_unicode_controls() {
    use tnl::access::read_password_file;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("password");
    let password = format!("{}\r\n", "é".repeat(512));
    write_password(&path, password.as_bytes(), 0o600);
    assert_eq!(read_password_file(&path).unwrap().len(), 1024);
    for value in [
        vec![b'x'; 1027],
        "SECRET_SENTINEL\u{85}".as_bytes().to_vec(),
    ] {
        write_password(&path, &value, 0o600);
        let err = read_password_file(&path).unwrap_err();
        assert!(!format!("{err:?} {err}").contains("SECRET_SENTINEL"));
    }
}

#[test]
fn executable_http_exposes_optional_controls_and_rejects_http_before_password_file() {
    let binary = env!("CARGO_BIN_EXE_tnl");
    let help = std::process::Command::new(binary)
        .args(["http", "3000", "--help"])
        .output()
        .unwrap();
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--basic-auth"));
    let dir = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(binary)
        .args([
            "http",
            "3000",
            "--basic-auth",
            "user",
            "--basic-auth-password-file",
            "/does-not-exist",
        ])
        .env("TNL_CONFIG", dir.path().join("config.toml"))
        .env("TNL_ENDPOINT", "http://127.0.0.1:1")
        .env("TNL_TOKEN", "tnl_TEST")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!output.status.success());
    assert!(stderr.contains("https endpoint"));
    assert!(!stderr.contains("unable to read password"));
}
