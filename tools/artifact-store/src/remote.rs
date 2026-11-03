use super::*;
use reqwest::{
    Method, StatusCode,
    blocking::{Client, Response},
    header::{AUTHORIZATION, HeaderValue},
};
use serde_json::{Value, json};
use std::process::Command;

// Only the Cloudflare management API is used. No public bucket URL, redirects,
// arbitrary endpoint, S3 credential file, or secret in process arguments.
struct R2 {
    client: Client,
    base: String,
    auth: HeaderValue,
    refreshes: u32,
}
// An expired Wrangler OAuth token (HTTP 401 or 403) is replaced by asking Wrangler again (its normal
// automatic renewal), then the one rejected request is repeated. Bounded per run.
const MAX_REFRESHES: u32 = 64;
fn bearer() -> Result<HeaderValue> {
    let executable = std::env::var_os("SLOP_NINJA_WRANGLER").unwrap_or_else(|| "wrangler".into());
    // Capture stdout/stderr. Never print the credential or forward raw child output.
    let result = Command::new(executable)
        .args(["auth", "token", "--json"])
        .env("WRANGLER_SEND_METRICS", "false")
        .env("WRANGLER_LOG", "log")
        .env("WRANGLER_WRITE_LOGS", "false")
        .output()
        .context("launch Wrangler for existing authentication")?;
    ensure!(
        result.status.success(),
        "Wrangler authentication failed; run wrangler login separately"
    );
    let credential: Value = serde_json::from_slice(&result.stdout)
        .context("invalid Wrangler authentication response")?;
    ensure!(
        matches!(
            credential.get("type").and_then(Value::as_str),
            Some("oauth" | "api_token")
        ),
        "Wrangler bearer token required"
    );
    let token = credential
        .get("token")
        .and_then(Value::as_str)
        .context("missing Wrangler token")?;
    let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| anyhow::anyhow!("invalid token header"))?;
    auth.set_sensitive(true);
    Ok(auth)
}
/// Validates a key prefix or object key: `/`-separated segments of ASCII letters,
/// digits, `-`, `_` and `.`, never empty, `.` or `..`. Keys are never source paths.
pub(super) fn key_path(s: &str) -> Result<()> {
    ensure!(
        !s.is_empty()
            && s.len() <= 512
            && s.split('/').all(|x| {
                !x.is_empty()
                    && x != "."
                    && x != ".."
                    && x.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            }),
        "invalid R2 key or prefix"
    );
    Ok(())
}
fn namespace(prefix: Option<&str>) -> Result<String> {
    match prefix {
        None => Ok(String::new()),
        Some(p) => {
            key_path(p)?;
            Ok(format!("{p}/"))
        }
    }
}
fn private_result(managed: &Value, custom: &Value) -> Result<()> {
    ensure!(
        managed.get("enabled").and_then(Value::as_bool) == Some(false),
        "R2 managed public URL must be disabled"
    );
    ensure!(
        custom
            .get("domains")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty),
        "R2 custom domains must be absent"
    );
    Ok(())
}
impl R2 {
    fn new(account: &str, bucket: &str) -> Result<Self> {
        ensure!(
            account.len() == 32 && account.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid account ID"
        );
        ensure!(
            (3..=63).contains(&bucket.len())
                && bucket
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !bucket.starts_with('-')
                && !bucket.ends_with('-'),
            "invalid bucket name"
        );
        let auth = bearer()?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .build()?;
        Ok(Self {
            client,
            base: format!(
                "https://api.cloudflare.com/client/v4/accounts/{account}/r2/buckets/{bucket}"
            ),
            auth,
            refreshes: 0,
        })
    }
    /// Sends one request; on HTTP 401/403 renews the token through Wrangler and repeats it once.
    fn send(
        &mut self,
        build: &dyn Fn(&Client) -> Result<reqwest::blocking::RequestBuilder>,
    ) -> Result<Response> {
        // Every request here is an idempotent GET of an immutable object or a PUT of a
        // content-addressed value that is always read back, so a bounded retry of transport
        // errors and 5xx answers (including Cloudflare's 524 timeout) cannot change data.
        let mut refreshed = false;
        let mut last = None;
        let mut attempt = 0;
        while attempt < ATTEMPTS {
            let r = match build(&self.client)?
                .header(AUTHORIZATION, self.auth.clone())
                .send()
            {
                Ok(r) => r,
                Err(e) => {
                    attempt += 1;
                    eprintln!("R2 transport error (attempt {attempt}/{ATTEMPTS}): {e}");
                    last = Some(anyhow::Error::new(e));
                    pause(attempt);
                    continue;
                }
            };
            let status = r.status();
            // Cloudflare answers an expired captured token with 401 or 403.
            if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) && !refreshed {
                drop(r);
                ensure!(
                    self.refreshes < MAX_REFRESHES,
                    "R2 authentication kept expiring; stopping"
                );
                self.refreshes += 1;
                refreshed = true;
                self.auth = bearer()?;
                eprintln!("R2 token renewed through Wrangler ({})", self.refreshes);
                continue;
            }
            if status.is_server_error() {
                drop(r);
                attempt += 1;
                eprintln!("R2 HTTP {status} (attempt {attempt}/{ATTEMPTS})");
                last = Some(anyhow::anyhow!("R2 HTTP {status}"));
                pause(attempt);
                continue;
            }
            return Ok(r);
        }
        Err(last
            .unwrap_or_else(|| anyhow::anyhow!("R2 request failed"))
            .context("R2 request failed after bounded retries; rerun verifies existing objects"))
    }
    fn privacy(&mut self) -> Result<()> {
        let mut results = Vec::new();
        for suffix in ["domains/managed", "domains/custom"] {
            let url = format!("{}/{suffix}", self.base);
            let r = self.send(&|c| Ok(c.get(&url)))?;
            let status = r.status();
            ensure!(
                status.is_success(),
                "R2 privacy check failed: HTTP {status}"
            );
            let b = read_bounded(r, 1024 * 1024)?;
            let v: Value = serde_json::from_slice(&b)?;
            ensure!(
                v.get("success").and_then(Value::as_bool) == Some(true),
                "R2 privacy API rejected request"
            );
            results.push(v.get("result").context("R2 result")?.clone());
        }
        private_result(&results[0], &results[1])
    }
    fn object_url(&self, key: &str) -> String {
        // Keys are generated from validated digests and prefixes, never source paths.
        format!("{}/objects/{key}", self.base)
    }
}
fn read_bounded(mut r: impl Read, max: u64) -> Result<Vec<u8>> {
    let mut b = Vec::new();
    r.by_ref().take(max + 1).read_to_end(&mut b)?;
    ensure!(b.len() as u64 <= max, "response exceeded size limit");
    Ok(b)
}

