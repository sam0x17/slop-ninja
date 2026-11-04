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
//! visibility test inside the same sandbox before the drop-in is used. The
//! sandbox's root is remounted read-only after the binds, so a write to a
//! path the drop-in does not bind fails instead of landing on a temporary
//! root that vanishes at the next start (except under the sandbox's own
//! private `/tmp` and `/dev`, which are writable and start empty each time:
//! a configured path under `/tmp` gets writable private parent
//! directories there), and the drop-in sets [`SANDBOX_ENV`]
//! to a digest of its bind list, which [`guard`] compares with the bind list
//! the configuration names now: a drop-in older than a configuration change
//! refuses to start (exit 2) instead of running with unbound state.
//!
//! Inside the sandbox the validator is PID 1, as it is in every other
//! sandboxed validator's namespace. rtao's native journal names its write
//! temporary `.journal-write-<pid>-<n>` in the journal's directory, with `n`
//! counting from 0 in each process, and creates it exclusively; nothing
//! removes one a killed process left behind. [`sweep_journal_temps`]
//! removes them before the journal opens, while holding the journal's lock
//! and the lock of every other journal in the directory, and [`guard`]
//! refuses a sandboxed start whose journal directory holds another journal
//! a running process has open (both would be PID 1 and collide on the same
//! names). A retired journal's lock, which no process holds, blocks neither.

