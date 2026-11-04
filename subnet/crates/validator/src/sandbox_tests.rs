//! The sandbox drop-in's bind list and its unit-file rendering. The live
//! bubblewrap run is `tests/sandbox.rs`.

use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::ValidatorConfig;
use crate::sandbox::{
    Access, Bind, Guarded, Layout, RootEntry, SANDBOX_ENV, UnitKind, binds, binds_digest, guard,
    journal_dir_strays, render_drop_in, sweep_journal_temps,
};

fn scratch() -> Result<tempfile::TempDir> {
    let root = std::env::var_os("SLOP_NINJA_TEST_TMPDIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    Ok(tempfile::Builder::new()
        .prefix("sandbox-")
        .tempdir_in(root)?)
}

/// An operator's home with the standard directories, a wallet holding the
/// validator's hotkey beside other keys, and a configuration text whose
/// paths point into it. `extra` is appended to the configuration.
struct Home {
    _root: tempfile::TempDir,
    home: PathBuf,
    state: PathBuf,
    hotkeys: PathBuf,
    config_path: PathBuf,
    binary: PathBuf,
}

impl Home {
    fn new() -> Result<Self> {
        let root = scratch()?;
        let home = root.path().join("home");
        let state = home.join(".local/state/slop_ninja_validator");
        let hotkeys = home.join(".bittensor/wallets/w/hotkeys");
        let config_dir = home.join(".config/slop_ninja_validator");
        let bin = home.join(".local/bin");
        for dir in [&state, &hotkeys, &config_dir, &bin] {
            std::fs::create_dir_all(dir)?;
        }
        for file in ["hk", "hkpub.txt", "other", "otherpub.txt"] {
            std::fs::write(hotkeys.join(file), b"synthetic")?;
        }
        std::fs::write(hotkeys.parent().unwrap().join("coldkey"), b"synthetic")?;
        let binary = bin.join("slop_ninja_validator");
        std::fs::write(&binary, b"")?;
        Ok(Self {
            config_path: config_dir.join("v.toml"),
            _root: root,
            home,
            state,
            hotkeys,
            binary,
        })
    }

    fn config(&self, extra: &str) -> Result<ValidatorConfig> {
        let text = format!(
            "network = \"test\"\nnetuid = 601\nweight_version = 0\njournal_path = \"{}\"\n\
             [wallet]\npath = \"{}\"\nname = \"w\"\nhotkey = \"hk\"\n\
             [rpc]\nendpoints = [\"wss://test.finney.opentensor.ai:443\"]\n{extra}",
            self.state.join("journal.json").display(),
            self.home.join(".bittensor/wallets").display(),
        );
        std::fs::write(&self.config_path, &text)?;
        ValidatorConfig::parse(&text)
    }

    fn binds(&self, config: &ValidatorConfig, kind: UnitKind) -> Result<Vec<Bind>> {
        binds(config, &self.config_path, &self.binary, kind, &self.home)
    }
}

fn package(path: &Path) -> String {
    let keys = slop_ninja_protocol::fixtures::trusted_keys();
    let k = |role| hex::encode(keys.key(role));
    format!(
        "[package]\npath = \"{}\"\n[package.trusted_keys]\npackage = \"{}\"\nreceipt = \"{}\"\nauthorization = \"{}\"\n",
        path.display(),
        k(slop_ninja_protocol::KeyRole::Package),
        k(slop_ninja_protocol::KeyRole::Receipt),
        k(slop_ninja_protocol::KeyRole::Authorization),
    )
}

fn listed(binds: &[Bind]) -> Vec<(PathBuf, Access)> {
    binds
        .iter()
        .map(|bind| (bind.path.clone(), bind.access))
        .collect()
}

