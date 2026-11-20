use super::*;
use crate::{OutcomeClass, ReceiptIndex, Signed, SigningKey, TrustedKeys, domain_hash, sign};

fn manifest() -> Digest {
    Digest([91; 32])
}

fn attempt(n: u64) -> Digest {
    let mut bytes = [0; 32];
    bytes[..8].copy_from_slice(&n.to_be_bytes());
    Digest(bytes)
}

fn chunk(ordinal: u64, start: u64, count: usize) -> ReceiptIndexChunk {
    ReceiptIndexChunk {
        cohort_manifest: manifest(),
        ordinal,
        entries: (start..start + count as u64)
            .map(|n| ReceiptIndexEntry {
                attempt: attempt(n),
                outcome: OutcomeClass::Prediction,
                object_sha256: Digest([19; 32]),
                object_len: 153,
            })
            .collect(),
    }
}

fn object(n: u64) -> Vec<u8> {
    AttemptReceipt {
        attempt: attempt(n),
        assignment: Digest([21; 32]),
        artifact: Digest([22; 32]),
        number: 1,
        outcome: OutcomeClass::Prediction,
        result_commitment: Digest([23; 32]),
    }
    .to_bytes()
    .unwrap()
}

#[test]
fn chunk_budget_is_derived_and_decode_rejects_oversize_before_entries() {
    let full = chunk(0, 0, CHUNK_ENTRIES);
    let bytes = full.to_bytes().unwrap();
    assert_eq!(
        bytes.len(),
        CHUNK_HEADER_BYTES + CHUNK_ENTRIES * ENTRY_BYTES
    );
    assert!(bytes.len() <= CHUNK_BYTES);
    assert!(bytes.len() + ENTRY_BYTES > CHUNK_BYTES);
    assert_eq!(ReceiptIndexChunk::from_bytes(&bytes).unwrap(), full);
    assert!(chunk(0, 0, CHUNK_ENTRIES + 1).to_bytes().is_err());
    let mut bad = bytes[..CHUNK_HEADER_BYTES].to_vec();
    bad[CHUNK_HEADER_BYTES - 4..].copy_from_slice(&((CHUNK_ENTRIES + 1) as u32).to_le_bytes());
    assert!(matches!(
        ReceiptIndexChunk::from_bytes(&bad),
        Err(Error::LengthLimit { .. })
    ));
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert_eq!(
        ReceiptIndexChunk::from_bytes(&trailing),
        Err(Error::TrailingBytes)
    );
    assert_eq!(
        ReceiptIndexChunk::from_bytes(&bytes[..bytes.len() - 1]),
        Err(Error::UnexpectedEnd)
    );
}

#[test]
fn canonical_boundaries_and_global_order_are_checked() {
    let mut b = ReceiptIndexBuilder::new(manifest());
    assert!(b.push(&chunk(1, 0, CHUNK_ENTRIES)).is_err());
    assert!(b.push(&chunk(0, 0, 0)).is_err());
    let first = chunk(0, 0, CHUNK_ENTRIES);
    b.push(&first).unwrap();
    assert!(b.push(&first).is_err());
    assert!(b.push(&chunk(1, 0, CHUNK_ENTRIES)).is_err());
    assert!(matches!(
        b.push(&chunk(1, CHUNK_ENTRIES as u64 - 1, 1)),
        Err(Error::Duplicate(_))
    ));
    let mut wrong_manifest = chunk(1, CHUNK_ENTRIES as u64, 1);
    wrong_manifest.cohort_manifest = Digest([92; 32]);
    assert!(b.push(&wrong_manifest).is_err());
    b.push(&chunk(1, CHUNK_ENTRIES as u64, 1)).unwrap();
    assert!(b.push(&chunk(2, CHUNK_ENTRIES as u64 + 1, 1)).is_err());
    assert_eq!(b.finish().receipt_count, CHUNK_ENTRIES as u64 + 1);
    let mut unordered = chunk(0, 0, 2);
    unordered.entries.swap(0, 1);
    assert!(matches!(unordered.validate(), Err(Error::Unsorted(_))));
    unordered.entries[1] = unordered.entries[0].clone();
    assert!(matches!(unordered.validate(), Err(Error::Duplicate(_))));
}

