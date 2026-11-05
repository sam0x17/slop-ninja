//! Seeded, domain-separated digests. Every part is length-prefixed, so no two
//! part lists share a digest by concatenation.

use sha2::{Digest, Sha256};

/// SHA-256 of `parts`, each prefixed by its length (u64, big-endian).
pub fn h(parts: &[&[u8]]) -> [u8; 32] {
    let mut s = Sha256::new();
    for p in parts {
        s.update((p.len() as u64).to_be_bytes());
        s.update(p);
    }
    s.finalize().into()
}

/// The first eight bytes of [`h`] as an integer.
pub fn h64(parts: &[&[u8]]) -> u64 {
    let d = h(parts);
    let mut b = [0u8; 8];
    b.copy_from_slice(d.get(..8).unwrap_or(&[0; 8]));
    u64::from_be_bytes(b)
}

/// Plain SHA-256, hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The indices `0..n` ranked by `h64(seed, label, index)` (ties by index):
/// a seeded permutation.
pub fn permutation(seed: &[u8; 32], label: &[&[u8]], n: usize) -> Vec<u16> {
    let mut keyed: Vec<(u64, u16)> = (0..n as u16)
        .map(|i| {
            let ib = i.to_be_bytes();
            let mut parts: Vec<&[u8]> = Vec::with_capacity(label.len() + 2);
            parts.push(seed);
            parts.extend_from_slice(label);
            parts.push(&ib);
            (h64(&parts), i)
        })
        .collect();
    keyed.sort_unstable();
    keyed.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_are_separated_and_permutations_are_permutations() {
        assert_ne!(h(&[b"ab", b"c"]), h(&[b"a", b"bc"]));
        let p = permutation(&[1; 32], &[b"x"], 14);
        let mut s = p.clone();
        s.sort_unstable();
        assert_eq!(s, (0..14).collect::<Vec<u16>>());
        assert_eq!(p, permutation(&[1; 32], &[b"x"], 14));
        assert_ne!(p, permutation(&[2; 32], &[b"x"], 14));
    }
}