/// The dispatch unit sees its binary, configuration, the two hotkey files
/// and the state directory (read-write: the journal, the fetched bundle and
/// the cache all live there), and nothing else: not the wallet directory,
/// the coldkey or another hotkey. The dry-run unit never sees the keyfile.
#[test]
fn the_binds_are_exactly_the_configured_paths() -> Result<()> {
    let h = Home::new()?;
    let extra = format!(
        "{}[dispatch]\nhotkey_file = \"{}\"\n[fetch]\nbase_url = \"https://status.example/\"\n",
        package(&h.state.join("package.json")),
        h.hotkeys.join("hk").display()
    );
    let config = h.config(&extra)?;
    let dispatch = h.binds(&config, UnitKind::Dispatch)?;
    let mut expected = vec![
        (h.config_path.clone(), Access::Read),
        (h.binary.clone(), Access::Read),
        (h.state.clone(), Access::Write),
        (h.hotkeys.join("hk"), Access::Read),
        (h.hotkeys.join("hkpub.txt"), Access::Read),
    ];
    let mut got = listed(&dispatch);
    expected.sort();
    got.sort();
    assert_eq!(got, expected);
    // A directory precedes everything beneath it.
    for (i, bind) in dispatch.iter().enumerate() {
        assert!(
            !dispatch[..i]
                .iter()
                .any(|earlier| earlier.path.starts_with(&bind.path) && earlier.path != bind.path),
            "{} is bound after a path inside it",
            bind.path.display()
        );
    }
    let dry = h.binds(&config, UnitKind::DryRun)?;
    assert!(!dry.iter().any(|bind| bind.path == h.hotkeys.join("hk")));
    assert!(
        dry.iter()
            .any(|bind| bind.path == h.hotkeys.join("hkpub.txt"))
    );
    for bind in dispatch.iter().chain(&dry) {
        for hidden in ["other", "otherpub.txt"] {
            assert_ne!(bind.path, h.hotkeys.join(hidden));
        }
        assert!(!h.hotkeys.starts_with(&bind.path) || !bind.dir);
    }
    Ok(())
}

/// Without the fetcher the package directory is bound read-only (the
/// operator replaces the bundle there); a fetch cache, a local mirror and a
/// runtime cache outside the state directory get their own binds, the
/// mirror read-only.
#[test]
fn package_mirror_and_caches_get_their_own_access() -> Result<()> {
    let h = Home::new()?;
    let bundles = h.home.join("bundles");
    let mirror = h.state.join("public-mirror");
    let cache = h.home.join("cache");
    let runtime = h.home.join("runtime");
    for dir in [&bundles, &mirror, &cache, &runtime] {
        std::fs::create_dir_all(dir)?;
    }
    let config = h.config(&package(&bundles.join("active.json")).replace(
        "[package]\n",
        &format!("runtime_cache_dir = \"{}\"\n[package]\n", runtime.display()),
    ))?;
    let got = listed(&h.binds(&config, UnitKind::DryRun)?);
    assert!(got.contains(&(bundles.clone(), Access::Read)), "{got:?}");
    assert!(got.contains(&(runtime.clone(), Access::Write)), "{got:?}");

    let fetching = format!(
        "{}[fetch]\nbase_url = \"file://{}/\"\ncache_dir = \"{}\"\n",
        package(&bundles.join("active.json")),
        mirror.display(),
        cache.display()
    );
    let got = listed(&h.binds(&h.config(&fetching)?, UnitKind::DryRun)?);
    assert!(got.contains(&(bundles.clone(), Access::Write)), "{got:?}");
    assert!(got.contains(&(cache.clone(), Access::Write)), "{got:?}");
    // The mirror inside the read-write state directory needs no bind.
    assert!(!got.iter().any(|(path, _)| path == &mirror), "{got:?}");

    let elsewhere = h.home.join("mirror");
    std::fs::create_dir_all(&elsewhere)?;
    let got = listed(&h.binds(
        &h.config(&fetching.replace(
            &mirror.display().to_string(),
            &elsewhere.display().to_string(),
        ))?,
        UnitKind::DryRun,
    )?);
    assert!(got.contains(&(elsewhere, Access::Read)), "{got:?}");
    Ok(())
}