#[test]
fn streaming_digest_matches_independent_flat_digest_of_chunk_ids() {
    let chunks = [
        chunk(0, 0, CHUNK_ENTRIES),
        chunk(1, CHUNK_ENTRIES as u64, 2),
    ];
    let mut payload = manifest().0.to_vec();
    let mut b = ReceiptIndexBuilder::new(manifest());
    for c in &chunks {
        payload.extend_from_slice(&c.digest().unwrap().0);
        b.push(c).unwrap();
    }
    let commitment = b.finish();
    assert_eq!(
        commitment.chunks_digest,
        domain_hash(CHUNKS_DOMAIN, &payload)
    );
    assert_eq!(commitment.chunk_count(), 2);
    assert_eq!(
        ReceiptIndexCommitment::from_bytes(&commitment.to_bytes().unwrap()).unwrap(),
        commitment
    );
}

#[test]
fn omitted_or_mutated_chunks_cannot_verify() {
    let first = chunk(0, 0, CHUNK_ENTRIES);
    let last = chunk(1, CHUNK_ENTRIES as u64, 2);
    let mut b = ReceiptIndexBuilder::new(manifest());
    b.push(&first).unwrap();
    b.push(&last).unwrap();
    let expected = b.finish();
    let mut omitted = ReceiptIndexBuilder::new(manifest());
    omitted.push(&first).unwrap();
    assert!(omitted.verify(&expected).is_err());
    let mut altered = first.clone();
    altered.entries[0].object_sha256 = Digest([29; 32]);
    let mut b = ReceiptIndexBuilder::new(manifest());
    b.push(&altered).unwrap();
    b.push(&last).unwrap();
    assert!(b.verify(&expected).is_err());
    let mut correct = ReceiptIndexBuilder::new(manifest());
    correct.push(&first).unwrap();
    correct.push(&last).unwrap();
    correct.verify(&expected).unwrap();
}

#[test]
fn object_verification_is_exact_and_failed_push_does_not_advance() {
    let objects = [object(0), object(1)];
    let entries = objects
        .iter()
        .map(|bytes| {
            ReceiptIndexEntry::for_receipt(&AttemptReceipt::from_bytes(bytes).unwrap()).unwrap()
        })
        .collect();
    let c = ReceiptIndexChunk {
        cohort_manifest: manifest(),
        ordinal: 0,
        entries,
    };
    let mut b = ReceiptIndexBuilder::new(manifest());
    assert!(
        b.push_verified(&c, objects[..1].iter().map(Vec::as_slice))
            .is_err()
    );
    assert!(
        b.push_verified(&c, objects.iter().rev().map(Vec::as_slice))
            .is_err()
    );
    assert!(
        b.push_verified(&c, objects.iter().chain(objects.iter()).map(Vec::as_slice))
            .is_err()
    );
    let mut corrupt = objects.clone();
    corrupt[1][40] ^= 1;
    assert!(
        b.push_verified(&c, corrupt.iter().map(Vec::as_slice))
            .is_err()
    );
    let mut wrong_entry = c.clone();
    wrong_entry.entries[0].outcome = OutcomeClass::ModelError;
    assert!(
        b.push_verified(&wrong_entry, objects.iter().map(Vec::as_slice))
            .is_err()
    );
    b.push_verified(&c, objects.iter().map(Vec::as_slice))
        .unwrap();
    assert_eq!(b.finish().receipt_count, 2);
}

#[test]
fn invalid_receipt_cannot_pass_even_with_matching_hash() {
    let mut bytes = object(0);
    // A syntactically encoded but invalid attempt number.
    let number_position = 4 + AttemptReceipt::KIND.len() + 2 + 32 * 3;
    bytes[number_position] = 0;
    let mut c = chunk(0, 0, 1);
    c.entries[0].object_sha256 = object_sha256(&bytes);
    c.entries[0].object_len = bytes.len() as u32;
    let mut b = ReceiptIndexBuilder::new(manifest());
    assert!(matches!(
        b.push_verified(&c, [bytes.as_slice()]),
        Err(Error::Invalid("attempt number"))
    ));
}

