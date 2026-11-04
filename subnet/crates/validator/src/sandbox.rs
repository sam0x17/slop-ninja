//! File-system isolation for the persistent units (`sandbox-unit`).
//!
//! A validator process should read only its binary, its configuration, the
//! hotkey files that configuration names and its own state directories. The
//! units run under the operator's user manager, and systemd's mount-namespace
//! settings (`ProtectHome=`, `BindPaths=`, `PrivateTmp=`, ...) need a user
//! namespace there. Where unprivileged user namespaces are restricted (the
//! Ubuntu AppArmor default) systemd's executor is refused the capability it
//! needs and the unit fails with exit 226, so the unit templates carry only
//! settings that need no namespace, and isolation comes from bubblewrap
//! (`bwrap`), which such hosts allow through its own profile.
//!
//! [`render_drop_in`] writes a drop-in whose `ExecStart=` starts the validator
//! inside bubblewrap: a fresh root holding the system directories (`/usr`,
//! `/etc` and the `/bin`-style links to them), the resolver's runtime
//! directory, a private `/tmp`, `/dev` and `/proc` (its own PID namespace, so
//! no other process and no `/proc/<pid>/root` of one is reachable), the host
//! network, and one bind per path in [`binds`]. Everything else the operator
//! account can read is absent: the rest of the home directory (other
//! wallets, coldkeys, tokens, logins, unrelated data), `/run` (the session
//! bus, the agent sockets, a container daemon's socket) and `/tmp`. The
//! drop-in is generated from the configuration, so the installer writes it
//! again whenever it runs; [`check`] runs the configuration check and a
//! visibility test inside the same sandbox before the drop-in is used.

use std::{
    ffi::OsString,
    path::{Component, Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;

use crate::config::ValidatorConfig;

/// Which persistent unit the drop-in is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    /// `slop_ninja_validator-dispatch@`: `run --dispatch`, which reads the
    /// dispatch hotkey file.
    Dispatch,
    /// `slop_ninja_validator@`: the dry run, which never opens a private key.
    DryRun,
}

impl UnitKind {
    /// The validator arguments the unit runs.
    pub fn command(self) -> &'static [&'static str] {
        match self {
            Self::Dispatch => &["run", "--dispatch"],
            Self::DryRun => &["run"],
        }
    }
}

/// What the sandbox lets the validator do with a bound path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    Write,
}

/// One host path made visible inside the sandbox at the same path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Bind {
    pub path: PathBuf,
    pub access: Access,
    pub dir: bool,
    /// What the configuration uses it for.
    pub role: &'static str,
}

struct Wanted {
    path: PathBuf,
    access: Access,
    dir: bool,
    role: &'static str,
    /// Bound only when it exists (a companion file the validator does not need).
    optional: bool,
}

/// The `<hotkey>pub.txt` companion of a hotkey keyfile.
fn companion(hotkey_file: &Path) -> PathBuf {
    let mut path: OsString = hotkey_file.as_os_str().to_owned();
    path.push("pub.txt");
    PathBuf::from(path)
}

fn parent(path: &Path, role: &str) -> Result<PathBuf> {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .with_context(|| format!("{role} has no parent directory"))
}