/// A directory bind that would show the home directory (or an ancestor),
/// a required path that does not exist, and a path with `..` are refused.
#[test]
fn binds_refuse_paths_that_would_expose_the_home_or_vanish() -> Result<()> {
    let h = Home::new()?;
    // A journal directly in the home directory: its directory is the home.
    let text = std::fs::read_to_string({
        h.config("")?;
        &h.config_path
    })?;
    let in_home = ValidatorConfig::parse(&text.replace(
        &h.state.join("journal.json").display().to_string(),
        &h.home.join("journal.json").display().to_string(),
    ))?;
    let error = h.binds(&in_home, UnitKind::DryRun).unwrap_err().to_string();
    assert!(error.contains("home directory"), "{error}");

    let at_top = h.config(&package(Path::new("/package.json")))?;
    assert!(h.binds(&at_top, UnitKind::DryRun).is_err());

    let missing = h.config(&package(&h.home.join("absent/package.json")))?;
    let error = h.binds(&missing, UnitKind::DryRun).unwrap_err().to_string();
    assert!(error.contains("not an existing directory"), "{error}");

    let dotted = h.config(&package(&h.state.join("../state/package.json")))?;
    let error = h.binds(&dotted, UnitKind::DryRun).unwrap_err().to_string();
    assert!(error.contains(".."), "{error}");

    // The dispatch keyfile must exist; its companion is optional.
    let keyfile = |name: &str| {
        format!(
            "[dispatch]\nhotkey_file = \"{}\"\n",
            h.hotkeys.join(name).display()
        )
    };
    let config = h.config(&format!(
        "{}{}",
        package(&h.state.join("p.json")),
        keyfile("absent")
    ))?;
    assert!(h.binds(&config, UnitKind::Dispatch).is_err());
    assert!(h.binds(&config, UnitKind::DryRun).is_ok());
    std::fs::write(h.hotkeys.join("lone"), b"synthetic")?;
    let config = h.config(&format!(
        "{}{}",
        package(&h.state.join("p.json")),
        keyfile("lone")
    ))?;
    let got = listed(&h.binds(&config, UnitKind::Dispatch)?);
    assert!(got.contains(&(h.hotkeys.join("lone"), Access::Read)));
    assert!(
        !got.iter()
            .any(|(path, _)| path == &h.hotkeys.join("lonepub.txt"))
    );
    Ok(())
}

fn layout() -> Layout {
    Layout {
        entries: vec![
            RootEntry::ReadOnly(PathBuf::from("/usr")),
            RootEntry::ReadOnly(PathBuf::from("/etc")),
            RootEntry::Symlink {
                target: PathBuf::from("usr/bin"),
                link: PathBuf::from("/bin"),
            },
        ],
        resolver_dir: Some(PathBuf::from("/run/systemd/resolve")),
    }
}