#[test]
fn versions_and_signature_roles_do_not_alias() {
    let receipt_key = SigningKey::from_bytes(&[31; 32]);
    let package_key = SigningKey::from_bytes(&[32; 32]);
    let auth_key = SigningKey::from_bytes(&[33; 32]);
    let keys = TrustedKeys::new(
        package_key.verifying_key().to_bytes(),
        receipt_key.verifying_key().to_bytes(),
        auth_key.verifying_key().to_bytes(),
    )
    .unwrap();
    let old = ReceiptIndex {
        cohort_manifest: manifest(),
        entries: chunk(0, 0, 2).entries,
    };
    let old_bytes = sign(old.clone(), &receipt_key).unwrap().to_bytes().unwrap();
    assert_eq!(
        Signed::<ReceiptIndex>::open(&old_bytes, &keys).unwrap(),
        old
    );
    let mut b = ReceiptIndexBuilder::new(manifest());
    b.push(&chunk(0, 0, 2)).unwrap();
    let new = b.finish();
    let new_bytes = sign(new.clone(), &receipt_key).unwrap().to_bytes().unwrap();
    assert_eq!(
        Signed::<ReceiptIndexCommitment>::open(&new_bytes, &keys).unwrap(),
        new
    );
    assert!(Signed::<ReceiptIndex>::open(&new_bytes, &keys).is_err());
    assert!(Signed::<ReceiptIndexCommitment>::open(&old_bytes, &keys).is_err());
    assert_ne!(old.digest().unwrap(), new.digest().unwrap());
    let bad_role = sign(new.clone(), &package_key).unwrap().to_bytes().unwrap();
    assert_eq!(
        Signed::<ReceiptIndexCommitment>::open(&bad_role, &keys),
        Err(Error::UntrustedKey)
    );
    let mut tampered = sign(new, &receipt_key).unwrap();
    tampered.record.receipt_count += 1;
    assert_eq!(tampered.verify(&keys), Err(Error::BadSignature));
    assert!(new_bytes.len() < 256);
}

#[test]
fn empty_index_has_one_canonical_commitment() {
    let empty = ReceiptIndexBuilder::new(manifest()).finish();
    assert_eq!(empty.chunk_count(), 0);
    empty.validate().unwrap();
    ReceiptIndexBuilder::new(manifest()).verify(&empty).unwrap();
    let mut bad = empty;
    bad.chunks_digest.0[0] ^= 1;
    assert!(bad.validate().is_err());
}

#[test]
fn full_receipt_bound_streams_without_a_flat_index() {
    // 256 entrants * (4,000 batch + 1,000 probe + 800 audit), three
    // references including a separate span anchor, and three attempts.
    let count = (256 * (4_000 + 1_000 + 800) + 3 * 4_000) * 3_u64;
    assert_eq!(count, 4_490_400);
    assert!(count > crate::target_bundle::receipt_index_capacity().unwrap());
    let mut b = ReceiptIndexBuilder::new(manifest());
    let mut digests = Vec::new();
    let mut position = 0;
    let mut ordinal = 0;
    while position < count {
        let n = (count - position).min(CHUNK_ENTRIES as u64) as usize;
        let c = chunk(ordinal, position, n);
        assert!(c.to_bytes().unwrap().len() <= CHUNK_BYTES);
        b.push(&c).unwrap();
        digests.push(c.digest().unwrap());
        position += n as u64;
        ordinal += 1;
    }
    let record = b.finish();
    assert_eq!(record.receipt_count, count);
    assert_eq!(record.chunk_count(), ordinal);
    assert_eq!(
        ReceiptIndexCommitment::from_verified_chunk_digests(
            manifest(),
            count,
            digests.iter().copied().map(Ok)
        )
        .unwrap(),
        record
    );
    assert!(
        ReceiptIndexCommitment::from_verified_chunk_digests(
            manifest(),
            count,
            digests.iter().copied().take(digests.len() - 1).map(Ok)
        )
        .is_err()
    );
    assert!(
        ReceiptIndexCommitment::from_verified_chunk_digests(
            manifest(),
            count,
            digests.iter().copied().chain([attempt(99)]).map(Ok)
        )
        .is_err()
    );
    let failed = digests.iter().enumerate().map(|(i, digest)| {
        if i == 256 {
            Err(Error::Invalid("synthetic ledger read"))
        } else {
            Ok(*digest)
        }
    });
    assert_eq!(
        ReceiptIndexCommitment::from_verified_chunk_digests(manifest(), count, failed),
        Err(Error::Invalid("synthetic ledger read"))
    );
    assert_eq!(
        record.to_bytes().unwrap().len(),
        4 + ReceiptIndexCommitment::KIND.len() + 2 + 32 + 8 + 32
    );
}

