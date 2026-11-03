use super::*;
use schnorrkel::{ExpansionMode, MiniSecretKey};
use serde_json::json;

const NOW: u64 = 150;
const PRIVATE_MARKER: &str = "PRIVATE-OPENING-SENTINEL";

fn signer(seed: u8) -> Keypair {
    MiniSecretKey::from_bytes(&[seed; 32])
        .unwrap()
        .expand_to_keypair(ExpansionMode::Ed25519)
}

struct Fixture {
    sender: Keypair,
    validator: Keypair,
    secret: RecipientSecret,
    opening: PreparedPayload,
    context: DeliveryContext,
    registration: RecipientRegistration,
}

fn fixture(payload: &[u8]) -> Fixture {
    let sender = signer(1);
    let validator = signer(2);
    let secret = RecipientSecret::generate().unwrap();
    let opening = PreparedPayload::new(
        CommitmentContext {
            network_genesis: [3; 32],
            netuid: 27,
            epoch: 42,
            task_id: [4; 32],
            miner_hotkey: sender.public.to_bytes(),
            beacon_chain: [5; 32],
            beacon_round: 901,
        },
        payload,
    )
    .unwrap();
    let (context, registration) = recipient(&opening, &validator, &secret, 6);
    Fixture {
        sender,
        validator,
        secret,
        opening,
        context,
        registration,
    }
}

fn recipient(
    opening: &PreparedPayload,
    validator: &Keypair,
    secret: &RecipientSecret,
    key_id: u8,
) -> (DeliveryContext, RecipientRegistration) {
    let context = DeliveryContext {
        committed: opening.context.clone(),
        commitment: opening.commitment(),
        recipient_hotkey: validator.public.to_bytes(),
        key_id: [key_id; 32],
        recipient_public_key: secret.public_key(),
        not_before: 100,
        expires_at: 200,
    };
    let registration = RecipientKeyRecord {
        network_genesis: context.committed.network_genesis,
        netuid: context.committed.netuid,
        epoch: context.committed.epoch,
        validator_hotkey: context.recipient_hotkey,
        key_id: context.key_id,
        public_key: context.recipient_public_key,
        not_before: 80,
        expires_at: 300,
    }
    .sign(validator)
    .unwrap();
    (context, registration)
}

fn sealed(f: &Fixture) -> Envelope {
    seal(
        &f.opening,
        &f.registration,
        f.context.clone(),
        &f.sender,
        NOW,
    )
    .unwrap()
}

fn failure<T>(result: Result<T>) -> String {
    match result {
        Ok(_) => panic!("Invalid synthetic envelope was accepted"),
        Err(error) => {
            let message = error.to_string();
            assert!(!message.contains(PRIVATE_MARKER));
            message
        }
    }
}

// Deliberately bypass the production frame builder, while retaining valid
// HPKE authentication and the real synthetic sender signature. This exercises
// plaintext validation rather than merely detecting corrupted ciphertext.
fn seal_frame(f: &Fixture, frame: &[u8]) -> Envelope {
    let public =
        <Kem as hpke::Kem>::PublicKey::from_bytes(&f.context.recipient_public_key).unwrap();
    let (encapped, ciphertext) = hpke::single_shot_seal::<ChaCha20Poly1305, HkdfSha256, Kem>(
        &OpModeS::Base,
        &public,
        HPKE_INFO,
        frame,
        &f.context.encode(),
    )
    .unwrap();
    let mut envelope = Envelope {
        version: 1,
        context: f.context.clone(),
        encapsulated_key: encapped.to_bytes().into(),
        ciphertext,
        sender_signature: Vec::new(),
    };
    envelope.sender_signature = hotkey_signature::sign(&f.sender, &envelope.signing_bytes());
    envelope
}