/// The drop-in resets `ExecStart=` and runs the unit's own command inside
/// bubblewrap, with the system layout, private /proc, /dev and /tmp, then
/// each bind; it widens the namespace and system-call allow-lists only by
/// what bubblewrap needs. Paths are quoted and escaped for systemd
/// (`\`, `"`, `%`, `$`); control characters and a non-plain bwrap path are
/// refused.
#[test]
fn the_drop_in_quotes_paths_and_resets_exec_start() -> Result<()> {
    let odd = PathBuf::from("/home/op/my state/100%$HOME\"x\\y");
    let binds = vec![
        Bind {
            path: PathBuf::from("/home/op/.config/slop_ninja_validator/v.toml"),
            access: Access::Read,
            dir: false,
            role: "the configuration",
            covers: Vec::new(),
        },
        Bind {
            path: odd.clone(),
            access: Access::Write,
            dir: true,
            role: "the journal directory",
            covers: vec![(odd.join("bundles"), "the package directory")],
        },
    ];
    let binary = Path::new("/home/op/.local/bin/slop_ninja_validator");
    let config = Path::new("/home/op/.config/slop_ninja_validator/v.toml");
    let text = render_drop_in(
        Path::new("/usr/bin/bwrap"),
        &layout(),
        &binds,
        binary,
        config,
        UnitKind::Dispatch,
        &[],
    )?;
    let lines: Vec<&str> = text.lines().collect();
    let service = lines.iter().position(|line| *line == "[Service]").unwrap();
    assert_eq!(
        &lines[service..service + 7],
        &[
            "[Service]",
            "RestrictNamespaces=",
            "RestrictNamespaces=user mnt pid ipc uts cgroup",
            "SystemCallFilter=",
            "SystemCallFilter=@system-service @mount",
            "SystemCallErrorNumber=EPERM",
            "ExecStart=",
        ]
    );
    let exec = lines[service + 7];
    assert_eq!(lines.len(), service + 8, "ExecStart is the last line");
    let marker = format!("dispatch:{}", binds_digest(&binds));
    assert!(exec.starts_with(&format!(
        "ExecStart=/usr/bin/bwrap --unshare-all --unshare-user --share-net --disable-userns --as-pid-1 \
         --new-session --clearenv --setenv HOME %h --setenv RUST_BACKTRACE 0 \
         --setenv {SANDBOX_ENV} {marker} \
         --ro-bind /usr /usr --ro-bind /etc /etc --symlink usr/bin /bin \
         --ro-bind-try /run/systemd/resolve /run/systemd/resolve --proc /proc --dev /dev --tmpfs /tmp "
    )), "{exec}");
    let quoted = "\"/home/op/my state/100%%$$HOME\\\"x\\\\y\"";
    assert!(
        exec.contains(&format!("--bind {quoted} {quoted} ")),
        "{exec}"
    );
    assert!(exec.contains(
        "--ro-bind /home/op/.config/slop_ninja_validator/v.toml /home/op/.config/slop_ninja_validator/v.toml "
    ));
    assert!(
        exec.ends_with(
            " --remount-ro / --chdir / -- /home/op/.local/bin/slop_ninja_validator --config \
         /home/op/.config/slop_ninja_validator/v.toml run --dispatch"
        ),
        "{exec}"
    );
    // Every bind is listed in the header comment, before [Service], with
    // the configured paths it covers and the access they get through it.
    assert!(
        lines[..service]
            .iter()
            .any(|line| line.contains("read-write") && line.contains("my state"))
    );
    assert!(
        lines[..service].iter().any(|line| line.contains("read-write through it")
            && line.contains("bundles (the package directory)")),
        "{text}"
    );
    assert!(!text.contains("SLOP_NINJA_DRILL_FAULT"));
    // A drill passes its fault explicitly; no other variable is accepted.
    let drill = render_drop_in(
        Path::new("/usr/bin/bwrap"),
        &layout(),
        &binds,
        binary,
        config,
        UnitKind::Dispatch,
        &[("SLOP_NINJA_DRILL_FAULT", "submitted:2".to_owned())],
    )?;
    assert!(drill.contains(" --setenv SLOP_NINJA_DRILL_FAULT submitted:2 "), "{drill}");
    assert!(drill.contains("# Also passed: SLOP_NINJA_DRILL_FAULT"));
    assert!(
        render_drop_in(
            Path::new("/usr/bin/bwrap"),
            &layout(),
            &binds,
            binary,
            config,
            UnitKind::Dispatch,
            &[("HTTPS_PROXY", "http://proxy".to_owned())],
        )
        .is_err()
    );

    let dry = render_drop_in(
        Path::new("/usr/bin/bwrap"),
        &layout(),
        &binds,
        binary,
        config,
        UnitKind::DryRun,
        &[],
    )?;
    assert!(
        dry.trim_end()
            .ends_with("--config /home/op/.config/slop_ninja_validator/v.toml run")
    );

    let mut control = binds.clone();
    control[1].path = PathBuf::from("/home/op/a\nb");
    assert!(
        render_drop_in(
            Path::new("/usr/bin/bwrap"),
            &layout(),
            &control,
            binary,
            config,
            UnitKind::Dispatch,
            &[]
        )
        .is_err()
    );
    assert!(
        render_drop_in(
            Path::new("/opt/my bwrap"),
            &layout(),
            &binds,
            binary,
            config,
            UnitKind::Dispatch,
            &[]
        )
        .is_err()
    );
    assert!(
        render_drop_in(
            Path::new("bwrap"),
            &layout(),
            &binds,
            binary,
            config,
            UnitKind::Dispatch,
            &[]
        )
        .is_err()
    );
    Ok(())
}

