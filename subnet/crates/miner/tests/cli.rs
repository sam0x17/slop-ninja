//! The `slop_ninja_miner` binary against the local mock: argument parsing,
//! wallet hotkey loading, exit codes, the trust file that pins the
//! coordinator, `next-cohort` and `status` from the planned public snapshot,
//! a resumed `upload`, a refused second artifact shown by `status`, `abort` by
//! id prefix or cohort, and the choice of opening. The commitment is prepared
//! through the library (the `commit` command needs a chain) and then placed
//! on the mock's chain table; nothing is submitted anywhere.

mod support;

use std::path::Path;
use std::process::{Command, Output};

use rtao_primitives::CryptoType;
use rtao_signer::KeyView as _;
use rtao_wallet::{RegenerateFrom, SecretString, Wallet, WriteOptions};
use slop_ninja_artifact_check::spec;
use slop_ninja_miner::opening::{OsNonce, Store};
use slop_ninja_miner::{commit, object};
use slop_ninja_protocol::{AccountId, NeuronIdentity};
use support::common::{chain601, config};
use support::mock::Mock;

const BIN: &str = env!("CARGO_BIN_EXE_slop_ninja_miner");

async fn run(args: Vec<String>) -> Output {
    tokio::task::spawn_blocking(move || {
        Command::new(BIN)
            .args(&args)
            .env_remove("SLOP_NINJA_COORDINATOR")
            .env_remove("SLOP_NINJA_PUBLIC")
            .env_remove("SLOP_NINJA_TRUST")
            .env_remove("SLOP_NINJA_MINER_HOME")
            .output()
            .unwrap()
    })
    .await
    .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn p(path: &Path) -> &str {
    path.to_str().unwrap()
}

fn with(mut base: Vec<String>, more: &[&str]) -> Vec<String> {
    base.extend(more.iter().map(|s| (*s).to_owned()));
    base
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_pack_check_upload_and_status() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("tok")).unwrap();
    std::fs::write(src.join("model.onnx"), vec![7u8; 2_500]).unwrap();
    std::fs::write(src.join("tok/tokenizer.json"), b"{\"t\":1}").unwrap();
    std::fs::write(src.join("owner.json"), b"{\"o\":1}").unwrap();
    std::fs::write(src.join("owner2.json"), b"{\"o\":2}").unwrap();
    let object_path = dir.path().join("a.sna");
    let state = dir.path().join("state");

    // A test wallet hotkey (a dev key; plaintext like btcli hotkeys).
    let wallets = dir.path().join("wallets");
    let wallet = Wallet::new("miner", "h1", &wallets).unwrap();
    wallet
        .regenerate_hotkey(
            RegenerateFrom::Uri(SecretString::new("//Alice".to_owned())),
            CryptoType::Sr25519,
            WriteOptions::hotkey(),
        )
        .await
        .unwrap();
    let hotkey = AccountId(wallet.hotkey_file().hotkey_address().unwrap().0);
    let wallet_args = [
        "--wallet-path",
        p(&wallets),
        "--wallet",
        "miner",
        "--hotkey",
        "h1",
    ];

    // pack: the contract defaults to the artifact spec's; the object is 0600.
    let pack_args = |out: &Path, owner: &str| {
        args(&[
            "pack",
            "--root",
            p(&src),
            "--file",
            "graph:model.onnx",
            "--file",
            "tokenizer:tok/tokenizer.json",
            "--file",
            &format!("owner-metadata:{owner}"),
            "--out",
            p(out),
        ])
    };
    let packed = run(pack_args(&object_path, "owner.json")).await;
    assert!(packed.status.success(), "{}", text(&packed));
    assert!(text(&packed).contains(&format!("contract {}", spec::CONTRACT_ID)));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(&object_path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600);
    let artifact = object::verify_file(&object_path).unwrap();

    let mock = Mock::start(config(), 1_950).await;
    let url = mock.url();
    let origin = url.trim_end_matches('/').to_owned();

    // check runs the artifact spec: this synthetic object is refused (1) with
    // the spec's code, and so is another contract than the cohort's.
    let checked = run(args(&["check", p(&object_path), "--coordinator", &url])).await;
    assert_eq!(checked.status.code(), Some(1), "{}", text(&checked));
    assert!(
        text(&checked).contains("undeclared_file"),
        "{}",
        text(&checked)
    );
    let refused = run(args(&["check", p(&object_path), "--contract", "other"])).await;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(text(&refused).contains("contract_mismatch"));
    // A directory with the spec's six files packs under the spec's table;
    // its synthetic tokenizer is refused by the byte match.
    let model = dir.path().join("model");
    std::fs::create_dir_all(&model).unwrap();
    for role in &spec::ROLES {
        std::fs::write(model.join(role.name), role.name).unwrap();
    }
    let six = dir.path().join("six.sna");
    let packed = run(args(&["pack", "--dir", p(&model), "--out", p(&six)])).await;
    assert!(packed.status.success(), "{}", text(&packed));
    let checked = run(args(&["check", p(&six)])).await;
    assert_eq!(checked.status.code(), Some(1), "{}", text(&checked));
    assert!(
        text(&checked).contains("tokenizer_mismatch"),
        "{}",
        text(&checked)
    );

    // next-cohort reads the public snapshot, labelled unsigned.
    let next = run(args(&["next-cohort", "--public", &url])).await;
    assert!(next.status.success(), "{}", text(&next));
    let out = text(&next);
    assert!(
        out.contains("unsigned") && out.contains("admitting cohort 7"),
        "{out}"
    );
    assert!(out.contains("planned cutoff at epoch 23170 (1 epoch after the confirmed epoch)"));
    assert!(
        out.contains("opened at epoch 23169 (the confirmed epoch)"),
        "{out}"
    );

    // The commitment, prepared in the same state directory and finalized on the mock chain.
    let identity = NeuronIdentity {
        uid: 12,
        hotkey,
        registration_block: 1_012,
    };
    let store = Store::open(&state).unwrap();
    let prepared = commit::prepare(
        &store,
        &chain601(),
        7,
        &identity,
        &artifact,
        &mut OsNonce,
        false,
    )
    .unwrap();
    mock.register(identity);
    mock.commit(hotkey, 2_000, 4, &prepared.payload);

    let upload = with(
        args(&[
            "upload",
            "--artifact",
            p(&object_path),
            "--public",
            &url,
            "--state-dir",
            p(&state),
            "--poll-secs",
            "1",
        ]),
        &wallet_args,
    );
    // The unsigned snapshot alone never chooses where the opening goes.
    let unpinned = run(upload.clone()).await;
    assert!(!unpinned.status.success());
    assert!(text(&unpinned).contains("unsigned"), "{}", text(&unpinned));

    // A trust file that pins the coordinator's origin.
    let trust = dir.path().join("testnet-trust.json");
    std::fs::write(
        &trust,
        serde_json::json!({
            "schema": "slop_ninja_testnet_trust_roots_v1",
            "network": "testnet",
            "netuid": 601,
            "admission_origins": [origin],
        })
        .to_string(),
    )
    .unwrap();
    let upload = with(upload, &["--trust", p(&trust)]);
    // An origin the trust file does not pin is refused even when named.
    let other = run(with(
        upload.clone(),
        &["--coordinator", "http://127.0.0.1:9/"],
    ))
    .await;
    assert!(!other.status.success());
    assert!(
        text(&other).contains("pins other coordinator origins"),
        "{}",
        text(&other)
    );

    // upload, interrupted after one part, then resumed.
    let interrupted = run(with(upload.clone(), &["--stop-after-parts", "1"])).await;
    let out = text(&interrupted);
    assert!(interrupted.status.success(), "{out}");
    assert!(out.contains("stopped after 1 parts"), "{out}");
    assert!(
        out.contains(&format!(
            "to coordinator {origin} (pinned by the trust file)"
        )),
        "{out}"
    );
    let resumed = run(upload.clone()).await;
    let out = text(&resumed);
    assert!(resumed.status.success(), "{out}");
    assert!(out.contains("(attempt 1) in cohort 7: verified"), "{out}");
    let session_id = out
        .split_whitespace()
        .find(|w| w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit()))
        .expect("the full session id is printed")
        .to_owned();

    // A second artifact for the same commitment is refused...
    let second_path = dir.path().join("b.sna");
    let packed = run(pack_args(&second_path, "owner2.json")).await;
    assert!(packed.status.success(), "{}", text(&packed));
    let second = object::verify_file(&second_path).unwrap();
    assert_eq!(second.content, artifact.content);
    let mut second_file = prepared.file.clone();
    second_file.artifact_sha256 = second.sha256.hex();
    second_file.artifact_len = second.len;
    let second_opening = dir.path().join("second-opening.json");
    std::fs::write(&second_opening, serde_json::to_vec(&second_file).unwrap()).unwrap();
    std::fs::set_permissions(&second_opening, std::fs::Permissions::from_mode(0o600)).unwrap();
    let mut second_upload = upload.clone();
    second_upload[2] = p(&second_path).to_owned();
    let refused = run(with(second_upload, &["--opening", p(&second_opening)])).await;
    let out = text(&refused);
    assert!(!refused.status.success(), "{out}");
    assert!(
        out.contains("second_artifact") && out.contains(&session_id),
        "{out}"
    );

    // ...and status shows the refusal, with the live session to abort.
    let status = with(
        args(&[
            "status",
            "--public",
            &url,
            "--trust",
            p(&trust),
            "--state-dir",
            p(&state),
        ]),
        &wallet_args,
    );
    let shown = run(status.clone()).await;
    let out = text(&shown);
    assert!(shown.status.success(), "{out}");
    for needle in [
        "admitting cohort 7",
        "phase admission, mode burn",
        "opening cohort 7",
        &format!("coordinator {origin} (pinned by the trust file)"),
        "registered: uid 12",
        "commitment at block 2000 extrinsic 4 for cohort 7; matches a local opening",
        &format!("session {session_id} (attempt 1) in cohort 7: verified"),
        "last refused request: second_artifact",
        &format!("the live session is {session_id}"),
    ] {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }

    // abort takes a unique prefix of the id status prints.
    let abort = with(
        args(&["abort", "--public", &url, "--trust", p(&trust)]),
        &wallet_args,
    );
    let aborted = run(with(abort.clone(), &[&session_id[..12]])).await;
    let out = text(&aborted);
    assert!(aborted.status.success(), "{out}");
    assert!(out.contains("(attempt 1) in cohort 7: aborted"), "{out}");

    // A newer opening that was never committed (a failed submission) does not
    // displace the one the coordinator observed: upload picks that one and
    // reopens the aborted session as attempt 2.
    std::thread::sleep(std::time::Duration::from_millis(20));
    commit::prepare(
        &store,
        &chain601(),
        7,
        &identity,
        &artifact,
        &mut OsNonce,
        true,
    )
    .unwrap();
    let reopened = run(upload.clone()).await;
    let out = text(&reopened);
    assert!(reopened.status.success(), "{out}");
    assert!(out.contains("(attempt 2) in cohort 7: verified"), "{out}");

    // abort --cohort finds the live session itself; upload reopens it again.
    let aborted = run(with(abort, &["--cohort", "7"])).await;
    assert!(
        text(&aborted).contains("(attempt 2) in cohort 7: aborted"),
        "{}",
        text(&aborted)
    );
    let reopened = run(upload).await;
    assert!(
        text(&reopened).contains("(attempt 3) in cohort 7: verified"),
        "{}",
        text(&reopened)
    );

    mock.close();
    let after = run(with(status, &["--cohort", "7"])).await;
    let out = text(&after);
    assert!(after.status.success(), "{out}");
    assert!(
        out.contains(": admitted; position block 2000 extrinsic 4"),
        "{out}"
    );
    assert!(out.contains("no cohort is admitting"), "{out}");
    assert!(out.contains("admission closed at cutoff block"), "{out}");
}

