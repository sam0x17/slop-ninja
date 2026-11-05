//! The `slop_ninja_miner` binary against the local mock: argument parsing,
//! wallet hotkey loading, exit codes, the trust file that pins the
//! coordinator, `next-cohort` and `status` from the planned public snapshot,
//! a resumed direct `upload` (the objects go to the mock's R2 stub through
//! the hidden loopback endpoint override), a refused second artifact shown
//! by `status`, `repair` and `status --repair` after a platform loss, and
//! the choice of opening. The commitment is prepared through the library
//! (the `commit` command needs a chain) and then placed on the mock's chain
//! table; nothing is submitted anywhere.

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
    assert!(
        text(&packed).contains("each file hashed once"),
        "{}",
        text(&packed)
    );
    let artifact = object::read_header(&object_path).unwrap();

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
    // An absolute local path in a JSON file warns at pack and at check, and
    // changes nothing else: pack succeeds, check refuses only what it did.
    std::fs::write(
        model.join("contract.json"),
        br#"{"runs":[{"run":"/opt/elsewhere/run-000"}]}"#,
    )
    .unwrap();
    let paths = dir.path().join("paths.sna");
    let packed = run(args(&["pack", "--dir", p(&model), "--out", p(&paths)])).await;
    assert!(packed.status.success(), "{}", text(&packed));
    let warned = String::from_utf8_lossy(&packed.stderr).into_owned();
    assert!(
        warned.contains(
            "warning: contract.json: 1 string or key read as or carry a local path (/runs/*/run)"
        ),
        "{warned}"
    );
    assert!(
        !text(&packed).contains("/opt/elsewhere"),
        "{}",
        text(&packed)
    );
    let checked = run(args(&["check", p(&paths)])).await;
    assert_eq!(checked.status.code(), Some(1), "{}", text(&checked));
    let out = text(&checked);
    assert!(
        out.contains("[WARNING] absolute_path: contract.json: 1 string or key read")
            && out.contains("tokenizer_mismatch"),
        "{out}"
    );
    let json = run(args(&["check", p(&paths), "--json"])).await;
    let report: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
    let warnings: Vec<&serde_json::Value> = report["reports"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|r| r["findings"].as_array().unwrap())
        .filter(|f| f["warning"] == true)
        .collect();
    assert_eq!(warnings.len(), 1, "{report}");
    assert_eq!(warnings[0]["file"], "contract.json");
    assert_eq!(warnings[0]["refuses"], false);

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

    let r2 = mock.r2_url();
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
            "--r2-endpoint-override",
            &r2,
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
            "upload_host": support::mock::S3_HOST,
            "upload_bucket": support::mock::BUCKET,
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

    // upload, interrupted after one object, then resumed.
    let interrupted = run(with(upload.clone(), &["--stop-after-objects", "1"])).await;
    let out = text(&interrupted);
    assert!(interrupted.status.success(), "{out}");
    assert!(out.contains("stopped after 1 objects"), "{out}");
    assert!(out.contains("objects go straight to R2"), "{out}");
    assert!(
        out.contains(&format!(
            "to coordinator {origin} (pinned by the trust file)"
        )),
        "{out}"
    );
    let resumed = run(upload.clone()).await;
    let out = text(&resumed);
    assert!(resumed.status.success(), "{out}");
    assert!(out.contains("in cohort 7: verified"), "{out}");
    // Progress on standard error: each PUT and its bytes.
    assert!(
        out.contains("PUT model.onnx") && out.contains("100.0%"),
        "{out}"
    );
    assert_eq!(mock.puts(), 4, "the header, then three files, each once");
    let session_id = out
        .split_whitespace()
        .find(|w| w.len() == 64 && w.bytes().all(|b| b.is_ascii_hexdigit()))
        .expect("the full session id is printed")
        .to_owned();

    // A second artifact for the same commitment is refused...
    let second_path = dir.path().join("b.sna");
    let packed = run(pack_args(&second_path, "owner2.json")).await;
    assert!(packed.status.success(), "{}", text(&packed));
    let second = object::read_header(&second_path).unwrap();
    assert_eq!(second.content, artifact.content);
    let mut second_file = prepared.file.clone();
    second_file.header_sha256 = Some(second.sha256.hex());
    second_file.header_len = Some(second.len);
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

    // ...and status shows the refusal, with the live session.
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
        &format!("session {session_id} in cohort 7: verified"),
        "last refused request: second_artifact",
        &format!("the live session is {session_id}"),
        "commit again (a new nonce)",
    ] {
        assert!(out.contains(needle), "missing {needle:?} in:\n{out}");
    }

    // A newer opening that was never committed (a failed submission) does not
    // displace the one the coordinator observed: upload picks that one and
    // finds its verified session; nothing is sent again.
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
    let again = run(upload.clone()).await;
    let out = text(&again);
    assert!(again.status.success(), "{out}");
    assert!(
        out.contains(&format!("session {session_id} in cohort 7: verified")),
        "{out}"
    );
    assert_eq!(mock.puts(), 4);

    mock.close();
    let after = run(with(status.clone(), &["--cohort", "7"])).await;
    let out = text(&after);
    assert!(after.status.success(), "{out}");
    assert!(
        out.contains(": admitted; position block 2000 extrinsic 4"),
        "{out}"
    );
    assert!(out.contains("no cohort is admitting"), "{out}");
    assert!(out.contains("admission closed at cutoff block"), "{out}");

    // A platform loss after admission: status says to repair; repair (by id
    // prefix) and status --repair send the lost objects again from the local
    // object, after the close.
    assert_eq!(mock.lose_objects(&session_id, |key| key.contains("/f/")), 3);
    let shown = run(with(status.clone(), &["--cohort", "7"])).await;
    let out = text(&shown);
    assert!(out.contains("run slop_ninja_miner repair"), "{out}");
    let repair = with(
        args(&[
            "repair",
            "--artifact",
            p(&object_path),
            "--cohort",
            "7",
            "--public",
            &url,
            "--trust",
            p(&trust),
            "--r2-endpoint-override",
            &r2,
            &session_id[..12],
        ]),
        &wallet_args,
    );
    let repaired = run(repair).await;
    let out = text(&repaired);
    assert!(repaired.status.success(), "{out}");
    assert!(
        out.contains(": admitted") && !out.contains("went missing"),
        "{out}"
    );
    assert_eq!(mock.puts(), 7);
    assert_eq!(mock.lose_objects(&session_id, |key| key.ends_with("/h")), 1);
    let fixed = run(with(
        status,
        &[
            "--cohort",
            "7",
            "--repair",
            "--artifact",
            p(&object_path),
            "--r2-endpoint-override",
            &r2,
            // No trust file here: the release's compiled-in pins would
            // refuse the mock's host and bucket.
            "--any-upload-host",
        ],
    ))
    .await;
    let out = text(&fixed);
    assert!(fixed.status.success(), "{out}");
    assert_eq!(mock.puts(), 8);
    // An endpoint override that is not loopback is refused.
    let refused = run(args(&[
        "repair",
        "--artifact",
        p(&object_path),
        "--coordinator",
        &url,
        "--r2-endpoint-override",
        "http://10.0.0.1:9/",
        "--wallet-path",
        p(&wallets),
        "--wallet",
        "miner",
        "--hotkey",
        "h1",
    ]))
    .await;
    assert_eq!(refused.status.code(), Some(2), "{}", text(&refused));
    assert!(text(&refused).contains("loopback"), "{}", text(&refused));

    // A coordinator that cannot be reached (a coordinator outage): status
    // still shows the local openings, then fails on the read (exit 2).
    let outage = run(args(&[
        "status",
        "--coordinator",
        "https://127.0.0.1:9",
        "--http-timeout-secs",
        "3",
        "--state-dir",
        p(&state),
    ]))
    .await;
    let out = text(&outage);
    assert_eq!(outage.status.code(), Some(2), "{out}");
    assert!(out.contains("opening cohort 7"), "{out}");
    assert!(out.contains("the cohort could not be read"), "{out}");
    let opening = out.find("opening cohort 7").unwrap();
    assert!(
        opening < out.find("error:").unwrap(),
        "openings first: {out}"
    );
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

