//! `slop_ninja_validator --version` and `-V` print the crate version and the
//! build's commit, without the otherwise required `--config`.

use std::process::Command;

fn version_output(flag: &str) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_slop_ninja_validator"))
        .arg(flag)
        .output()
        .expect("run slop_ninja_validator");
    assert!(out.status.success(), "{flag} exited {:?}", out.status);
    String::from_utf8(out.stdout).expect("utf-8 version")
}

#[test]
fn the_validator_reports_its_version_and_commit() {
    let expected = match slop_ninja_subnet::build_info::build_commit() {
        Some(commit) => format!(
            "slop_ninja_validator {} (commit {commit})\n",
            env!("CARGO_PKG_VERSION")
        ),
        None => format!("slop_ninja_validator {}\n", env!("CARGO_PKG_VERSION")),
    };
    assert_eq!(version_output("--version"), expected);
    assert_eq!(version_output("-V"), expected);
}
