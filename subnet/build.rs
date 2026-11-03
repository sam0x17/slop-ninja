//! Records the Git commit this crate was built from, for `--version`.
//!
//! `SLOP_NINJA_BUILD_COMMIT`, when set (a build from a source archive with no
//! `.git`, or a release build), must be 40 hexadecimal digits and wins; any
//! other non-empty value fails the build rather than being ignored. Otherwise
//! `git rev-parse HEAD` in this directory, marked dirty when tracked files had
//! uncommitted changes when this script last ran; and nothing when Git, the
//! repository or the paths that tell Cargo to re-run on a new commit are
//! unavailable (a commit that could go stale is worse than none).
//!
//! `SLOP_NINJA_BUILD_SOURCE` tells the tests which of the three applied.
//!
//! The library compiles this file again under `cfg(test)` so the helpers'
//! unit tests run; `main` is unused there.

use std::path::{Path, PathBuf};
use std::process::Command;

const OVERRIDE: &str = "SLOP_NINJA_BUILD_COMMIT";

fn is_commit(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The override's commit: `Ok(None)` when unset or empty, the lower-case
/// commit when it is 40 hexadecimal digits, and an error otherwise.
fn override_commit(value: Option<&str>) -> Result<Option<String>, String> {
    match value.map(str::trim) {
        None | Some("") => Ok(None),
        Some(v) if is_commit(v) => Ok(Some(v.to_ascii_lowercase())),
        Some(v) => Err(format!(
            "{OVERRIDE} is set to {v:?}, which is not a 40-digit hexadecimal commit; \
             set it to the full commit id or unset it"
        )),
    }
}

/// The files whose change means HEAD moved: HEAD itself (a checkout) and the
/// HEAD reflog (every commit, reset or checkout), both kept in the git
/// directory of this worktree.
fn head_paths(git_dir: &Path) -> [PathBuf; 2] {
    [git_dir.join("HEAD"), git_dir.join("logs").join("HEAD")]
}

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8(out.stdout).ok()?.trim().to_owned())
}

/// The re-run paths: `--git-path` (Git 2.31 or later for an absolute path),
/// else `--absolute-git-dir` (Git 2.13 or later) joined with them.
fn rerun_paths() -> Option<[PathBuf; 2]> {
    let by_git_path = ["HEAD", "logs/HEAD"]
        .map(|p| git(&["rev-parse", "--path-format=absolute", "--git-path", p]).map(PathBuf::from));
    if let [Some(head), Some(log)] = by_git_path
        && head.is_absolute()
        && log.is_absolute()
    {
        return Some([head, log]);
    }
    let dir = PathBuf::from(git(&["rev-parse", "--absolute-git-dir"])?);
    dir.is_absolute().then(|| head_paths(&dir))
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed={OVERRIDE}");
    let from_env = override_commit(std::env::var(OVERRIDE).ok().as_deref())
        .unwrap_or_else(|message| panic!("{message}"));
    let (source, commit) = match from_env {
        Some(commit) => ("override", commit),
        None => match rerun_paths() {
            Some(paths) => {
                for p in paths {
                    println!("cargo:rerun-if-changed={}", p.display());
                }
                match git(&["rev-parse", "--verify", "HEAD^{commit}"]).filter(|c| is_commit(c)) {
                    Some(commit) => {
                        let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                            .is_some_and(|s| !s.is_empty());
                        let suffix = if dirty { "-dirty" } else { "" };
                        ("git", format!("{}{suffix}", commit.to_ascii_lowercase()))
                    }
                    None => ("none", String::new()),
                }
            }
            None => ("none", String::new()),
        },
    };
    println!("cargo:rustc-env={OVERRIDE}={commit}");
    println!("cargo:rustc-env=SLOP_NINJA_BUILD_SOURCE={source}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_is_a_full_commit_or_fails_the_build() {
        let sha = "0123456789ABCDEF0123456789abcdef01234567";
        assert_eq!(override_commit(None), Ok(None));
        assert_eq!(override_commit(Some("")), Ok(None));
        assert_eq!(
            override_commit(Some(sha)),
            Ok(Some(sha.to_ascii_lowercase()))
        );
        assert_eq!(
            override_commit(Some(&format!("{sha}\n"))),
            Ok(Some(sha.to_ascii_lowercase()))
        );
        for bad in [
            "abc123",
            &sha[..39],
            &format!("{sha}0"),
            "g123456789abcdef0123456789abcdef01234567",
        ] {
            let err = override_commit(Some(bad)).expect_err(bad);
            assert!(err.contains(OVERRIDE), "{err}");
        }
    }

    #[test]
    fn the_head_paths_are_in_the_worktree_git_dir() {
        let dir = Path::new("/repo/.git/worktrees/w");
        assert_eq!(
            head_paths(dir),
            [
                PathBuf::from("/repo/.git/worktrees/w/HEAD"),
                PathBuf::from("/repo/.git/worktrees/w/logs/HEAD")
            ]
        );
    }

    #[test]
    fn both_ways_of_finding_the_head_paths_agree() {
        // Needs Git 2.31 or later for --git-path with an absolute path; skipped
        // when this test does not run inside a Git checkout.
        let Some(dir) = git(&["rev-parse", "--absolute-git-dir"]) else {
            return;
        };
        let by_git_path = ["HEAD", "logs/HEAD"].map(|p| {
            git(&["rev-parse", "--path-format=absolute", "--git-path", p]).map(PathBuf::from)
        });
        if let [Some(head), Some(log)] = by_git_path {
            assert_eq!(head_paths(Path::new(&dir)), [head, log]);
        }
        assert!(rerun_paths().is_some());
    }
}
