//! The binary's exit statuses: 0 success (and `--help`), 2 refused, 64 usage.

use std::process::Command;

fn run(args: &[&str]) -> (i32, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_slop_ninja_artifact_check"))
        .args(args)
        .output()
        .expect("run slop_ninja_artifact_check");
    (
        out.status.code().expect("exit code"),
        String::from_utf8(out.stdout).expect("utf-8 stdout"),
        String::from_utf8(out.stderr).expect("utf-8 stderr"),
    )
}

#[test]
fn help_prints_usage_and_succeeds() {
    for flag in ["--help", "-h", "help"] {
        let (code, stdout, _) = run(&[flag]);
        assert_eq!(code, 0, "{flag}");
        assert!(
            stdout.starts_with("usage: slop_ninja_artifact_check"),
            "{flag}: {stdout}"
        );
        assert!(stdout.contains("64 usage error"), "{flag}: {stdout}");
    }
}

#[test]
fn usage_error_exits_64() {
    for args in [&[][..], &["check-dir"][..], &["no-such-command", "x"][..]] {
        let (code, stdout, stderr) = run(args);
        assert_eq!(code, 64, "{args:?}");
        assert!(stdout.is_empty(), "{args:?}: {stdout}");
        assert!(stderr.starts_with("usage:"), "{args:?}: {stderr}");
    }
}

#[test]
fn unreadable_path_is_refused_as_object() {
    let missing = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("artifact-check-cli-missing-{}", std::process::id()));
    assert!(!missing.exists());
    let missing = missing.to_str().expect("utf-8 path");
    for command in ["check-dir", "check-object", "graph"] {
        let (code, stdout, _) = run(&[command, missing]);
        assert_eq!(code, 2, "{command}");
        assert!(
            stdout.starts_with("refused object: "),
            "{command}: {stdout}"
        );
    }
}