/// Every host path the unit's validator reads or writes, from the
/// configuration: deduplicated, with paths inside a bound directory of at
/// least the same access dropped, ordered so that a directory precedes the
/// paths beneath it. Refuses a path with `..`, a directory bind that would
/// expose the home directory (or one of its ancestors) and a required path
/// that does not exist (a missing directory inside the sandbox would be
/// created on its temporary root and lost at every restart).
pub fn binds(
    config: &ValidatorConfig,
    config_path: &Path,
    binary: &Path,
    kind: UnitKind,
    home: &Path,
) -> Result<Vec<Bind>> {
    let want = |path: PathBuf, access, dir, role, optional| Wanted {
        path,
        access,
        dir,
        role,
        optional,
    };
    let mut wanted = vec![
        want(
            binary.to_path_buf(),
            Access::Read,
            false,
            "the validator binary",
            false,
        ),
        want(
            config_path.to_path_buf(),
            Access::Read,
            false,
            "the configuration",
            false,
        ),
        want(
            companion(&config.wallet.hotkey_file()),
            Access::Read,
            false,
            "the wallet hotkey's public companion",
            false,
        ),
        want(
            parent(&config.journal_path, "journal_path")?,
            Access::Write,
            true,
            "the journal directory",
            false,
        ),
    ];
    if kind == UnitKind::Dispatch
        && let Some(dispatch) = &config.dispatch
    {
        wanted.push(want(
            dispatch.hotkey_file.clone(),
            Access::Read,
            false,
            "the dispatch hotkey file",
            false,
        ));
        wanted.push(want(
            companion(&dispatch.hotkey_file),
            Access::Read,
            false,
            "the dispatch hotkey's public companion",
            true,
        ));
    }
    if let Some(package) = &config.package {
        // The fetcher installs bundles there (a staged file renamed into
        // place); without it the operator replaces the bundle, and a bound
        // directory, unlike a bound file, shows the replacement.
        let access = if config.fetch.is_some() {
            Access::Write
        } else {
            Access::Read
        };
        wanted.push(want(
            parent(&package.path, "package.path")?,
            access,
            true,
            "the package directory",
            false,
        ));
    }
    if let Some(fetch) = &config.fetch {
        wanted.push(want(
            fetch.cache_dir(&config.journal_path)?,
            Access::Write,
            true,
            "the fetch cache",
            false,
        ));
        let base = fetch.base()?;
        if base.scheme() == "file" {
            let dir = base
                .to_file_path()
                .map_err(|()| anyhow::anyhow!("fetch.base_url file:// is not an absolute path"))?;
            wanted.push(want(dir, Access::Read, true, "the fetch mirror", false));
        }
    }
    if let Some(dir) = &config.rpc.runtime_cache_dir {
        wanted.push(want(
            dir.clone(),
            Access::Write,
            true,
            "the runtime metadata cache",
            false,
        ));
    }

    for item in &wanted {
        check_path(&item.path, item.role)?;
        if item.dir {
            ensure!(
                !home.starts_with(&item.path),
                "{} {} is the home directory or contains it; give it a directory of its own \
                 (for example ~/.local/state/slop_ninja_validator)",
                item.role,
                item.path.display()
            );
        }
    }
    // Shallow paths first, so a directory is bound before what it contains;
    // an exact repeat keeps the stronger access.
    wanted.sort_by(|a, b| {
        (a.path.components().count(), &a.path).cmp(&(b.path.components().count(), &b.path))
    });
    let mut kept: Vec<Bind> = Vec::new();
    for item in wanted {
        if let Some(same) = kept.iter_mut().find(|bind| bind.path == item.path) {
            ensure!(
                same.dir == item.dir,
                "{} and {} name the same path {} as a file and as a directory",
                same.role,
                item.role,
                item.path.display()
            );
            same.access = same.access.max(item.access);
            continue;
        }
        let covered = kept.iter().any(|bind| {
            bind.dir && item.path.starts_with(&bind.path) && bind.access >= item.access
        });
        if covered {
            continue;
        }
        let exists = if item.dir {
            item.path.is_dir()
        } else {
            item.path.is_file()
        };
        if !exists {
            if item.optional {
                continue;
            }
            bail!(
                "{} {} is not an existing {}{}",
                item.role,
                item.path.display(),
                if item.dir { "directory" } else { "file" },
                if item.dir {
                    "; create it (mode 700) before installing the sandbox"
                } else {
                    ""
                }
            );
        }
        kept.push(Bind {
            path: item.path,
            access: item.access,
            dir: item.dir,
            role: item.role,
        });
    }
    // The first sort placed shallow paths first; exact repeats were merged
    // in place, so the order still has each directory before its contents.
    Ok(kept)
}

/// Absolute, valid UTF-8, no control characters, and only plain components
/// (no `.` or `..`), so the path means the same inside the sandbox and in a
/// unit file.
fn check_path(path: &Path, role: &str) -> Result<()> {
    ensure!(path.is_absolute(), "{role} must be an absolute path");
    let text = path
        .to_str()
        .with_context(|| format!("{role} is not valid UTF-8"))?;
    ensure!(
        !text.chars().any(char::is_control),
        "{role} contains a control character"
    );
    ensure!(
        path.components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_))),
        "{role} {} must not contain . or .. components",
        path.display()
    );
    ensure!(
        path != Path::new("/"),
        "{role} must not be the root directory"
    );
    Ok(())
}

