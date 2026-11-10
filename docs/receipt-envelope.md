# Private receipt delivery

This page documents an executable prototype with a version-1 receipt schema.
The [whitepaper, draft 0.64](../whitepaper/main.tex), and
[Cloudflare design](cloudflare-subnet-draft.md) define production delivery and
receipt requirements. The prototype's validator-recipient and beacon fields
are implementation details used by its examples and fixtures. Production
receipt encoding and delivery require separate qualification.

Implemented local prototype, 2026-09-09. The Rust
[envelope library](../src/receipt_envelope.rs) encrypts an exact receipt payload
for an assigned validator and checks that decryption opens the miner's earlier
salted commitment. The caller supplies an independently authenticated assignment
and validator key record. It implements no chain registration, beacon
verification, persistent replay accounting, or network delivery. Current chain
recovery work belongs to the separate [native adapter](../subnet/README.md).

The [receipt evidence guide](subnet-receipts-and-audits.md) describes its place
in the current proposal. This module can carry an opaque payload; it does not
judge a revision, qualify current customer delivery, or establish that a Pangram
record is immutable.

## Encryption and signatures

The suite is HPKE Base with DHKEM(X25519, HKDF-SHA256), HKDF-SHA256 and
ChaCha20Poly1305. Each recipient gets a fresh encapsulation. A miner signature
authenticates the complete envelope, including ciphertext. A validator signature
authenticates a separate encryption key and its network, epoch and validity.
HPKE Base supplies neither sender authentication nor general replay protection
by itself. Recipient key compromise can expose recorded deliveries.
[RFC 9180](https://www.rfc-editor.org/rfc/rfc9180.html),
[pinned Rust API](https://docs.rs/hpke/0.14.1/hpke/).

The [hotkey helper](../src/hotkey_signature.rs) accepts raw sr25519 application
signatures over exact bytes, using Schnorrkel's `substrate` signing context.
It accepts neither an extrinsic signature prefix nor a `<Bytes>` wrapper.
This follows the reviewed [wallet signing implementation](https://github.com/RaoFoundation/subtensor/blob/67dcf7f791dc495064c293f080a0702cb433e51e/sdk/bittensor-core/src/keys/mod.rs)
and [Substrate primitive](https://github.com/RaoFoundation/polkadot-sdk/blob/cacb4310f20c7cac83eb3ccd8ed5a5ad4212608a/substrate/primitives/core/src/sr25519.rs).
The tests use synthetic signers and a published key vector; an independent
Bittensor wallet interoperability run remains outstanding. Other wallet schemes
are outside this prototype.

## Encoding

JSON transports public envelope fields. Cryptographic inputs use binary encoding,
so JSON key ordering and whitespace have no effect on their interpretation.
Contexts concatenate fields in their Rust declaration order: byte arrays have
fixed lengths, and integers use fixed-width big-endian encoding. Version 1 uses:

```text
commitment = SHA256(
  "slopninja/receipt-commitment/v1\0" || commitment_context ||
  secret_salt_32 || u64(payload_length) || exact_payload_bytes
)
opening = "SNJOPEN1" || secret_salt_32 || u32(payload_length) ||
          exact_payload_bytes || zero_padding
signature_input = "slopninja/receipt-envelope-signature/v1\0" ||
                  u16(version) || delivery_context || encapsulated_key_32 ||
                  u32(ciphertext_length) || ciphertext
```

The commitment context binds network genesis, subnet, epoch, opaque task ID,
miner hotkey, beacon chain and round. Delivery additionally binds the commitment,
recipient hotkey, key ID, encryption public key and validity window. HPKE uses
that delivery context as associated data and a fixed versioned suite identifier
as `info`. The signed key record has its own domain separator.

Payloads must contain 1 to 262,144 bytes. Openings use 4,096-byte padding buckets;
the AEAD adds 16 bytes. Decryption checks the exact length, zero padding and
prior commitment. It preserves UTF-8, CRLF and terminal whitespace as supplied.
URLs, scores, plaintext hashes and the salt belong inside the payload. Public
IDs must be opaque. Bucket size, participant identities and timing remain visible.

Private payload wrappers omit `Debug` and serialization and zeroize their owned
buffers on drop. Callers remain responsible for any copies, logs and storage.
The report fetcher discards transport error sources that could include the
secret report URL.

## Local demonstration

Place an authorized payload in ignored local storage. For a report check, its
JSON fields are `report_id`, `candidate_text`, `version`, `not_before` and `before`;
timestamps use RFC 3339. The report must already exist and its analyzed text must
match exactly. Then run the two commands as separate processes:

```sh
cargo run --example receipt_delivery -- demo \
  --payload data/receipt-envelope-v1/payload.json \
  --out data/receipt-envelope-v1/delivery
cargo run --example receipt_delivery -- open \
  --envelope data/receipt-envelope-v1/delivery/public/envelope.json \
  --assignment data/receipt-envelope-v1/delivery/public/assignment-fixture.json \
  --registration data/receipt-envelope-v1/delivery/public/recipient-registration.json \
  --secret-key data/receipt-envelope-v1/delivery/private/recipient-key.bin \
  --out data/receipt-envelope-v1/opened --fetch-report
```

Use fresh output directories. The demo creates disposable signers and a local
assignment fixture; it never accesses a wallet or submits a paid detector task.
On Unix, new directories use mode 0700 and files 0600. Decrypted text, provider
response and observation stay in the private output directory. Standard output
reports only operation status and whether chain/replay checks were performed.

Recorded validation from the September 9 prototype: five envelope tests cover two recipients, exact bytes,
tampering, authenticated malformed openings, commitment binding and fixed
encoding vectors. Three hotkey tests cover the signature convention. Root
`cargo fmt --check`, `cargo test` and strict all-target Clippy passed. Separate
demo/open processes decrypted our existing synthetic receipt and verified it
with one unauthenticated provider GET. A wrong recipient key failed before
creating an output directory. No new Pangram inference was purchased.