fn signing_keys() -> (SigningKey, TrustedKeys) {
    let receipt = SigningKey::from_bytes(&[51; 32]);
    let package = SigningKey::from_bytes(&[52; 32]);
    let authorization = SigningKey::from_bytes(&[53; 32]);
    let keys = TrustedKeys::new(
        package.verifying_key().to_bytes(),
        receipt.verifying_key().to_bytes(),
        authorization.verifying_key().to_bytes(),
    )
    .unwrap();
    (receipt, keys)
}

#[test]
fn commitment_reader_authenticates_every_transport_split() {
    let (key, keys) = signing_keys();
    let mut builder = ReceiptIndexBuilder::new(manifest());
    builder.push(&chunk(0, 0, 3)).unwrap();
    let record = builder.finish();
    assert_eq!(record.to_bytes().unwrap().len(), COMMITMENT_BYTES);
    let bytes = sign(record.clone(), &key).unwrap().to_bytes().unwrap();
    for split in 0..=bytes.len() {
        let mut reader = ReceiptCommitmentStream::new();
        reader.update(&bytes[..split]);
        reader.update(&[]);
        reader.update(&bytes[split..]);
        assert_eq!(reader.finish(&keys).unwrap(), record);
    }
    let mut reader = ReceiptCommitmentStream::default();
    for byte in &bytes {
        reader.update(std::slice::from_ref(byte));
    }
    let verified = reader.finish(&keys).unwrap();
    assert_eq!(
        verified.binding().unwrap(),
        crate::ReceiptIndexBinding {
            cohort_manifest: manifest(),
            root: record.digest().unwrap(),
            count: 3,
        }
    );
}

#[test]
fn commitment_reader_refuses_truncation_trailing_bytes_and_signature_damage() {
    let (key, keys) = signing_keys();
    let record = ReceiptIndexBuilder::new(manifest()).finish();
    let bytes = sign(record, &key).unwrap().to_bytes().unwrap();
    for end in 0..bytes.len() {
        let mut reader = ReceiptCommitmentStream::new();
        reader.update(&bytes[..end]);
        assert!(reader.finish(&keys).is_err());
    }
    for position in 0..bytes.len() {
        let mut altered = bytes.clone();
        altered[position] ^= 1;
        let mut reader = ReceiptCommitmentStream::new();
        reader.update(&altered);
        assert!(
            reader.finish(&keys).is_err(),
            "accepted mutation at {position}"
        );
    }
    let mut reader = ReceiptCommitmentStream::new();
    reader.update(&bytes);
    reader.update(&[0]);
    assert_eq!(reader.finish(&keys), Err(Error::TrailingBytes));
}

#[test]
fn trusted_signature_cannot_bypass_commitment_shape_or_version() {
    let (key, keys) = signing_keys();
    let valid = ReceiptIndexBuilder::new(manifest()).finish();
    let mut extra = valid.to_bytes().unwrap();
    extra.push(0);
    let short = extra[..COMMITMENT_BYTES - 1].to_vec();
    let mut invalid_empty = valid.clone();
    invalid_empty.chunks_digest.0[0] ^= 1;
    let mut encoded = Encoder::new();
    encoded.str(ReceiptIndexCommitment::KIND);
    encoded.u16(ReceiptIndexCommitment::VERSION);
    invalid_empty.encode_to(&mut encoded);
    let mut wrong_version = valid.to_bytes().unwrap();
    let version_at = 4 + ReceiptIndexCommitment::KIND.len();
    wrong_version[version_at..version_at + 2].copy_from_slice(&1_u16.to_le_bytes());
    for raw in [extra, short, encoded.into_bytes(), wrong_version] {
        let bytes = crate::sign_raw::<ReceiptIndexCommitment>(&raw, &key).unwrap();
        let mut reader = ReceiptCommitmentStream::new();
        reader.update(&bytes);
        assert!(reader.finish(&keys).is_err());
    }
    let legacy = ReceiptIndex {
        cohort_manifest: manifest(),
        entries: vec![],
    };
    let old = sign(legacy, &key).unwrap().to_bytes().unwrap();
    let mut new_reader = ReceiptCommitmentStream::new();
    new_reader.update(&old);
    assert!(new_reader.finish(&keys).is_err());
    let new = sign(valid, &key).unwrap().to_bytes().unwrap();
    let mut old_reader = crate::ReceiptIndexStream::new();
    old_reader.update(&new);
    assert!(old_reader.finish(&keys).is_err());
}