/// The host layout always binds /usr and /etc, reproduces each /bin-style
/// entry as what it is, and finds the resolver's runtime directory only
/// under /run.
#[test]
fn the_host_layout_mirrors_the_root() -> Result<()> {
    let layout = Layout::host()?;
    assert_eq!(
        layout.entries[0],
        RootEntry::ReadOnly(PathBuf::from("/usr"))
    );
    assert_eq!(
        layout.entries[1],
        RootEntry::ReadOnly(PathBuf::from("/etc"))
    );
    for entry in &layout.entries[2..] {
        match entry {
            RootEntry::Symlink { link, target } => {
                assert_eq!(&std::fs::read_link(link)?, target);
            }
            RootEntry::ReadOnly(path) => assert!(path.is_dir()),
        }
    }
    if let Some(dir) = &layout.resolver_dir {
        assert!(dir.starts_with("/run"));
    }
    Ok(())
}

/// The journal's directory is bound read-write as a whole, so anything in it
/// that is not this instance's journal family, package or caches is a
/// stray: another instance's journal and lock, an old package, key files.
#[test]
fn the_journal_directory_holds_only_this_instance() -> Result<()> {
    let h = Home::new()?;
    let extra = format!(
        "{}[fetch]\nbase_url = \"https://status.example/\"\n",
        package(&h.state.join("package.json")),
    );
    let config = h.config(&extra)?;
    for name in [
        "journal.json",
        "journal.json.lock",
        "journal.json.reveal-rows.json",
        "journal.json.held-target.json.reset-1",
        "journal.json.revocations.json.lock",
        ".journal-write-1-0",
        "package.json",
        "package.json.next",
    ] {
        std::fs::write(h.state.join(name), b"")?;
    }
    std::fs::create_dir(h.state.join("public-cache"))?;
    // The installer's empty per-instance directory beside it shows nothing.
    std::fs::create_dir(h.state.join("v2"))?;
    assert_eq!(journal_dir_strays(&config)?, Vec::<PathBuf>::new());
    // A name that only starts like the journal's is not in its family.
    for name in ["journal.json-old", "other-journal.json.lock", "package-t1.json"] {
        std::fs::write(h.state.join(name), b"")?;
    }
    std::fs::create_dir(h.state.join("keys"))?;
    std::fs::write(h.state.join("keys/trusted_keys.toml"), b"")?;
    let strays = journal_dir_strays(&config)?;
    let names: Vec<_> = strays
        .iter()
        .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names,
        ["journal.json-old", "keys", "other-journal.json.lock", "package-t1.json"]
    );

    // A package in a subdirectory of the journal's directory owns that
    // subdirectory, and so do the caches.
    let h = Home::new()?;
    std::fs::create_dir_all(h.state.join("bundles"))?;
    std::fs::create_dir_all(h.state.join("runtime"))?;
    let config = h.config(
        &package(&h.state.join("bundles/active.json")).replace(
            "[package]\n",
            &format!(
                "runtime_cache_dir = \"{}\"\n[package]\n",
                h.state.join("runtime").display()
            ),
        ),
    )?;
    assert_eq!(journal_dir_strays(&config)?, Vec::<PathBuf>::new());
    Ok(())
}