/// The host's root layout as the sandbox reproduces it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Read-only binds and symbolic links, in order.
    pub entries: Vec<RootEntry>,
    /// The runtime directory holding the resolver configuration that
    /// `/etc/resolv.conf` links to, when it lives under `/run`.
    pub resolver_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RootEntry {
    ReadOnly(PathBuf),
    Symlink { target: PathBuf, link: PathBuf },
}

impl Layout {
    /// Reads this host's `/usr`, `/etc`, the `/bin`-style entries (links into
    /// `/usr` on a merged-`/usr` system, directories otherwise) and the
    /// resolver link.
    pub fn host() -> Result<Self> {
        let mut entries = Vec::new();
        for dir in ["/usr", "/etc"] {
            ensure!(Path::new(dir).is_dir(), "{dir} is not a directory");
            entries.push(RootEntry::ReadOnly(PathBuf::from(dir)));
        }
        for top in ["/bin", "/sbin", "/lib", "/lib32", "/lib64", "/libx32"] {
            let path = Path::new(top);
            let Ok(meta) = path.symlink_metadata() else {
                continue;
            };
            if meta.file_type().is_symlink() {
                entries.push(RootEntry::Symlink {
                    target: std::fs::read_link(path)?,
                    link: path.to_path_buf(),
                });
            } else if meta.is_dir() {
                entries.push(RootEntry::ReadOnly(path.to_path_buf()));
            }
        }
        let resolver_dir = std::fs::canonicalize("/etc/resolv.conf")
            .ok()
            .filter(|path| path.starts_with("/run"))
            .and_then(|path| path.parent().map(Path::to_path_buf));
        Ok(Self {
            entries,
            resolver_dir,
        })
    }
}

/// One word of the sandboxed command line.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Word {
    Lit(String),
    /// The home directory: `%h` in a unit file, the real path when run.
    Home,
}

fn lit(text: impl Into<String>) -> Word {
    Word::Lit(text.into())
}

fn path_word(path: &Path) -> Word {
    // check_path or the layout already required valid UTF-8.
    Word::Lit(path.to_string_lossy().into_owned())
}

/// The bubblewrap options that build the sandbox, without the command.
fn sandbox_words(bwrap: &Path, layout: &Layout, binds: &[Bind]) -> Vec<Word> {
    let mut words = vec![
        path_word(bwrap),
        lit("--unshare-all"),
        lit("--unshare-user"),
        lit("--share-net"),
        lit("--disable-userns"),
        // The validator is PID 1 of its namespace, so systemd's SIGTERM
        // reaches it directly and it pauses its journal; bubblewrap's
        // monitor outside the namespace passes on its exit code.
        lit("--as-pid-1"),
        lit("--new-session"),
        lit("--clearenv"),
        lit("--setenv"),
        lit("HOME"),
        Word::Home,
        lit("--setenv"),
        lit("RUST_BACKTRACE"),
        lit("0"),
    ];
    for entry in &layout.entries {
        match entry {
            RootEntry::ReadOnly(path) => {
                words.extend([lit("--ro-bind"), path_word(path), path_word(path)]);
            }
            RootEntry::Symlink { target, link } => {
                words.extend([lit("--symlink"), path_word(target), path_word(link)]);
            }
        }
    }
    if let Some(dir) = &layout.resolver_dir {
        words.extend([lit("--ro-bind-try"), path_word(dir), path_word(dir)]);
    }
    words.extend([
        lit("--proc"),
        lit("/proc"),
        lit("--dev"),
        lit("/dev"),
        lit("--tmpfs"),
        lit("/tmp"),
    ]);
    for bind in binds {
        let option = match bind.access {
            Access::Read => "--ro-bind",
            Access::Write => "--bind",
        };
        words.extend([lit(option), path_word(&bind.path), path_word(&bind.path)]);
    }
    words.extend([lit("--chdir"), lit("/"), lit("--")]);
    words
}