#[test]
fn exact_opaque_bytes_round_trip_for_two_recipients_and_wrong_key_fails() {
    let bytes = " {\r\n  \"note\": \"Café 東京\", \"unknown\": [1, 2] } \r\n".as_bytes();
    let f = fixture(bytes);
    let first = sealed(&f);
    let encoded = serde_json::to_vec(&first).unwrap();
    let decoded = Envelope::from_json(&encoded).unwrap();
    assert_eq!(
        open(&decoded, &f.context, &f.registration, &f.secret, NOW)
            .unwrap()
            .as_bytes(),
        bytes
    );

    let other_secret = RecipientSecret::generate().unwrap();
    let (other_context, other_registration) = recipient(&f.opening, &signer(7), &other_secret, 8);
    let other = seal(
        &f.opening,
        &other_registration,
        other_context.clone(),
        &f.sender,
        NOW,
    )
    .unwrap();
    assert_eq!(
        open(
            &other,
            &other_context,
            &other_registration,
            &other_secret,
            NOW
        )
        .unwrap()
        .as_bytes(),
        bytes
    );
    assert!(
        failure(open(
            &first,
            &f.context,
            &f.registration,
            &other_secret,
            NOW
        ))
        .contains("private key differs")
    );
    assert_ne!(first.encapsulated_key, other.encapsulated_key);
    assert_ne!(first.encapsulated_key, sealed(&f).encapsulated_key);

    // This pure primitive has no persistent replay ledger; callers must supply
    // one. Reopening the same valid envelope remains deterministic.
    assert_eq!(
        open(&first, &f.context, &f.registration, &f.secret, NOW)
            .unwrap()
            .as_bytes(),
        bytes
    );
}

#[test]
fn authenticated_binding_ciphertext_registration_and_validity_are_required() {
    let f = fixture(PRIVATE_MARKER.as_bytes());
    let original = sealed(&f);
    let mutations: [fn(&mut Envelope); 6] = [
        |e| e.context.committed.task_id[0] ^= 1,
        |e| e.context.committed.miner_hotkey = signer(9).public.to_bytes(),
        |e| e.context.recipient_hotkey = signer(10).public.to_bytes(),
        |e| e.encapsulated_key[0] ^= 1,
        |e| e.ciphertext[0] ^= 1,
        |e| e.sender_signature[0] ^= 1,
    ];
    for mutate in mutations {
        let mut changed = original.clone();
        mutate(&mut changed);
        failure(open(&changed, &f.context, &f.registration, &f.secret, NOW));
    }
    let mut wrong_assignment = f.context.clone();
    wrong_assignment.committed.beacon_round += 1;
    failure(open(
        &original,
        &wrong_assignment,
        &f.registration,
        &f.secret,
        NOW,
    ));
    failure(seal(
        &f.opening,
        &f.registration,
        f.context.clone(),
        &signer(9),
        NOW,
    ));

    let mut invalid_registration = f.registration.clone();
    invalid_registration.record.public_key[0] ^= 1;
    failure(open(
        &original,
        &f.context,
        &invalid_registration,
        &f.secret,
        NOW,
    ));
    let mut other_key_record = f.registration.record.clone();
    other_key_record.key_id[0] ^= 1;
    let other_key_registration = other_key_record.sign(&f.validator).unwrap();
    failure(open(
        &original,
        &f.context,
        &other_key_registration,
        &f.secret,
        NOW,
    ));
    let mut short_record = f.registration.record.clone();
    short_record.expires_at = NOW;
    let short_registration = short_record.sign(&f.validator).unwrap();
    failure(open(
        &original,
        &f.context,
        &short_registration,
        &f.secret,
        NOW,
    ));
    for now in [f.context.not_before - 1, f.context.expires_at] {
        failure(open(&original, &f.context, &f.registration, &f.secret, now));
    }
    assert!(open(&original, &f.context, &f.registration, &f.secret, 100).is_ok());
    assert!(open(&original, &f.context, &f.registration, &f.secret, 199).is_ok());
}

#[test]
fn valid_hpke_and_sender_signature_cannot_replace_the_committed_opening() {
    let f = fixture(PRIVATE_MARKER.as_bytes());
    assert!(
        open(
            &seal_frame(&f, &f.opening.frame()),
            &f.context,
            &f.registration,
            &f.secret,
            NOW,
        )
        .is_ok()
    );
    for changed_offset in [8, FRAME_HEADER] {
        let mut frame = f.opening.frame();
        frame[changed_offset] ^= 1;
        let envelope = seal_frame(&f, &frame);
        assert!(
            failure(open(&envelope, &f.context, &f.registration, &f.secret, NOW))
                .contains("does not match commitment")
        );
    }
}