/// The release trust file alone (no `--coordinator`, no `--public`) is
/// enough, as the miner guide says: `check` reads the cohort from its one
/// pinned origin, and `status` and `next-cohort` show the calendar from the
/// coordinator's unsigned cohort read while the status host is not live. A
/// coordinator refusal of a signed read exits 1, as a refusal.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn binary_uses_the_trust_files_pinned_origin() {
    let dir = tempfile::tempdir().unwrap();
    let mock = Mock::start(config(), 1_950).await;
    let origin = mock.url().trim_end_matches('/').to_owned();
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
    let state = dir.path().join("state");

    // check: the cohort is read from the pinned origin, so the check is
    // complete and only the synthetic tokenizer is refused.
    let model = dir.path().join("model");
    std::fs::create_dir_all(&model).unwrap();
    for role in &spec::ROLES {
        std::fs::write(model.join(role.name), role.name).unwrap();
    }
    let six = dir.path().join("six.sna");
    let packed = run(args(&["pack", "--dir", p(&model), "--out", p(&six)])).await;
    assert!(packed.status.success(), "{}", text(&packed));
    let checked = run(args(&["check", p(&six), "--trust", p(&trust)])).await;
    let out = text(&checked);
    assert_eq!(checked.status.code(), Some(1), "{out}");
    assert!(out.contains("tokenizer_mismatch"), "{out}");
    assert!(!out.contains("cohort_unknown"), "{out}");
    assert!(mock.with(|s| s.counters.routes.get("cohort").copied()) >= Some(1));

    // next-cohort and status: the cohort read of the pinned origin.
    let report = format!(
        "coordinator cohort read (unsigned) from {origin} (pinned by the trust file): subnet 601; \
         finalized block 1950, confirmed epoch 23169"
    );
    let next = run(args(&["next-cohort", "--trust", p(&trust)])).await;
    let out = text(&next);
    assert!(next.status.success(), "{out}");
    assert!(out.contains(&report), "{out}");
    assert!(
        out.contains("admitting cohort 7 (mock-c7): opened at epoch 23169 (the confirmed epoch)"),
        "{out}"
    );
    let shown = run(args(&[
        "status",
        "--trust",
        p(&trust),
        "--state-dir",
        p(&state),
    ]))
    .await;
    let out = text(&shown);
    assert!(shown.status.success(), "{out}");
    assert!(
        out.contains(&report) && out.contains("admitting cohort 7"),
        "{out}"
    );
    assert!(out.contains("no hotkey named"), "{out}");
    mock.close();
    let next = run(args(&["next-cohort", "--trust", p(&trust)])).await;
    let out = text(&next);
    assert!(next.status.success(), "{out}");
    assert!(out.contains("no cohort is admitting now"), "{out}");
    assert!(
        out.contains("next cohort 8 opens at epoch 23189 (20 epochs after the confirmed epoch)"),
        "{out}"
    );
    // Between windows check compares with the next cohort's plan values
    // and says whose they are.
    let between = run(args(&["check", p(&six), "--trust", p(&trust)])).await;
    let out = text(&between);
    assert_eq!(between.status.code(), Some(1), "{out}");
    assert!(out.contains("tokenizer_mismatch"), "{out}");
    assert!(!out.contains("cohort_unknown"), "{out}");
    assert!(
        out.contains("next_cohort") && out.contains("cohort 8's, the next to open"),
        "{out}"
    );

    // Nothing to choose from: next-cohort says what to pass and exits 2;
    // status reads the local openings only and says why.
    let none = run(args(&["next-cohort"])).await;
    assert_eq!(none.status.code(), Some(2), "{}", text(&none));
    assert!(text(&none).contains("--trust"), "{}", text(&none));
    let two = dir.path().join("two-trust.json");
    std::fs::write(
        &two,
        serde_json::json!({
            "schema": "slop_ninja_testnet_trust_roots_v1",
            "admission_origins": ["https://a.example", "https://b.example"],
        })
        .to_string(),
    )
    .unwrap();
    let local = run(args(&[
        "status",
        "--trust",
        p(&two),
        "--state-dir",
        p(&state),
    ]))
    .await;
    let out = text(&local);
    assert!(local.status.success(), "{out}");
    assert!(out.contains("no trust file pinning one origin"), "{out}");

    // An unregistered hotkey's signed read, refused by the upload Worker's
    // gate with not_registered (422): a refusal, exit 1, not an error.
    let wallets = dir.path().join("wallets");
    let wallet = Wallet::new("stranger", "h1", &wallets).unwrap();
    wallet
        .regenerate_hotkey(
            RegenerateFrom::Uri(SecretString::new("//Dave".to_owned())),
            CryptoType::Sr25519,
            WriteOptions::hotkey(),
        )
        .await
        .unwrap();
    mock.open_next(2_100);
    mock.with(|s| s.upload_gate = true);
    let refused = run(args(&[
        "status",
        "--trust",
        p(&trust),
        "--state-dir",
        p(&state),
        "--wallet-path",
        p(&wallets),
        "--wallet",
        "stranger",
        "--hotkey",
        "h1",
    ]))
    .await;
    let out = text(&refused);
    assert_eq!(refused.status.code(), Some(1), "{out}");
    assert!(out.contains("not_registered (422)"), "{out}");
}