fn validator_words(binary: &Path, config_path: &Path, command: &[&str]) -> Vec<Word> {
    let mut words = vec![path_word(binary), lit("--config"), path_word(config_path)];
    words.extend(command.iter().map(|word| lit(*word)));
    words
}

/// A word for a unit file's `ExecStart=`: bare when it is plain, otherwise
/// double-quoted with `\`, `"`, the specifier `%` and the variable `$`
/// escaped.
fn unit_word(word: &Word) -> Result<String> {
    let text = match word {
        Word::Home => return Ok("%h".to_owned()),
        Word::Lit(text) => text,
    };
    ensure!(
        !text.chars().any(char::is_control),
        "a sandbox argument contains a control character"
    );
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+=,:@".contains(c));
    if plain {
        return Ok(text.clone());
    }
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    for c in text.chars() {
        match c {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '%' => quoted.push_str("%%"),
            '$' => quoted.push_str("$$"),
            other => quoted.push(other),
        }
    }
    quoted.push('"');
    Ok(quoted)
}

/// The drop-in for `kind`'s unit: resets `ExecStart=` and starts the same
/// command inside bubblewrap, and sets the namespace and system-call
/// allow-lists to the template's plus exactly what bubblewrap needs to build
/// the sandbox (no network namespace; no nested user namespace inside it).
/// Both lists are reset first, so the drop-in means the same over a template
/// that sets neither.
pub fn render_drop_in(
    bwrap: &Path,
    layout: &Layout,
    binds: &[Bind],
    binary: &Path,
    config_path: &Path,
    kind: UnitKind,
) -> Result<String> {
    check_path(bwrap, "the bwrap path")?;
    ensure!(
        unit_word(&path_word(bwrap))? == bwrap.to_string_lossy(),
        "the bwrap path must be plain (letters, digits and /._-+=,:@)"
    );
    let mut words = sandbox_words(bwrap, layout, binds);
    words.extend(validator_words(binary, config_path, kind.command()));
    let line = words
        .iter()
        .map(unit_word)
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let mut text = String::from(
        "# Generated by `slop_ninja_validator sandbox-unit` (deploy/install.sh writes it).\n\
         # Runs the validator in a bubblewrap sandbox that shows it only the system\n\
         # directories, its binary, its configuration, the hotkey files that\n\
         # configuration names and its state directories. Re-run the installer\n\
         # after changing any path in the configuration; do not edit by hand.\n",
    );
    text.push_str("# Bound paths:\n");
    for bind in binds {
        let access = match bind.access {
            Access::Read => "read-only",
            Access::Write => "read-write",
        };
        text.push_str(&format!(
            "#   {access:<10} {} ({})\n",
            bind.path.to_string_lossy().replace('\n', " "),
            bind.role
        ));
    }
    text.push_str(
        "[Service]\n\
         RestrictNamespaces=\n\
         RestrictNamespaces=user mnt pid ipc uts cgroup\n\
         SystemCallFilter=\n\
         SystemCallFilter=@system-service @mount\n\
         SystemCallErrorNumber=EPERM\n\
         ExecStart=\n",
    );
    text.push_str("ExecStart=");
    text.push_str(&line);
    text.push('\n');
    Ok(text)
}

fn os_words(words: &[Word], home: &Path) -> Vec<OsString> {
    words
        .iter()
        .map(|word| match word {
            Word::Lit(text) => OsString::from(text),
            Word::Home => home.as_os_str().to_owned(),
        })
        .collect()
}

/// Content-free result of [`check`].
#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub binds: usize,
    /// Host entries the test confirmed absent inside the sandbox.
    pub hidden_checked: usize,
    pub journal_exists: bool,
}

