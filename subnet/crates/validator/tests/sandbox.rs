//! `sandbox-unit --check` end to end: the validator, started inside the
//! bubblewrap sandbox its drop-in describes, reads its configuration and
//! hotkey companion and writes its state directory, while the coldkey, other
//! hotkeys, other home entries and the host's /tmp and /run are absent.
//! Skipped (with a note) where bubblewrap is missing or refused user
//! namespaces.

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

/// Outside `/tmp`: the sandbox's own `/tmp` is a writable tmpfs, so a home
/// under it would get writable private parents there and the tests below
/// would test that instead of the read-only root.
fn scratch() -> tempfile::TempDir {
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_TARGET_TMPDIR")));
    assert!(!root.starts_with(std::env::temp_dir()), "scratch under the temporary directory");
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
    // A host /tmp entry, which the check must confirm absent.
    let planted = tempfile::Builder::new()
        .prefix("sandbox-it-host-")
        .tempfile_in(std::env::temp_dir())
        .unwrap();
    let out = sandbox_unit(&op, &["--check"]);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(report["journal_exists"], false);
    let dirs: Vec<&str> = report["hidden_dirs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|dir| dir.as_str().unwrap())
        .collect();
    assert!(dirs.contains(&"/tmp"), "{report}");
    if Path::new("/run").is_dir() {
        assert!(dirs.contains(&"/run"), "{report}");
    }
    assert!(dirs.contains(&op.home.to_str().unwrap()), "{report}");
    let state = &report["dir_binds"][0];
    assert_eq!(state["path"], op.state.display().to_string(), "{report}");
    assert_eq!(state["access"], "write", "{report}");
    drop(planted);
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

/// The words of the drop-in's `ExecStart=` (every word here is plain), with
/// `%h` replaced and the unit's `run` replaced by `command`.
fn drop_in_argv(op: &Operator, command: &[&str]) -> Vec<String> {
    let out = sandbox_unit(op, &[]);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8(out.stdout).unwrap();
    let exec = text
        .lines()
        .filter_map(|line| line.strip_prefix("ExecStart="))
        .find(|line| !line.is_empty())
        .expect("an ExecStart line");
    let home = op.home.display().to_string();
    let mut words: Vec<String> = exec
        .split(' ')
        .map(|word| {
            assert!(!word.starts_with('"'), "quoted word {word} in {exec}");
            if word == "%h" { home.clone() } else { word.to_owned() }
        })
        .collect();
    assert_eq!(words.pop().as_deref(), Some("run"));
    words.extend(command.iter().map(|word| (*word).to_owned()));
    words
}

fn run_argv(argv: &[String], home: &Path) -> std::process::Output {
    Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("HOME", home)
        .output()
        .expect("run bwrap")
}

/// Inside the sandbox every validator is PID 1, so a write temporary a
/// killed process left (`.journal-write-1-<n>`) collides with the next
/// process's write of the same number. The journal opens anyway: the
/// temporaries are swept under the journal's lock first.
#[test]
fn a_left_journal_temporary_does_not_block_the_next_sandboxed_start() {
    if !bwrap_works() {
        eprintln!("skipped: {BWRAP} is missing or cannot create user namespaces here");
        return;
    }
    let op = operator();
    for name in [".journal-write-1-0", ".journal-write-1-1", ".journal-write-1-2"] {
        std::fs::write(op.state.join(name), b"").unwrap();
    }
    let out = run_argv(&drop_in_argv(&op, &["init"]), &op.home);
    assert!(
        out.status.success(),
        "init inside the sandbox: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(op.state.join("journal.json").is_file());
    let left: Vec<_> = std::fs::read_dir(&op.state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".journal-write-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

/// A drop-in rendered before a configured path changed refuses to start
/// (exit 2) instead of writing that path onto the sandbox's temporary root;
/// so does a journal directory shared with another journal that a running
/// process has open (a retired journal's free lock only logs
/// `journal_dir_shared`). The root itself is read-only.
#[test]
fn a_stale_drop_in_or_a_shared_journal_directory_refuses_to_start() {
    if !bwrap_works() {
        eprintln!("skipped: {BWRAP} is missing or cannot create user namespaces here");
        return;
    }
    let op = operator();
    let argv = drop_in_argv(&op, &["check-config"]);
    let out = run_argv(&argv, &op.home);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // Writes outside the binds fail.
    let mut shell = argv.clone();
    let start = shell.iter().position(|word| word == "--").unwrap() + 1;
    shell.truncate(start);
    shell.extend(["/bin/sh".into(), "-c".into(), "touch /x || touch \"$HOME/x\" || exit 9".into()]);
    assert_eq!(run_argv(&shell, &op.home).status.code(), Some(9));

    let text = std::fs::read_to_string(&op.config).unwrap();
    let runtime = op.home.join("runtime-cache");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(
        &op.config,
        text.replace(
            "[rpc]\n",
            &format!("[rpc]\nruntime_cache_dir = \"{}\"\n", runtime.display()),
        ),
    )
    .unwrap();
    let out = run_argv(&argv, &op.home);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("re-run deploy/install.sh"));
    std::fs::write(&op.config, &text).unwrap();

    std::fs::write(op.state.join("other-journal.json.lock"), b"").unwrap();
    let out = run_argv(&argv, &op.home);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("journal_dir_shared"));
    let other = std::fs::File::open(op.state.join("other-journal.json.lock")).unwrap();
    other.lock().unwrap();
    let out = run_argv(&argv, &op.home);
    assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains("another journal"));
    drop(other);
    // The installer's render refuses the shared directory too.
    let out = sandbox_unit(&op, &[]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("other-journal.json.lock"));
}
