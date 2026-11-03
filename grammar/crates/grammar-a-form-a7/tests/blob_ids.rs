//! Every source file of this crate is a byte copy of `grammar-a-form`'s at
//! `ed25a148` (protocol A2 amendment 7's code, log index 19): its git blob id
//! (`SHA-1("blob " + length + "\0" + bytes)`) equals that commit's. No rule is
//! re-implemented (item 14.1's reason; BUILD-PLAN 12.3 open item (b)).
use sha1::{Digest, Sha1};

/// `git ls-tree ed25a148 grammar/crates/grammar-a-form/src/`.
const ED25A148: [(&str, &str); 11] = [
    ("conventions.rs", "b596e621b3e1e2ede1eed08cb3b262ba89ac5e49"),
    ("echo.rs", "624aa948dd917c26e2147d0661e260080f6d787b"),
    ("layout.rs", "e82d4ba53f22121ddfae306e408a6416c8cc4c1c"),
    ("lib.rs", "572951fc00f7c9a48c222bedb5dcbb7fe274368a"),
    ("measure.rs", "0536dfae001713a23c01c591d1c3e5dd7d398773"),
    ("profile.rs", "0ab4d2e067e3b27287cf8921f9e7449574c89e0b"),
    ("rank.rs", "fcce1d8210a7bbd84b91641e8da7ef16de0b2969"),
    ("screen.rs", "5894e4496fa444e25a72b3fce315dba1bbeb72f8"),
    ("text.rs", "9a49f93c64ca1ab16f824d8f7fcb32b427389d74"),
    ("transfer.rs", "2a559da9a6bd320e4225f4278004f3288b1bf220"),
    ("window.rs", "1a06be3d517dc16e7c4332ee881efd0e485bc09e"),
];

fn blob_id(b: &[u8]) -> String {
    let mut h = Sha1::new();
    h.update(format!("blob {}\0", b.len()).as_bytes());
    h.update(b);
    hex::encode(h.finalize())
}

#[test]
fn every_source_file_is_ed25a148s_blob() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut names: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    names.sort();
    let want: Vec<&str> = ED25A148.iter().map(|x| x.0).collect();
    assert_eq!(names, want, "the copy holds exactly ed25a148's source files");
    for (name, id) in ED25A148 {
        assert_eq!(blob_id(&std::fs::read(dir.join(name)).unwrap()), id, "{name} is not ed25a148's blob");
    }
}

#[test]
fn the_blob_id_is_gits() {
    // `printf 'x\n' | git hash-object --stdin`
    assert_eq!(blob_id(b"x\n"), "587be6b4c3f93f93c489c0111bba5596147a26cb");
}