/// Runs `check-config` and a visibility test inside the sandbox the drop-in
/// describes (the same bubblewrap options): every bound path must be
/// readable (directories bound read-write also writable), and every other
/// host entry of each directory above a bound path (from `/` down, other
/// wallets and keys beside a bound hotkey included) and of the home
/// directory must be absent, as must the host's entries of `/tmp`. Reads no
/// key and no chain.
pub fn check(
    bwrap: &Path,
    layout: &Layout,
    binds: &[Bind],
    binary: &Path,
    config_path: &Path,
    home: &Path,
    journal_exists: bool,
) -> Result<CheckReport> {
    let sandbox = sandbox_words(bwrap, layout, binds);
    let run = |tail: Vec<Word>| -> Result<std::process::Output> {
        let mut words = sandbox.clone();
        words.extend(tail);
        let argv = os_words(&words, home);
        Command::new(&argv[0])
            .args(&argv[1..])
            .output()
            .context("cannot start bwrap")
    };

    let out = run(validator_words(binary, config_path, &["check-config"]))?;
    ensure!(
        out.status.success(),
        "check-config failed inside the sandbox (exit {:?}): {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    let summary: serde_json::Value = serde_json::from_slice(&out.stdout)
        .context("check-config inside the sandbox printed no JSON summary")?;
    let inside = summary["journal_exists"].as_bool();
    ensure!(
        inside == Some(journal_exists),
        "the sandbox sees journal_exists = {inside:?}, the host {journal_exists}"
    );

    let mut tests: Vec<Word> = Vec::new();
    for bind in binds {
        tests.extend([lit("r"), path_word(&bind.path)]);
        if bind.access == Access::Write {
            tests.extend([lit("w"), path_word(&bind.path)]);
        }
    }
    let bound: Vec<&Path> = binds.iter().map(|bind| bind.path.as_path()).collect();
    // Shown whole: the system directories and links the layout binds.
    let mut wholesale: Vec<&Path> = layout
        .entries
        .iter()
        .map(|entry| match entry {
            RootEntry::ReadOnly(path) => path.as_path(),
            RootEntry::Symlink { link, .. } => link.as_path(),
        })
        .collect();
    wholesale.extend(layout.resolver_dir.as_deref());
    // Present in the sandbox, but fresh: their host entries must not show.
    let fresh = [Path::new("/proc"), Path::new("/dev"), Path::new("/tmp")];
    let on_a_bound_path = |path: &Path| {
        bound
            .iter()
            .chain(wholesale.iter())
            .any(|shown| shown.starts_with(path) || path.starts_with(shown))
    };
    // Every directory above a bound path, and the home directory.
    let mut scanned: Vec<&Path> = bound
        .iter()
        .flat_map(|path| path.ancestors().skip(1))
        .collect();
    scanned.push(home);
    scanned.sort();
    scanned.dedup();
    let mut hidden = Vec::new();
    for dir in scanned {
        let shown_whole = bound
            .iter()
            .zip(binds)
            .any(|(path, bind)| bind.dir && dir.starts_with(path))
            || wholesale.iter().any(|path| dir.starts_with(path));
        if shown_whole {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if on_a_bound_path(&path)
                || fresh.contains(&path.as_path())
                || path
                    .to_str()
                    .is_none_or(|text| text.chars().any(char::is_control))
            {
                continue;
            }
            hidden.push(path);
        }
    }
    for path in &hidden {
        tests.extend([lit("x"), path_word(path)]);
    }
    let script = "fail=0; while [ $# -gt 1 ]; do op=$1; p=$2; shift 2; case $op in \
                  r) test -r \"$p\" || { echo \"unreadable: $p\"; fail=1; } ;; \
                  w) test -w \"$p\" || { echo \"not writable: $p\"; fail=1; } ;; \
                  x) if test -e \"$p\" || test -L \"$p\"; then echo \"visible: $p\"; fail=1; fi ;; \
                  esac; done; exit $fail";
    let mut tail = vec![lit("/bin/sh"), lit("-c"), lit(script), lit("sh")];
    tail.extend(tests);
    let out = run(tail)?;
    ensure!(
        out.status.success(),
        "the sandbox visibility test failed (exit {:?}):\n{}{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(CheckReport {
        binds: binds.len(),
        hidden_checked: hidden.len(),
        journal_exists,
    })
}

/// Whether bubblewrap can build a sandbox on this host (user namespaces
/// permitted to it): runs `/bin/true` in the least sandbox the drop-in uses.
pub fn available(bwrap: &Path) -> bool {
    Command::new(bwrap)
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
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}