trait Objects {
    fn get(&mut self, key: &str) -> Result<Option<Box<dyn Read>>>;
    fn put(&mut self, key: &str, path: &Path) -> Result<()>;
}
impl Objects for R2 {
    fn get(&mut self, key: &str) -> Result<Option<Box<dyn Read>>> {
        let url = self.object_url(key);
        let r: Response = self.send(&|c| Ok(c.request(Method::GET, &url)))?;
        if r.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        ensure!(
            r.status().is_success(),
            "R2 GET failed: HTTP {}; credential refresh/retry is explicit",
            r.status()
        );
        Ok(Some(Box::new(r)))
    }
    fn put(&mut self, key: &str, path: &Path) -> Result<()> {
        let url = self.object_url(key);
        let r = self.send(&|c| {
            Ok(c.request(Method::PUT, &url)
                .header("Content-Type", "application/octet-stream")
                .header("Content-Length", fs::metadata(path)?.len())
                .header("cf-r2-data-catalog-check", "true")
                .body(File::open(path)?))
        })?;
        ensure!(
            r.status().is_success(),
            "R2 PUT failed: HTTP {}; rerun verifies existing objects before retry",
            r.status()
        );
        Ok(())
    }
}
const ATTEMPTS: u32 = 4;
fn pause(attempt: u32) {
    #[cfg(not(test))]
    std::thread::sleep(Duration::from_secs(15 * u64::from(attempt)));
    #[cfg(test)]
    let _ = attempt;
}
/// Ok(true): bytes and SHA256 match. Ok(false): a complete, different object (definitive).
/// Err: the body could not be read completely (transport; may be retried).
fn check_stream(reader: &mut dyn Read, bytes: u64, sha: &str) -> std::io::Result<bool> {
    let mut h = Sha256::new();
    let n = std::io::copy(&mut reader.take(bytes.saturating_add(1)), &mut h)?;
    Ok(n == bytes && format!("{:x}", h.finalize()) == sha)
}
/// Reads an existing object back in full. None if absent. A mismatch is never retried.
fn verify_remote(store: &mut impl Objects, key: &str, bytes: u64, sha: &str) -> Result<Option<()>> {
    for attempt in 1..=ATTEMPTS {
        let Some(mut r) = store.get(key)? else {
            return Ok(None);
        };
        match check_stream(&mut r, bytes, sha) {
            Ok(true) => return Ok(Some(())),
            Ok(false) => bail!("remote object checksum/size mismatch; refusing overwrite"),
            Err(e) => {
                eprintln!("R2 read-back interrupted (attempt {attempt}/{ATTEMPTS}): {e}");
                pause(attempt);
            }
        }
    }
    bail!("R2 read-back kept failing for {key}; rerun verifies existing objects")
}
fn ensure_object(
    store: &mut impl Objects,
    key: &str,
    path: &Path,
    bytes: u64,
    sha: &str,
) -> Result<bool> {
    if verify_remote(store, key, bytes, sha)?.is_some() {
        return Ok(false);
    }
    // PUT is idempotent for this immutable, content-addressed value. An uncertain
    // submission is reconciled by GET, here or on the next explicit run.
    ensure!(
        fs::metadata(path)?.len() == bytes && hash_file(path)? == sha,
        "upload source checksum mismatch"
    );
    store.put(key, path)?;
    verify_remote(store, key, bytes, sha)?.context("uploaded object unavailable")?;
    Ok(true)
}
fn journal(path: &Path, event: &Value) -> Result<()> {
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    writeln!(f, "{}", serde_json::to_string(event)?)?;
    f.sync_all()?;
    Ok(())
}
pub(super) fn upload(root: &Path, account: &str, bucket: &str, prefix: Option<&str>) -> Result<()> {
    let ns = namespace(prefix)?;
    let (m, b) = read_manifest(&root.join("manifest.json"))?;
    verify_blobs(root, &m)?;
    let sha = digest(&b);
    let mut r2 = R2::new(account, bucket)?;
    r2.privacy()?;
    let log = root.join("upload-journal.jsonl");
    journal(
        &log,
        &json!({"event":"start","account":account,"bucket":bucket,"prefix":ns,"manifest_sha256":sha,"public_access_checked":true}),
    )?;
    let mut seen = BTreeSet::new();
    for blob in &m.blobs {
        if !seen.insert(&blob.sha256) {
            continue;
        }
        let key = format!("{ns}v1/blobs/{}", blob.sha256);
        let uploaded = ensure_object(
            &mut r2,
            &key,
            &root.join("blobs").join(&blob.sha256),
            blob.bytes,
            &blob.sha256,
        )?;
        journal(
            &log,
            &json!({"event":"verified","account":account,"bucket":bucket,"key":key,"sha256":blob.sha256,"bytes":blob.bytes,"new_upload":uploaded}),
        )?;
        eprintln!("R2 verified {} unique chunks", seen.len());
    }
    r2.privacy()?;
    let key = format!("{ns}v1/manifests/{sha}.json");
    ensure_object(
        &mut r2,
        &key,
        &root.join("manifest.json"),
        b.len() as u64,
        &sha,
    )?;
    let receipt = json!({"event":"complete","schema":"slop_ninja_r2_snapshot_receipt_v1","account":account,"bucket":bucket,"prefix":ns,"manifest_key":key,"manifest_sha256":sha,"files":m.files.len(),"bytes":m.total_bytes,"unique_blobs":seen.len(),"verification":"full authenticated GET and SHA256 for every blob and manifest","source_deleted":false});
    journal(&log, &receipt)?;
    println!("{receipt}");
    Ok(())
}
fn download(store: &mut impl Objects, key: &str, path: &Path, bytes: u64, sha: &str) -> Result<()> {
    if path.exists() {
        no_symlinks(path)?;
        ensure!(
            fs::metadata(path)?.len() == bytes && hash_file(path)? == sha,
            "existing download corrupted; refusing overwrite"
        );
        return Ok(());
    }
    let parent = path.parent().context("download parent")?;
    let mut done = None;
    for attempt in 1..=ATTEMPTS {
        let mut reader = store.get(key)?.context("remote object missing")?;
        let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
        match std::io::copy(&mut reader.by_ref().take(bytes.saturating_add(1)), &mut tmp) {
            Ok(count) => {
                tmp.as_file().sync_all()?;
                ensure!(
                    count == bytes && hash_file(tmp.path())? == sha,
                    "download checksum mismatch"
                );
                done = Some(tmp);
                break;
            }
            Err(e) => {
                eprintln!("R2 download interrupted (attempt {attempt}/{ATTEMPTS}): {e}");
                pause(attempt);
            }
        }
    }
    let tmp = done.context("R2 download kept failing; rerun resumes the cache")?;
    tmp.persist_noclobber(path)?;
    File::open(path.parent().context("download parent")?)?.sync_all()?;
    Ok(())
}
pub(super) fn fetch(
    account: &str,
    bucket: &str,
    sha: &str,
    out: &Path,
    prefix: Option<&str>,
) -> Result<()> {
    safe_hash(sha)?;
    let ns = namespace(prefix)?;
    let mut r2 = R2::new(account, bucket)?;
    r2.privacy()?;
    let mut reader = r2
        .get(&format!("{ns}v1/manifests/{sha}.json"))?
        .context("remote snapshot missing")?;
    let bytes = read_bounded(&mut reader, MANIFEST_LIMIT)?;
    ensure!(digest(&bytes) == sha, "manifest hash mismatch");
    let m: Manifest = serde_json::from_slice(&bytes)?;
    validate(&m)?;
    if out.exists() {
        no_symlinks(out)?;
        ensure!(out.is_dir(), "cache directory required");
    } else {
        no_symlinks(out.parent().context("cache parent")?)?;
        private_dir(out)?;
    }
    let dir = out.join("blobs");
    if dir.exists() {
        no_symlinks(&dir)?;
    } else {
        private_dir(&dir)?;
    }
    let manifest = out.join("manifest.json");
    if manifest.exists() {
        no_symlinks(&manifest)?;
        ensure!(
            hash_file(&manifest)? == sha,
            "cache belongs to another snapshot"
        );
    }
    for blob in &m.blobs {
        download(
            &mut r2,
            &format!("{ns}v1/blobs/{}", blob.sha256),
            &dir.join(&blob.sha256),
            blob.bytes,
            &blob.sha256,
        )?;
    }
    if !manifest.exists() {
        write_new(&manifest, &bytes)?;
    }
    verify_blobs(out, &m)?;
    println!("fetched and verified snapshot {sha}");
    Ok(())
}