use std::{
    ffi::{OsStr, OsString},
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
    /// The `sandbox-unit --unit` name, also the first field of [`SANDBOX_ENV`].
    pub fn name(self) -> &'static str {
        match self {
            Self::Dispatch => "dispatch",
            Self::DryRun => "dry-run",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        [Self::Dispatch, Self::DryRun]
            .into_iter()
            .find(|kind| kind.name() == name)
    }

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
    /// Configured paths inside this directory that need no bind of their
    /// own, with their roles: they get this bind's access, which can be
    /// more than they need (a package directory inside the read-write
    /// journal directory is writable).
    pub covers: Vec<(PathBuf, &'static str)>,
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

/// Home-relative directories that many tools of one account share. A
/// directory bind is refused when it is one of these (it is also refused
/// when it holds the home directory, the wallets, a hotkey file or the
/// configuration).
pub const SHARED_HOME_DIRS: &[&str] = &[
    ".config",
    ".cache",
    ".local",
    ".local/state",
    ".local/share",
    ".local/bin",
    ".local/lib",
    ".bittensor",
    ".ssh",
    ".gnupg",
];

/// Every host path the unit's validator reads or writes, from the
/// configuration: deduplicated, with paths inside a bound directory of at
/// least the same access dropped, ordered so that a directory precedes the
/// paths beneath it. Refuses a path with `..`, a directory bind that would
/// expose the home directory (or one of its ancestors), a directory in
/// [`SHARED_HOME_DIRS`], one that holds the wallets, a hotkey file or the
/// configuration, and a required path that does not exist (a missing
/// directory inside the sandbox would be created on its temporary root and
/// lost at every restart).
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

    // A directory is bound whole, so it must not be one every tool of this
    // account shares (other tools' logins, tokens and state would show, the
    // caches' read-write), nor hold the wallets, a hotkey file or the
    // configuration (other keys, other instances' configurations).
    let shared: Vec<PathBuf> = SHARED_HOME_DIRS.iter().map(|dir| home.join(dir)).collect();
    let mut secrets = vec![
        (config.wallet.path.clone(), "the wallet directory"),
        (config.wallet.hotkey_file(), "the wallet hotkey file"),
        (config_path.to_path_buf(), "the configuration"),
    ];
    if let Some(dispatch) = &config.dispatch {
        secrets.push((dispatch.hotkey_file.clone(), "the dispatch hotkey file"));
    }
    for item in &wanted {
        check_path(&item.path, item.role)?;
        if !item.dir {
            continue;
        }
        ensure!(
            !home.starts_with(&item.path),
            "{} {} is the home directory or contains it; give it a directory of its own \
             (for example ~/.local/state/slop_ninja_validator/<instance>)",
            item.role,
            item.path.display()
        );
        ensure!(
            !shared.contains(&item.path),
            "{} {} is a directory other tools of this account share; give it a directory of its \
             own (for example ~/.local/state/slop_ninja_validator/<instance>)",
            item.role,
            item.path.display()
        );
        for (secret, what) in &secrets {
            ensure!(
                !secret.starts_with(&item.path),
                "{} {} contains {what} {}, which the sandbox would show whole; give it a \
                 directory of its own (for example ~/.local/state/slop_ninja_validator/<instance>)",
                item.role,
                item.path.display(),
                secret.display()
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
        if let Some(cover) = kept.iter_mut().find(|bind| {
            bind.dir && item.path.starts_with(&bind.path) && bind.access >= item.access
        }) {
            cover.covers.push((item.path, item.role));
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
            covers: Vec::new(),
        });
    }
    // The first sort placed shallow paths first; exact repeats were merged
    // in place, so the order still has each directory before its contents.
    Ok(kept)
}

/// The variable the drop-in sets inside the sandbox: `<unit>:<digest>`, the
/// unit name and [`binds_digest`] of the bind list the drop-in was rendered
/// with.
pub const SANDBOX_ENV: &str = "SLOP_NINJA_SANDBOX_BINDS";

/// SHA-256 over the bind list's paths, access and kind, in bind order: what
/// the drop-in mounts, so it changes exactly when the drop-in must.
pub fn binds_digest(binds: &[Bind]) -> String {
    use sha2::{Digest as _, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"slop_ninja_sandbox_binds_v1\n");
    for bind in binds {
        let access = match bind.access {
            Access::Read => "r",
            Access::Write => "w",
        };
        let kind = if bind.dir { "d" } else { "f" };
        let path = bind.path.as_os_str().as_encoded_bytes();
        hasher.update(format!("{access}{kind}{}:", path.len()).as_bytes());
        hasher.update(path);
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

fn marker(kind: UnitKind, binds: &[Bind]) -> String {
    format!("{}:{}", kind.name(), binds_digest(binds))
}

/// rtao's native journal names its write temporaries
/// `.journal-write-<pid>-<n>` in the journal's directory.
const JOURNAL_TEMP_PREFIX: &str = ".journal-write-";

/// `name` is `stem` or `stem.<anything>`.
fn in_family(name: &OsStr, stem: &OsStr) -> bool {
    let (name, stem) = (name.as_encoded_bytes(), stem.as_encoded_bytes());
    name == stem || (name.len() > stem.len() && name.starts_with(stem) && name[stem.len()] == b'.')
}

fn journal_parts(journal: &Path) -> Result<(&Path, &OsStr)> {
    let dir = journal
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .context("journal_path has no parent directory")?;
    let name = journal
        .file_name()
        .context("journal_path has no file name")?;
    Ok((dir, name))
}

/// Another journal's lock in the journal's directory: a `*.lock` entry that
/// is not one of this journal's (`<journal>.lock`, `<journal>.*.lock`). rtao
/// never unlinks a lock file, so every journal ever opened there has one,
/// held (an exclusive `flock`) only while a process has that journal open.
fn foreign_locks(journal: &Path) -> Result<Vec<PathBuf>> {
    let (dir, name) = journal_parts(journal)?;
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))? {
        let entry = entry?;
        let entry_name = entry.file_name();
        if entry_name.as_encoded_bytes().ends_with(b".lock") && !in_family(&entry_name, name) {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// Takes `path`'s lock as rtao does (`flock`, exclusive, without waiting),
/// opening it read-only and never creating it. `Ok(None)`: it is gone.
/// An error: held by a live process, or not a regular file this account
/// can open (counted as held: nothing shows that no writer owns it).
fn take_lock(path: &Path) -> Result<Option<std::fs::File>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("cannot read {}", path.display())),
        Ok(meta) => ensure!(meta.file_type().is_file(), "{} is not a regular file", path.display()),
    }
    let file = match std::fs::File::open(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        other => other.with_context(|| format!("cannot open {}", path.display()))?,
    };
    file.try_lock()
        .map_err(|_| anyhow::anyhow!("{} is held by a running process", path.display()))?;
    Ok(Some(file))
}

/// The other journals' locks in the journal's directory, split into those
/// held now by a running process (or that cannot be tested) and those no
/// process holds (a retired journal's).
pub fn foreign_journal_locks(journal: &Path) -> Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let (mut held, mut unheld) = (Vec::new(), Vec::new());
    for lock in foreign_locks(journal)? {
        match take_lock(&lock) {
            Ok(Some(_)) => unheld.push(lock),
            Ok(None) => {}
            Err(_) => held.push(lock),
        }
    }
    Ok((held, unheld))
}

/// Entries of the directories the sandbox binds read-write as a whole that
/// belong to none of this instance's files, or that are not an empty
/// directory. In the journal's directory this instance owns its journal
/// (`<journal>` and `<journal>.*`, rtao's write temporaries), the package
/// (`<package>` and `<package>.*` beside it, or the directory holding it)
/// and the fetch cache, a `file://` mirror and the runtime cache (or the
/// directories holding them); in a package directory of its own (written
/// by the fetcher), the package family and those caches; in the fetch cache,
/// its lock, `cohorts` and `authorizations`; in the runtime cache, rtao's
/// metadata files (`md*.bin`) and their temporaries (`.rtao-md-*`). Another
/// instance's journal, an old journal, key files, a configuration or another
/// tool's files kept there would all be readable and writable inside the
/// sandbox; the drop-in is refused while any is present.
pub fn bind_strays(config: &ValidatorConfig) -> Result<Vec<PathBuf>> {
    let (dir, name) = journal_parts(&config.journal_path)?;
    let package = config.package.as_ref().map(|package| package.path.clone());
    let package_dir = package.as_deref().and_then(Path::parent).map(Path::to_path_buf);
    let fetch_cache = match &config.fetch {
        Some(fetch) => Some(fetch.cache_dir(&config.journal_path)?),
        None => None,
    };
    let mut owned: Vec<PathBuf> = Vec::new();
    owned.extend(fetch_cache.clone());
    if let Some(fetch) = &config.fetch
        && let Ok(base) = fetch.base()
        && base.scheme() == "file"
        && let Ok(mirror) = base.to_file_path()
    {
        owned.push(mirror);
    }
    owned.extend(config.rpc.runtime_cache_dir.clone());
    let stem = |path: &Option<PathBuf>| path.as_deref().and_then(Path::file_name).map(OsStr::to_os_string);
    let package_stem = stem(&package);

    let mut strays = Vec::new();
    // The journal's directory.
    let journal_owned: Vec<PathBuf> = owned.iter().cloned().chain(package.clone()).collect();
    strays.extend(dir_strays(dir, &journal_owned, |entry| {
        in_family(entry, name)
            || entry.as_encoded_bytes().starts_with(JOURNAL_TEMP_PREFIX.as_bytes())
            || (package_dir.as_deref() == Some(dir)
                && package_stem.as_deref().is_some_and(|stem| in_family(entry, stem)))
    })?);
    // A package directory of its own: read-write when the fetcher installs,
    // or inside the read-write journal directory.
    if let Some(package_dir) = package_dir.as_deref()
        && package_dir != dir
        && (config.fetch.is_some() || package_dir.starts_with(dir))
    {
        strays.extend(dir_strays(package_dir, &owned, |entry| {
            package_stem.as_deref().is_some_and(|stem| in_family(entry, stem))
        })?);
    }
    // The fetch cache and the runtime cache, read-write wherever they are.
    if let Some(cache) = &fetch_cache {
        strays.extend(dir_strays(cache, &owned, |entry| {
            [FETCH_CACHE_LOCK, "cohorts", "authorizations"]
                .iter()
                .any(|known| entry == OsStr::new(known))
        })?);
    }
    if let Some(cache) = &config.rpc.runtime_cache_dir {
        strays.extend(dir_strays(cache, &owned, |entry| {
            let bytes = entry.as_encoded_bytes();
            (bytes.starts_with(b"md") && bytes.ends_with(b".bin")) || bytes.starts_with(b".rtao-md-")
        })?);
    }
    strays.sort();
    strays.dedup();
    Ok(strays)
}

/// The fetch cache's lock file (`fetch::LOCK_FILE`).
const FETCH_CACHE_LOCK: &str = ".lock";

/// Entries of `dir` that `ours` does not name, that are not (and do not
/// hold) an `owned` path, and that are not an empty directory. A missing
/// directory has none ([`binds`] refuses it).
fn dir_strays(dir: &Path, owned: &[PathBuf], ours: impl Fn(&OsStr) -> bool) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        other => other.with_context(|| format!("cannot list {}", dir.display()))?,
    };
    let mut strays = Vec::new();
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if ours(&entry.file_name()) || owned.iter().any(|owned| owned.starts_with(&path)) {
            continue;
        }
        // An empty directory shows nothing (the installer creates
        // `<state>/<instance>` beside a journal kept directly in `<state>`).
        let empty_dir = entry.file_type().is_ok_and(|kind| kind.is_dir())
            && std::fs::read_dir(&path).is_ok_and(|mut entries| entries.next().is_none());
        if !empty_dir {
            strays.push(path);
        }
    }
    Ok(strays)
}

