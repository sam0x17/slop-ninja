// A throwaway OpenSSH key and its signature (`ssh-keygen -Y sign -n
// slop-ninja-operator-decision`, SHA-512) over `fixture statement\n`, kept as hex; the
// private key was deleted. The fingerprint is the SHA-256 that `ssh-keygen -l` printed.
const OPENSSH_FIXTURE_KEY_HEX: &str = "51ac964d3da312c18b963c526deb9b5cb121aa0134c78d398f71ddd3f38c25d1";
const OPENSSH_FIXTURE_FINGERPRINT_HEX: &str = "76b63e313ae0726ac8cf16509bb3743d86b0246acf54580a38fab68035c16658";
const OPENSSH_FIXTURE_SIG_HEX: &str = concat!(
    "53534853494700000001000000330000000b7373682d656432353531390000002051ac964d3da312c18b963c526deb9b",
    "5cb121aa0134c78d398f71ddd3f38c25d10000001c736c6f702d6e696e6a612d6f70657261746f722d6465636973696f",
    "6e0000000000000006736861353132000000530000000b7373682d656432353531390000004007dcdd176b38a099212b",
    "6302598ca8db2cf6bd2b23f7c218eb389e80f889dbbb9a8652f0d2874f97c51828a2b2352c0a90b11bc52550a519f8aa",
    "feefeb1eb101",
);