/// Stores one small immutable object (README, catalog) at a readable key. An existing
/// object must be byte-identical; it is never replaced. The object is read back and hashed.
pub(super) fn put_object(account: &str, bucket: &str, key: &str, path: &Path) -> Result<()> {
    key_path(key)?;
    ensure!(
        !snapshot_namespace(key),
        "object keys may not use snapshot namespaces"
    );
    no_symlinks(path)?;
    let bytes = fs::metadata(path)?.len();
    ensure!(bytes <= MANIFEST_LIMIT, "object too large for put-object");
    let sha = hash_file(path)?;
    let mut r2 = R2::new(account, bucket)?;
    r2.privacy()?;
    let uploaded = ensure_object(&mut r2, key, path, bytes, &sha)?;
    r2.privacy()?;
    println!(
        "{}",
        json!({"event":"object_verified","schema":"slop_ninja_r2_object_receipt_v1","account":account,"bucket":bucket,"key":key,"sha256":sha,"bytes":bytes,"new_upload":uploaded,"verification":"full authenticated GET and SHA256"})
    );
    Ok(())
}
/// True for keys inside a snapshot namespace: `v1/...` or `PREFIX/v1/{blobs,manifests}/...`.
fn snapshot_namespace(key: &str) -> bool {
    let parts: Vec<&str> = key.split('/').collect();
    parts.first() == Some(&"v1")
        || parts
            .windows(2)
            .any(|w| w[0] == "v1" && matches!(w[1], "blobs" | "manifests"))
}
/// Downloads one object into a new file, accepting it only if it matches the pinned SHA256.
pub(super) fn get_object(
    account: &str,
    bucket: &str,
    key: &str,
    sha: &str,
    out: &Path,
) -> Result<()> {
    key_path(key)?;
    safe_hash(sha)?;
    ensure!(!out.exists(), "destination exists");
    let mut r2 = R2::new(account, bucket)?;
    r2.privacy()?;
    let mut reader = r2.get(key)?.context("remote object missing")?;
    let b = read_bounded(&mut reader, MANIFEST_LIMIT)?;
    ensure!(digest(&b) == sha, "object checksum mismatch");
    no_symlinks(out.parent().context("output parent")?)?;
    write_new(out, &b)?;
    println!("fetched and verified object {key} {sha}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    #[derive(Default)]
    struct Memory {
        values: BTreeMap<String, Vec<u8>>,
        puts: usize,
        fail_after_put: bool,
    }
    impl Objects for Memory {
        fn get(&mut self, key: &str) -> Result<Option<Box<dyn Read>>> {
            Ok(self
                .values
                .get(key)
                .map(|v| Box::new(Cursor::new(v.clone())) as Box<dyn Read>))
        }
        fn put(&mut self, key: &str, path: &Path) -> Result<()> {
            self.puts += 1;
            self.values.insert(key.into(), fs::read(path)?);
            ensure!(!self.fail_after_put, "simulated uncertain PUT");
            Ok(())
        }
    }
    #[test]
    fn uncertain_upload_resumes_without_replacement() -> Result<()> {
        let t = tempfile::tempdir()?;
        let p = t.path().join("data");
        fs::write(&p, b"synthetic")?;
        let sha = hash_file(&p)?;
        let mut mem = Memory {
            fail_after_put: true,
            ..Default::default()
        };
        assert!(ensure_object(&mut mem, "key", &p, 9, &sha).is_err());
        mem.fail_after_put = false;
        assert!(!ensure_object(&mut mem, "key", &p, 9, &sha)?);
        assert_eq!(mem.puts, 1);
        let restored = t.path().join("download");
        download(&mut mem, "key", &restored, 9, &sha)?;
        assert_eq!(hash_file(&restored)?, sha);
        mem.values.insert("key".into(), b"corrupted".to_vec());
        assert!(ensure_object(&mut mem, "key", &p, 9, &sha).is_err());
        assert!(download(&mut mem, "key", &t.path().join("bad"), 9, &sha).is_err());
        assert!(!t.path().join("bad").exists());
        assert_eq!(mem.puts, 1);
        Ok(())
    }
    /// Serves each object, but the first `flaky` reads stop halfway with an I/O error.
    struct Flaky {
        inner: Memory,
        flaky: usize,
    }
    struct Broken(Cursor<Vec<u8>>);
    impl Read for Broken {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.0.position() * 2 >= self.0.get_ref().len() as u64 {
                return Err(std::io::Error::other("synthetic connection reset"));
            }
            let n = buf.len().min(1);
            self.0.read(&mut buf[..n])
        }
    }
    impl Objects for Flaky {
        fn get(&mut self, key: &str) -> Result<Option<Box<dyn Read>>> {
            let v = self.inner.values.get(key).cloned();
            Ok(v.map(|v| {
                if self.flaky > 0 {
                    self.flaky -= 1;
                    Box::new(Broken(Cursor::new(v))) as Box<dyn Read>
                } else {
                    Box::new(Cursor::new(v)) as Box<dyn Read>
                }
            }))
        }
        fn put(&mut self, key: &str, path: &Path) -> Result<()> {
            self.inner.put(key, path)
        }
    }
    #[test]
    fn transient_reads_retry_but_mismatch_never_does() -> Result<()> {
        let t = tempfile::tempdir()?;
        let p = t.path().join("data");
        fs::write(&p, b"synthetic payload")?;
        let sha = hash_file(&p)?;
        let mut f = Flaky {
            inner: Memory::default(),
            flaky: 0,
        };
        f.inner.values.insert("key".into(), fs::read(&p)?);
        f.flaky = 2;
        assert!(!ensure_object(&mut f, "key", &p, 17, &sha)?);
        assert_eq!(f.inner.puts, 0);
        f.flaky = 2;
        download(&mut f, "key", &t.path().join("out"), 17, &sha)?;
        assert_eq!(hash_file(&t.path().join("out"))?, sha);
        f.flaky = ATTEMPTS as usize;
        assert!(ensure_object(&mut f, "key", &p, 17, &sha).is_err());
        f.flaky = 0;
        f.inner
            .values
            .insert("key".into(), b"different payload".to_vec());
        assert!(ensure_object(&mut f, "key", &p, 17, &sha).is_err());
        assert_eq!(f.inner.puts, 0);
        Ok(())
    }
    #[test]
    fn key_paths_are_constrained() -> Result<()> {
        assert_eq!(namespace(None)?, "");
        assert_eq!(
            namespace(Some("evidence/detector-observations/pangram-gold-v2"))?,
            "evidence/detector-observations/pangram-gold-v2/"
        );
        for bad in [
            "",
            "/abs",
            "a//b",
            "a/../b",
            "a/./b",
            "trailing/",
            "sp ace",
            "q?x",
            "a\\b",
        ] {
            assert!(key_path(bad).is_err(), "{bad}");
        }
        assert!(key_path("manifests/README-20261029-v1.md").is_ok());
        assert!(!snapshot_namespace("manifests/catalog-20261030-v1.json"));
        assert!(!snapshot_namespace("restricted/x-v1/README.md"));
        assert!(snapshot_namespace("v1/blobs/abc"));
        assert!(snapshot_namespace("models/a/v1/manifests/abc.json"));
        Ok(())
    }
    #[test]
    fn privacy_fails_closed() {
        assert!(private_result(&json!({"enabled":false}), &json!({"domains":[]})).is_ok());
        for m in [
            json!({}),
            json!({"enabled":true}),
            json!({"enabled":"false"}),
        ] {
            assert!(private_result(&m, &json!({"domains":[]})).is_err());
        }
        assert!(
            private_result(
                &json!({"enabled":false}),
                &json!({"domains":[{"domain":"example.invalid","enabled":false}]})
            )
            .is_err()
        );
        assert!(private_result(&json!({"enabled":false}), &json!({})).is_err());
    }
}