/// Removes rtao write temporaries (`.journal-write-*`) that a process killed
/// between creating and renaming one left in the journal's directory.
/// Without this, every later process that is PID 1 of its namespace (every
/// sandboxed start) fails its journal write with the same number with
/// "File exists". Runs only while holding the journal's lifetime lock (so no
/// live writer of this journal exists) and the lock of every other journal
/// in the directory (a retired journal's lock is free; a live writer of
/// another journal could own a temporary, so then nothing is removed and an
/// error names it). Returns how many it removed. The locks are released
/// before returning, for rtao's own open.
pub fn sweep_journal_temps(journal: &Path) -> Result<usize> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let (dir, name) = journal_parts(journal)?;
    let mut temps = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))? {
        let entry = entry?;
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(JOURNAL_TEMP_PREFIX.as_bytes())
            && entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            temps.push(entry.path());
        }
    }
    if temps.is_empty() {
        return Ok(0);
    }
    let mut lock_name = name.to_os_string();
    lock_name.push(".lock");
    let lock_path = dir.join(lock_name);
    // As rtao does, refuse a lock that is not a regular file.
    if let Ok(meta) = std::fs::symlink_metadata(&lock_path) {
        ensure!(
            meta.file_type().is_file(),
            "the journal lock {} is not a regular file",
            lock_path.display()
        );
    }
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .context("cannot open the journal lock")?;
    if lock.try_lock().is_err() {
        // Held by a running validator: its own open reports that.
        return Ok(0);
    }
    let mut others = Vec::new();
    for other in foreign_locks(journal)? {
        let taken = take_lock(&other).with_context(|| {
            format!(
                "{} journal temporaries kept: another journal in {} may be open",
                temps.len(),
                dir.display()
            )
        })?;
        others.extend(taken);
    }
    for temp in &temps {
        std::fs::remove_file(temp)
            .with_context(|| format!("cannot remove {}", temp.display()))?;
    }
    std::fs::File::open(dir)?.sync_all()?;
    drop(others);
    drop(lock);
    Ok(temps.len())
}