/// A write temporary a killed process left is removed under the journal's
/// lock, and only when no other journal shares the directory and no running
/// validator holds this one's lock.
#[test]
fn stale_journal_temporaries_are_swept_only_when_safe() -> Result<()> {
    let h = Home::new()?;
    let journal = h.state.join("journal.json");
    assert_eq!(sweep_journal_temps(&journal)?, 0);
    let temps = [".journal-write-1-0", ".journal-write-1-3"];
    for name in temps {
        std::fs::write(h.state.join(name), b"")?;
    }
    // Another journal's lock beside it: its writer could own a temporary.
    std::fs::write(h.state.join("other.json.lock"), b"")?;
    assert_eq!(sweep_journal_temps(&journal)?, 0);
    assert!(h.state.join(temps[0]).exists());
    std::fs::remove_file(h.state.join("other.json.lock"))?;
    // This journal's lock held (a running validator): nothing removed.
    let held = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(h.state.join("journal.json.lock"))?;
    held.lock()?;
    assert_eq!(sweep_journal_temps(&journal)?, 0);
    assert!(h.state.join(temps[1]).exists());
    drop(held);
    assert_eq!(sweep_journal_temps(&journal)?, 2);
    for name in temps {
        assert!(!h.state.join(name).exists());
    }
    // The lock is released again for rtao's own open.
    let lock = std::fs::File::open(h.state.join("journal.json.lock"))?;
    assert!(lock.try_lock().is_ok());
    Ok(())
}

/// Inside a sandbox the startup check accepts the drop-in's marker only for
/// the bind list the configuration names now and for the unit it was
/// rendered for, and refuses a journal directory shared with another
/// journal while PID 1.
#[test]
fn the_startup_guard_compares_the_drop_in_with_the_configuration() -> Result<()> {
    let h = Home::new()?;
    let config = h.config(&package(&h.state.join("package.json")))?;
    let binds = h.binds(&config, UnitKind::DryRun)?;
    let marker = format!("dry-run:{}", binds_digest(&binds));
    let check = |config: &ValidatorConfig, unit, marker: Option<&str>, pid_one| {
        guard(config, &h.config_path, &h.binary, &h.home, unit, marker, pid_one)
    };
    assert_eq!(check(&config, None, None, false)?, Guarded::NotSandboxed);
    assert_eq!(check(&config, None, None, true)?, Guarded::Unverified);
    assert_eq!(check(&config, None, Some(&marker), true)?, Guarded::Matches);
    assert_eq!(
        check(&config, Some(UnitKind::DryRun), Some(&marker), true)?,
        Guarded::Matches
    );
    let error = check(&config, Some(UnitKind::Dispatch), Some(&marker), true).unwrap_err();
    assert!(error.to_string().contains("rendered for the dry-run unit"), "{error}");
    for bad in ["dry-run", "nonsense:00", "dry-run:00"] {
        assert!(check(&config, None, Some(bad), true).is_err(), "{bad}");
    }
    // A configured path that moved since the drop-in was rendered.
    let runtime = h.home.join("runtime");
    std::fs::create_dir_all(&runtime)?;
    let moved = h.config(&package(&h.state.join("package.json")).replace(
        "[package]\n",
        &format!("runtime_cache_dir = \"{}\"\n[package]\n", runtime.display()),
    ))?;
    let error = check(&moved, None, Some(&marker), true).unwrap_err();
    assert!(error.to_string().contains("re-run deploy/install.sh"), "{error}");
    // ... and one the sandbox does not even show.
    let unbound = h.config(&package(&h.home.join("absent/package.json")))?;
    let error = format!("{:#}", check(&unbound, None, Some(&marker), true).unwrap_err());
    assert!(error.contains("no longer matches"), "{error}");
    // Another journal's lock in the directory: refused as PID 1 only.
    std::fs::write(h.state.join("other.json.lock"), b"")?;
    let error = check(&config, None, Some(&marker), true).unwrap_err();
    assert!(error.to_string().contains("another journal"), "{error}");
    assert_eq!(check(&config, None, None, false)?, Guarded::NotSandboxed);
    Ok(())
}
