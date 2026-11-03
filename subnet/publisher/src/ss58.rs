//! SS58 addresses (generic Substrate prefix 42, as Bittensor uses) for the
//! hotkeys the leaderboard shows.

use blake2::{Blake2b512, Digest as _};
use slop_ninja_protocol::AccountId;

const PREFIX: u8 = 42;

/// SS58 (prefix 42) of a 32-byte public key.
pub fn encode(account: &AccountId) -> String {
    let mut payload = Vec::with_capacity(35);
    payload.push(PREFIX);
    payload.extend_from_slice(&account.0);
    let mut hash = Blake2b512::new();
    hash.update(b"SS58PRE");
    hash.update(&payload);
    let checksum = hash.finalize();
    payload.extend_from_slice(&checksum[..2]);
    bs58::encode(payload).into_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_address() {
        // The well-known development key `//Alice`.
        let alice = hex::decode("d43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d")
            .unwrap();
        let account = AccountId(alice.try_into().unwrap());
        assert_eq!(
            encode(&account),
            "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY"
        );
    }
}