/// What [`guard`] found.
#[derive(Debug, PartialEq, Eq)]
pub enum Guarded {
    /// Not in a sandbox: not PID 1 and no [`SANDBOX_ENV`].
    NotSandboxed,
    /// [`SANDBOX_ENV`] matches the bind list this configuration names.
    Matches,
    /// PID 1 with no [`SANDBOX_ENV`]: a drop-in from an earlier build, or a
    /// sandbox this crate did not render. Nothing could be compared.
    Unverified,
}

/// The startup check inside a sandbox. `marker` is [`SANDBOX_ENV`]'s value,
/// `pid_one` whether this process is PID 1 of its namespace, `unit` the
/// unit the command runs as (`None` for `check-config`, which takes it from
/// the marker) and `binary` the path the validator was started as.
/// Refuses when the journal's directory holds another journal that a running
/// process has open while this process is PID 1 (both would name their write
/// temporaries alike; a retired journal's free lock is only reported, by
/// the caller, as `journal_dir_shared`), and
/// when the marker names another unit or a bind list other than the one
/// [`binds`] derives from this configuration now (a configured path changed
/// without re-running the installer).
pub fn guard(
    config: &ValidatorConfig,
    config_path: &Path,
    binary: &Path,
    home: &Path,
    unit: Option<UnitKind>,
    marker: Option<&str>,
    pid_one: bool,
) -> Result<Guarded> {
    if pid_one {
        let (held, _) = foreign_journal_locks(&config.journal_path)?;
        ensure!(
            held.is_empty(),
            "the journal directory also holds another journal that a running process has open \
             ({}); inside a sandbox every validator is PID 1, so journals sharing a directory \
             collide on rtao's write temporaries. Give each instance a directory of its own \
             (~/.local/state/slop_ninja_validator/<instance>/) and re-run deploy/install.sh",
            held.iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let Some(marker) = marker else {
        return Ok(if pid_one {
            Guarded::Unverified
        } else {
            Guarded::NotSandboxed
        });
    };
    let (name, digest) = marker
        .split_once(':')
        .with_context(|| format!("{SANDBOX_ENV} is malformed"))?;
    let marked = UnitKind::from_name(name)
        .with_context(|| format!("{SANDBOX_ENV} names an unknown unit"))?;
    if let Some(unit) = unit {
        ensure!(
            unit == marked,
            "the sandbox drop-in was rendered for the {} unit, not the {} unit; re-run deploy/install.sh",
            marked.name(),
            unit.name()
        );
    }
    let now = binds(config, config_path, binary, marked, home).context(
        "the sandbox no longer matches the configuration (a configured path changed, or the \
         drop-in does not bind it); re-run deploy/install.sh",
    )?;
    ensure!(
        binds_digest(&now) == digest,
        "the sandbox drop-in binds other paths than this configuration names (a configured \
         path changed since the installer rendered it); re-run deploy/install.sh, which \
         renders and tests a new drop-in"
    );
    Ok(Guarded::Matches)
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
/// `marker` is [`SANDBOX_ENV`]'s value; `env` adds variables (names from
/// [`PASSED_ENV`]).
fn sandbox_words(
    bwrap: &Path,
    layout: &Layout,
    binds: &[Bind],
    marker: &str,
    env: &[(&str, String)],
) -> Vec<Word> {
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
        lit("--setenv"),
        lit(SANDBOX_ENV),
        lit(marker),
    ];
    for (name, value) in env {
        words.extend([lit("--setenv"), lit(*name), lit(value.clone())]);
    }
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
    // After every bind (bubblewrap creates their mount points on the root):
    // a write anywhere but a read-write bind, /tmp or /dev now fails.
    words.extend([
        lit("--remount-ro"),
        lit("/"),
        lit("--chdir"),
        lit("/"),
        lit("--"),
    ]);
    words
}

/// The only variables a drop-in passes besides `HOME`, `RUST_BACKTRACE` and
/// [`SANDBOX_ENV`]: the units clear the environment, so a unit's
/// `Environment=` and the user manager's variables never reach the
/// validator.
pub const PASSED_ENV: &[&str] = &[crate::faults::DRILL_FAULT_ENV];

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
/// that sets neither. `env` passes extra variables, each named in
/// [`PASSED_ENV`] (a testnet drill's fault).
pub fn render_drop_in(
    bwrap: &Path,
    layout: &Layout,
    binds: &[Bind],
    binary: &Path,
    config_path: &Path,
    kind: UnitKind,
    env: &[(&str, String)],
) -> Result<String> {
    check_path(bwrap, "the bwrap path")?;
    ensure!(
        unit_word(&path_word(bwrap))? == bwrap.to_string_lossy(),
        "the bwrap path must be plain (letters, digits and /._-+=,:@)"
    );
    for (name, value) in env {
        ensure!(
            PASSED_ENV.contains(name),
            "{name} is not a variable the sandbox passes"
        );
        ensure!(
            !value.is_empty() && !value.chars().any(char::is_control),
            "{name} must be a non-empty value without control characters"
        );
    }
    let mut words = sandbox_words(bwrap, layout, binds, &marker(kind, binds), env);
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
        for (path, role) in &bind.covers {
            text.push_str(&format!(
                "#     {access} through it: {} ({role})\n",
                path.to_string_lossy().replace('\n', " "),
            ));
        }
    }
    text.push_str(
        "# Everything else on the sandbox's root is read-only. Only HOME,\n\
         # RUST_BACKTRACE and SLOP_NINJA_SANDBOX_BINDS (this bind list's digest,\n\
         # checked at startup) reach the validator: a unit's Environment= does not.\n",
    );
    for (name, _) in env {
        text.push_str(&format!("# Also passed: {name}\n"));
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
    /// The host directories whose entries were tested.
    pub hidden_dirs: Vec<PathBuf>,
    /// Each directory bound whole, with its access and how many entries
    /// it shows (all of them visible inside the sandbox).
    pub dir_binds: Vec<DirBindReport>,
    pub journal_exists: bool,
}

#[derive(Debug, Serialize)]
pub struct DirBindReport {
    pub path: PathBuf,
    pub access: Access,
    pub entries: usize,
}

/// Runs `check-config` and a visibility test inside the sandbox the drop-in
/// describes (the same bubblewrap options, [`SANDBOX_ENV`] included, so
/// `check-config` also runs [`guard`] there): every bound path must be
/// readable (directories bound read-write also writable), and every other
/// host entry must be absent of each directory above a bound path (from `/`
/// down, other wallets and keys beside a bound hotkey included), of the
/// home directory, of `/tmp` and of `/run` (and of each directory above the
/// resolver's directory there, so `/run/user` too). `/proc` and `/dev` are
/// the sandbox's own and are not compared. Reads no key and no chain.
#[allow(clippy::too_many_arguments)]
pub fn check(
    bwrap: &Path,
    layout: &Layout,
    binds: &[Bind],
    binary: &Path,
    config_path: &Path,
    home: &Path,
    kind: UnitKind,
    journal_exists: bool,
) -> Result<CheckReport> {
    let sandbox = sandbox_words(bwrap, layout, binds, &marker(kind, binds), &[]);
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
    // Every directory above a bound path or the resolver's directory, the
    // home directory, the host's /tmp and /run.
    let mut scanned: Vec<&Path> = bound
        .iter()
        .copied()
        .chain(layout.resolver_dir.as_deref())
        .flat_map(|path| path.ancestors().skip(1))
        .collect();
    scanned.extend([home, Path::new("/tmp"), Path::new("/run")]);
    scanned.sort();
    scanned.dedup();
    let mut hidden = Vec::new();
    let mut hidden_dirs = Vec::new();
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
        hidden_dirs.push(dir.to_path_buf());
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
    let dir_binds = binds
        .iter()
        .filter(|bind| bind.dir)
        .map(|bind| DirBindReport {
            path: bind.path.clone(),
            access: bind.access,
            entries: std::fs::read_dir(&bind.path).map_or(0, |entries| entries.count()),
        })
        .collect();
    Ok(CheckReport {
        binds: binds.len(),
        hidden_checked: hidden.len(),
        hidden_dirs,
        dir_binds,
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
