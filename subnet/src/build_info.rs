//! The build's identity for `--version`: a crate version and, when the build
//! could read it, the Git commit it was built from (see `build.rs`).

/// The commit recorded at build time: 40 lower-case hexadecimal digits,
/// followed by `-dirty` when a Git build saw uncommitted changes to tracked
/// files; `None` when no commit was recorded (no Git, no repository, no
/// override).
pub fn build_commit() -> Option<&'static str> {
    commit_or_none(env!("SLOP_NINJA_BUILD_COMMIT"))
}

/// Where the recorded commit came from: `override` (`SLOP_NINJA_BUILD_COMMIT`
/// at build time), `git` (`git rev-parse HEAD`) or `none`.
pub fn build_source() -> &'static str {
    env!("SLOP_NINJA_BUILD_SOURCE")
}

fn commit_or_none(recorded: &'static str) -> Option<&'static str> {
    (!recorded.is_empty()).then_some(recorded)
}

/// The text after the program name in `--version`: `0.1.0 (commit <sha>)`,
/// or the crate version alone when no commit was recorded.
pub fn version_text(crate_version: &str) -> String {
    format_version(crate_version, build_commit())
}

fn format_version(crate_version: &str, commit: Option<&str>) -> String {
    match commit {
        Some(commit) => format!("{crate_version} (commit {commit})"),
        None => crate_version.to_owned(),
    }
}

/// Compiled again only so its helpers' unit tests run (see `build.rs`).
#[cfg(test)]
#[allow(dead_code)]
#[path = "../build.rs"]
mod build_script;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_names_the_commit_when_one_was_recorded() {
        let sha = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            format_version("0.1.0", Some(sha)),
            format!("0.1.0 (commit {sha})")
        );
        assert_eq!(format_version("0.1.0", None), "0.1.0");
        assert_eq!(commit_or_none(""), None);
        assert_eq!(commit_or_none("abc"), Some("abc"));
    }

    #[test]
    fn a_recorded_commit_is_forty_lowercase_hex_digits() {
        if let Some(commit) = build_commit() {
            let sha = commit.strip_suffix("-dirty").unwrap_or(commit);
            assert_eq!(sha.len(), 40, "{commit}");
            assert!(
                sha.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
                "{commit}"
            );
            assert!(
                build_source() == "git" || sha == commit,
                "only a Git build is marked dirty: {commit}"
            );
        } else {
            assert_eq!(build_source(), "none");
        }
    }

    /// The commit compiled in is the one this build came from: the override
    /// the build saw, or the checkout's HEAD now (which shows that Cargo
    /// re-ran the build script when HEAD moved).
    #[test]
    fn the_recorded_commit_is_the_commit_built() {
        let recorded = build_commit().map(|c| c.strip_suffix("-dirty").unwrap_or(c));
        match build_source() {
            "override" => {
                let set = std::env::var("SLOP_NINJA_BUILD_COMMIT")
                    .expect("an override build's tests run with the override set");
                assert_eq!(recorded, Some(set.trim().to_ascii_lowercase().as_str()));
            }
            "git" => {
                let out = std::process::Command::new("git")
                    .args(["rev-parse", "--verify", "HEAD^{commit}"])
                    .current_dir(env!("CARGO_MANIFEST_DIR"))
                    .output()
                    .expect("git ran at build time, so it runs now");
                assert!(out.status.success(), "git rev-parse HEAD failed");
                let head = String::from_utf8(out.stdout).expect("utf-8 commit");
                assert_eq!(recorded, Some(head.trim()));
            }
            other => {
                assert_eq!(other, "none");
                assert_eq!(recorded, None);
            }
        }
    }
}