#[test]
fn malformed_json_size_bounds_and_authenticated_bad_padding_fail_safely() {
    let f = fixture(PRIVATE_MARKER.as_bytes());
    failure(Envelope::from_json(PRIVATE_MARKER.as_bytes()));
    failure(Envelope::from_json(&vec![
        b' ';
        MAX_CIPHERTEXT_BYTES * 5 + 8193
    ]));
    let mut unknown = serde_json::to_value(sealed(&f)).unwrap();
    unknown["unexpected_private_field"] = json!(PRIVATE_MARKER);
    failure(Envelope::from_json(&serde_json::to_vec(&unknown).unwrap()));
    let mut malformed = sealed(&f);
    malformed.ciphertext.pop();
    failure(Envelope::from_json(
        &serde_json::to_vec(&malformed).unwrap(),
    ));
    failure(PreparedPayload::new(f.opening.context.clone(), &[]));
    failure(PreparedPayload::new(
        f.opening.context.clone(),
        &vec![0; MAX_PAYLOAD_BYTES + 1],
    ));

    let mut frames = Vec::new();
    let mut magic = f.opening.frame();
    magic[0] ^= 1;
    frames.push(magic);
    let mut zero_length = f.opening.frame();
    zero_length[40..44].copy_from_slice(&0u32.to_be_bytes());
    frames.push(zero_length);
    let mut over_length = f.opening.frame();
    over_length[40..44].copy_from_slice(&((MAX_PAYLOAD_BYTES + 1) as u32).to_be_bytes());
    frames.push(over_length);
    let mut padding = f.opening.frame();
    *padding.last_mut().unwrap() = 1;
    frames.push(padding);
    let mut extra_bucket = f.opening.frame();
    let extra_len = extra_bucket.len() + PADDING_BUCKET_BYTES;
    extra_bucket.resize(extra_len, 0);
    frames.push(extra_bucket);
    for frame in frames {
        failure(open(
            &seal_frame(&f, &frame),
            &f.context,
            &f.registration,
            &f.secret,
            NOW,
        ));
    }
}

#[test]
fn canonical_wire_vectors_bind_domains_field_order_exact_bytes_and_private_salt() {
    let context = CommitmentContext {
        network_genesis: [0x11; 32],
        netuid: 0x1234,
        epoch: 0x0102_0304_0506_0708,
        task_id: [0x22; 32],
        miner_hotkey: [0x33; 32],
        beacon_chain: [0x44; 32],
        beacon_round: 0x1112_1314_1516_1718,
    };
    // These vectors were computed from the literal domain, fixed-width fields
    // in declared order, big-endian lengths, and payload bytes independently
    // of encode(), signing_bytes(), and commitment().
    let payload = b"{ \"x\": 1 }\r\n";
    assert_eq!(
        hex::encode(commitment(&context, &[0x55; 32], payload)),
        "88c7963a0d8e04abb222fabba4f6d5c9a910d8d6511cba225ccdf26c2d07081c"
    );
    let envelope = Envelope {
        version: 1,
        context: DeliveryContext {
            committed: context.clone(),
            commitment: [0x99; 32],
            recipient_hotkey: [0x66; 32],
            key_id: [0x77; 32],
            recipient_public_key: [0x88; 32],
            not_before: 0x2122_2324_2526_2728,
            expires_at: 0x3132_3334_3536_3738,
        },
        encapsulated_key: [0xaa; 32],
        ciphertext: vec![1, 2, 3],
        sender_signature: vec![0; 64],
    };
    assert_eq!(
        hex::encode(Sha256::digest(envelope.signing_bytes())),
        "99f570b5dddced25636e3d6d03c112a637f941a61c68cfa97da26c931d0945e6"
    );
    assert_ne!(
        commitment(&context, &[0x55; 32], payload),
        commitment(&context, &[0x56; 32], payload)
    );
    assert_ne!(
        commitment(&context, &[0x55; 32], payload),
        commitment(&context, &[0x55; 32], b"{\"x\":1}")
    );
    let f = fixture(PRIVATE_MARKER.as_bytes());
    let another =
        PreparedPayload::new(f.opening.context.clone(), PRIVATE_MARKER.as_bytes()).unwrap();
    assert_ne!(f.opening.commitment(), another.commitment());
    let public = serde_json::to_value(sealed(&f)).unwrap();
    assert!(public.get("salt").is_none());
    assert!(public.get("payload").is_none());
    assert!(
        !serde_json::to_string(&public)
            .unwrap()
            .contains(PRIVATE_MARKER)
    );
    assert_eq!(f.opening.frame().len(), PADDING_BUCKET_BYTES);
}
