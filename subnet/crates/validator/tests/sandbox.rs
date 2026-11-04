//! `sandbox-unit --check` end to end: the validator, started inside the
//! bubblewrap sandbox its drop-in describes, reads its configuration and
//! hotkey companion and writes its state directory, while the coldkey, other
//! hotkeys, other home entries and the host's /tmp are absent. Skipped (with
//! a note) where bubblewrap is missing or refused user namespaces.

use std::path::{Path, PathBuf};
use std::process::Command;

const BWRAP: &str = "/usr/bin/bwrap";

fn bwrap_works() -> bool {
    Path::new(BWRAP).is_file()
        && Command::new(BWRAP)
            .args([
                "--unshare-all",
                "--unshare-user",
                "--share-net",
                "--disable-userns",
                "--ro-bind",
                "/",
                "/",
                "--",
                "/bin/true",
            ])
            .output()
            .is_ok_and(|out| out.status.success())
}

fn scratch() -> tempfile::TempDir {
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    tempfile::Builder::new()
        .prefix("sandbox-it-")
        .tempdir_in(root)
        .expect("scratch directory")
}

struct Operator {
    _root: tempfile::TempDir,
    home: PathBuf,
    config: PathBuf,
    state: PathBuf,
}

/// An operator home holding the things a validator must not see beside the
/// ones it needs. Every key file is synthetic; the public companion names a
/// fixed test account.
fn operator() -> Operator {
    let root = scratch();
    let home = root.path().join("home");
    let state = home.join(".local/state/slop_ninja_validator");
    let wallet = home.join(".bittensor/wallets/w");
    let hotkeys = wallet.join("hotkeys");
    let config_dir = home.join(".config/slop_ninja_validator");
    for dir in [
        &state,
        &hotkeys,
        &config_dir,
        &home.join(".config/.wrangler"),
        &home.join("secrets"),
    ] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let address = rtao_primitives::AccountId([2; 32]);
    let public = serde_json::json!({
        "publicKey": format!("0x{}", hex::encode(address.0)),
        "ss58Address": address.to_string(),
    })
    .to_string();
    std::fs::write(hotkeys.join("hkpub.txt"), &public).unwrap();
    std::fs::write(hotkeys.join("hk"), b"synthetic keyfile").unwrap();
    std::fs::write(hotkeys.join("other"), b"synthetic keyfile").unwrap();
    std::fs::write(hotkeys.join("otherpub.txt"), &public).unwrap();
    std::fs::write(wallet.join("coldkey"), b"synthetic coldkey").unwrap();
    std::fs::write(
        home.join(".config/.wrangler/config.toml"),
        b"synthetic login",
    )
    .unwrap();
    std::fs::write(home.join("secrets/seed"), b"synthetic seed").unwrap();
    let config = config_dir.join("v.toml");
    std::fs::write(
        &config,
        format!(
            "network = \"test\"\nnetuid = 601\nweight_version = 0\njournal_path = \"{}\"\n\
             [wallet]\npath = \"{}\"\nname = \"w\"\nhotkey = \"hk\"\n\
             [rpc]\nendpoints = [\"wss://test.finney.opentensor.ai:443\"]\n",
            state.join("journal.json").display(),
            home.join(".bittensor/wallets").display(),
        ),
    )
    .unwrap();
    Operator {
        _root: root,
        home,
        config,
        state,
    }
}

fn sandbox_unit(op: &Operator, extra: &[&str]) -> std::process::Output {
    let binary = env!("CARGO_BIN_EXE_slop_ninja_validator");
    Command::new(binary)
        .arg("--config")
        .arg(&op.config)
        .args([
            "sandbox-unit",
            "--unit",
            "dry-run",
            "--binary",
            binary,
            "--bwrap",
            BWRAP,
        ])
        .args(extra)
        .env("HOME", &op.home)
        .output()
        .expect("run slop_ninja_validator")
}

#[test]
fn the_sandbox_shows_only_the_configured_paths() {
    if !bwrap_works() {
        eprintln!("skipped: {BWRAP} is missing or cannot create user namespaces here");
        return;
    }
    let op = operator();
    let out = sandbox_unit(&op, &["--check"]);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["journal_exists"], false);
    // At least the other hotkey and its companion, the coldkey, the hotkey
    // keyfile itself (the dry run never reads it), .config/.wrangler and
    // secrets/ were confirmed absent.
    assert!(report["hidden_checked"].as_u64().unwrap() >= 6, "{report}");

    // The check fails when the sandbox would show something it must not:
    // a journal directory that is the home directory is refused outright.
    let text = std::fs::read_to_string(&op.config).unwrap();
    std::fs::write(
        &op.config,
        text.replace(
            &op.state.join("journal.json").display().to_string(),
            &op.home.join("journal.json").display().to_string(),
        ),
    )
    .unwrap();
    let out = sandbox_unit(&op, &["--check"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("home directory"));
}

#[test]
fn the_printed_drop_in_names_no_unbound_path() {
    let op = operator();
    let out = sandbox_unit(&op, &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.contains("hkpub.txt"));
    for absent in [
        "coldkey",
        "otherpub.txt",
        ".wrangler",
        "secrets",
        "hotkeys/hk ",
    ] {
        assert!(!text.contains(absent), "drop-in names {absent}:\n{text}");
    }
    assert!(
        text.trim_end()
            .ends_with(&format!("--config {} run", op.config.display()))
    );
}