/// Exit statuses: an unreadable file is an error (2); an object that does
/// not parse is refused (1, `object`); a coordinator that cannot be read
/// leaves the cohort unknown but the local checks still run, and their
/// refusal decides. `status` with nothing to show says so.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_exit_statuses_and_empty_status() {
    let dir = tempfile::tempdir().unwrap();
    let missing = run(args(&["check", p(&dir.path().join("missing.sna"))])).await;
    assert_eq!(missing.status.code(), Some(2), "{}", text(&missing));

    let garbage = dir.path().join("garbage.sna");
    std::fs::write(&garbage, b"not an artifact object").unwrap();
    let refused = run(args(&["check", p(&garbage)])).await;
    assert_eq!(refused.status.code(), Some(1), "{}", text(&refused));
    assert!(
        text(&refused).contains("[REFUSED] object"),
        "{}",
        text(&refused)
    );

    // The spec's six names with placeholder bytes, checked against a
    // coordinator that cannot be reached.
    let model = dir.path().join("model");
    std::fs::create_dir_all(&model).unwrap();
    for role in &spec::ROLES {
        std::fs::write(model.join(role.name), role.name).unwrap();
    }
    let six = dir.path().join("six.sna");
    let packed = run(args(&["pack", "--dir", p(&model), "--out", p(&six)])).await;
    assert!(packed.status.success(), "{}", text(&packed));
    let offline = run(args(&[
        "check",
        p(&six),
        "--coordinator",
        "http://127.0.0.1:9/",
    ]))
    .await;
    let out = text(&offline);
    assert_eq!(offline.status.code(), Some(1), "{out}");
    assert!(out.contains("tokenizer_mismatch"), "{out}");
    assert!(
        out.contains("cohort_unknown") && out.contains("the cohort could not be read"),
        "{out}"
    );

    let state = dir.path().join("state");
    let empty = run(args(&["status", "--state-dir", p(&state)])).await;
    let out = text(&empty);
    assert!(empty.status.success(), "{out}");
    assert!(
        out.contains("no local openings in state directory"),
        "{out}"
    );
    assert!(out.contains("no --public or --coordinator given"), "{out}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_refuses_plain_http_to_other_hosts() {
    let checked = run(args(&[
        "next-cohort",
        "--public",
        "http://example.invalid/",
    ]))
    .await;
    assert!(!checked.status.success());
    assert!(
        text(&checked).contains("must use https"),
        "{}",
        text(&checked)
    );
}
