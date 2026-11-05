//! Live-surface behavior against a recorded metadata snapshot and mutations of it.
//!
//! The snapshot is test data only. Nothing here asserts that production code
//! refuses a spec number; several tests assert the opposite.

use super::surface::{self, Adaptation, Ty};
use super::*;
use frame_metadata::{RuntimeMetadata, RuntimeMetadataPrefixed, v15::RuntimeMetadataV15};
use parity_scale_codec::{Decode, Encode};
use rtao_core::codec::Value;
use rtao_primitives::{MechId, NetUid};
use scale_info::{Field, PortableType, TypeDef, TypeDefPrimitive, form::PortableForm};

/// Testnet metadata observed at spec 486 (byte-identical to Finney 486 V15).
const METADATA: &[u8] = include_bytes!("../fixtures/runtime-486/testnet-486.v15.scale");
/// The runtime-487 build of subtensor PR #3294 (try-runtime CI artifact of
/// head `f3654266`, unmerged), extracted offline.
const REAL_487: &[u8] = include_bytes!("../fixtures/runtime-487/pr3294-f3654266.v15.scale");
const PALLET: &str = "SubtensorModule";
const COMMIT: &str = "commit_timelocked_mechanism_weights";
const SET: &str = "set_mechanism_weights";
const COMMIT_ID: &str = "SubtensorModule.commit_timelocked_mechanism_weights";
const SET_ID: &str = "SubtensorModule.set_mechanism_weights";

fn metadata() -> RuntimeMetadataV15 {
    match RuntimeMetadataPrefixed::decode(&mut &METADATA[..])
        .expect("snapshot decodes")
        .1
    {
        RuntimeMetadata::V15(metadata) => metadata,
        _ => panic!("snapshot is V15"),
    }
}

fn bytes_of(metadata: RuntimeMetadataV15) -> Vec<u8> {
    RuntimeMetadataPrefixed(
        frame_metadata::META_RESERVED,
        RuntimeMetadata::V15(metadata),
    )
    .encode()
}

fn handle_at(metadata: RuntimeMetadataV15, spec_version: u32) -> RuntimeHandle {
    RuntimeHandle::parse(&bytes_of(metadata), spec_version, 1, 42).expect("mutated metadata parses")
}

fn handle(metadata: RuntimeMetadataV15) -> RuntimeHandle {
    handle_at(metadata, 486)
}

fn calls_type(metadata: &RuntimeMetadataV15) -> u32 {
    metadata
        .pallets
        .iter()
        .find(|pallet| pallet.name == PALLET)
        .and_then(|pallet| pallet.calls.as_ref())
        .expect("pallet has calls")
        .ty
        .id
}

fn call_fields<'a>(
    metadata: &'a mut RuntimeMetadataV15,
    call: &str,
) -> &'a mut Vec<Field<PortableForm>> {
    let id = calls_type(metadata);
    let ty = metadata
        .types
        .types
        .iter_mut()
        .find(|ty| ty.id == id)
        .expect("calls type");
    let TypeDef::Variant(variants) = &mut ty.ty.type_def else {
        panic!("calls type is an enum")
    };
    &mut variants
        .variants
        .iter_mut()
        .find(|variant| variant.name == call)
        .expect("call present")
        .fields
}

fn type_id(metadata: &RuntimeMetadataV15, wanted: impl Fn(&PortableType) -> bool) -> u32 {
    metadata
        .types
        .types
        .iter()
        .find(|ty| wanted(ty))
        .expect("type present")
        .id
}

fn primitive(metadata: &RuntimeMetadataV15, primitive: TypeDefPrimitive) -> u32 {
    type_id(metadata, |ty| {
        ty.ty.type_def == TypeDef::Primitive(primitive.clone())
    })
}

fn option_type(metadata: &RuntimeMetadataV15) -> u32 {
    type_id(metadata, |ty| {
        ty.ty.path.segments.last().map(String::as_str) == Some("Option")
            && matches!(&ty.ty.type_def, TypeDef::Variant(v) if v.variants.len() == 2)
    })
}

fn field(name: &str, ty: u32) -> Field<PortableForm> {
    Field {
        name: Some(name.into()),
        ty: ty.into(),
        type_name: None,
        docs: Vec::new(),
    }
}

fn operation(id: &str) -> surface::Operation {
    surface::weight_operations()
        .into_iter()
        .find(|operation| operation.expected.id() == id)
        .expect("known operation")
}

fn commit_values() -> Vec<Value> {
    vec![
        Value::Int(7),
        Value::Int(0),
        Value::Bytes(vec![1, 2, 3, 4]),
        Value::Int(123_456),
        Value::Int(4),
    ]
}

/// The pinned rtao builder, exactly as its weight scheduler composes the call.
fn rtao_commit(runtime: &RuntimeHandle) -> rtao_runtime::CallBytes {
    rtao_runtime::calls::subtensor_module::commit_timelocked_mechanism_weights(
        NetUid(7),
        MechId(0),
        vec![1, 2, 3, 4],
        123_456,
        4,
    )
    .compose(runtime)
    .expect("unchanged metadata composes")
}

#[test]
fn expected_operations_match_the_recorded_metadata_exactly() {
    let runtime = handle(metadata());
    for operation in surface::weight_operations() {
        assert_eq!(
            surface::evaluate(&runtime, &operation),
            Ok(Vec::new()),
            "{}",
            operation.expected.id()
        );
    }
    let readiness = evaluate_operations(&runtime);
    // Both signed operations and rtao's finalized observation.
    assert_eq!(readiness.len(), 3);
    assert!(
        readiness
            .values()
            .all(|state| state.state == ReadinessState::Ready)
    );
}

#[test]
fn a_new_spec_number_with_identical_interfaces_proceeds() {
    let baseline = evaluate_operations(&handle(metadata()));
    for spec_version in [0, 1, 481, 482, 487, 500, 10_000, u32::MAX] {
        let runtime = handle_at(metadata(), spec_version);
        let readiness = evaluate_operations(&runtime);
        for (name, state) in &readiness {
            assert_eq!(
                state.state,
                ReadinessState::Ready,
                "{name} at {spec_version}"
            );
            assert_eq!(state.observed.spec_version, spec_version);
            assert_eq!(state.adaptations, baseline[name].adaptations);
        }
        let call = rtao_commit(&runtime);
        assert_eq!(check_live_call(&runtime, &call), Ok(()));
    }
}

#[test]
fn our_encoding_equals_the_rtao_builder_and_type_checks() {
    let runtime = handle(metadata());
    let ours = surface::encode(
        runtime.core(),
        &operation(COMMIT_ID).expected,
        commit_values(),
    )
    .expect("encodes");
    assert!(ours.resolution.adaptations.is_empty());
    assert_eq!(ours.bytes, rtao_commit(&runtime).as_bytes());
    let (pallet, call, args) = surface::decode_named(runtime.core(), &ours.bytes).unwrap();
    assert_eq!((pallet.as_str(), call.as_str()), (PALLET, COMMIT));
    let names: Vec<_> = args.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        [
            "netuid",
            "mecid",
            "commit",
            "reveal_round",
            "commit_reveal_version"
        ]
    );
}

#[test]
fn direct_weight_calls_pass_the_same_live_check() {
    let runtime = handle(metadata());
    let call = rtao_runtime::calls::subtensor_module::set_mechanism_weights(
        NetUid(7),
        MechId(0),
        vec![1, 5, 9],
        vec![10, 0, 65_535],
        3,
    )
    .compose(&runtime)
    .expect("unchanged metadata composes");
    assert_eq!(check_live_call(&runtime, &call), Ok(()));
}

#[test]
fn moved_pallet_and_call_indices_resolve_by_name() {
    let original = handle(metadata());
    let before = surface::resolve(original.core(), &operation(COMMIT_ID).expected).unwrap();

    let mut moved = metadata();
    let pallet = moved
        .pallets
        .iter_mut()
        .find(|pallet| pallet.name == PALLET)
        .unwrap();
    pallet.index = 250;
    let outer = moved.outer_enums.call_enum_ty.id;
    if let Some(TypeDef::Variant(variants)) = moved
        .types
        .types
        .iter_mut()
        .find(|ty| ty.id == outer)
        .map(|ty| &mut ty.ty.type_def)
        && let Some(variant) = variants.variants.iter_mut().find(|v| v.name == PALLET)
    {
        variant.index = 250;
    }
    let calls = calls_type(&moved);
    if let Some(TypeDef::Variant(variants)) = moved
        .types
        .types
        .iter_mut()
        .find(|ty| ty.id == calls)
        .map(|ty| &mut ty.ty.type_def)
    {
        let commit = variants
            .variants
            .iter()
            .position(|v| v.name == COMMIT)
            .unwrap();
        let set = variants
            .variants
            .iter()
            .position(|v| v.name == SET)
            .unwrap();
        let (a, b) = (
            variants.variants[commit].index,
            variants.variants[set].index,
        );
        variants.variants[commit].index = b;
        variants.variants[set].index = a;
    }
    let runtime = handle(moved);
    let after = surface::resolve(runtime.core(), &operation(COMMIT_ID).expected).unwrap();
    assert_eq!(after.pallet_index, 250);
    assert_ne!(after.call_index, before.call_index);
    assert!(after.adaptations.is_empty());
    let encoded = surface::encode(
        runtime.core(),
        &operation(COMMIT_ID).expected,
        commit_values(),
    )
    .unwrap();
    assert_eq!(&encoded.bytes[..2], &[250, after.call_index]);
    let (pallet, call, _) = surface::decode_named(runtime.core(), &encoded.bytes).unwrap();
    assert_eq!((pallet.as_str(), call.as_str()), (PALLET, COMMIT));

    // rtao composes a call whose live index moved but whose shape is
    // unchanged through its compiled descriptor, so nothing waits.
    let readiness = evaluate_operations(&runtime);
    assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Ready);
    assert_eq!(readiness[SET_ID].state, ReadinessState::Ready);
}

#[test]
fn an_added_trailing_option_argument_is_encoded_as_none_and_recorded() {
    let mut changed = metadata();
    let option = option_type(&changed);
    call_fields(&mut changed, COMMIT).push(field("memo", option));
    let runtime = handle(changed);
    let encoded = surface::encode(
        runtime.core(),
        &operation(COMMIT_ID).expected,
        commit_values(),
    )
    .unwrap();
    assert_eq!(
        encoded.resolution.adaptations,
        [Adaptation::TrailingOptionNone {
            argument: "memo".into()
        }]
    );
    let original = surface::encode(
        handle(metadata()).core(),
        &operation(COMMIT_ID).expected,
        commit_values(),
    )
    .unwrap();
    assert_eq!(encoded.bytes, [original.bytes.as_slice(), &[0]].concat());
    let (_, _, args) = surface::decode_named(runtime.core(), &encoded.bytes).unwrap();
    assert_eq!(
        args.last().map(|(name, value)| (name.as_str(), value)),
        Some(("memo", &Value::Null))
    );
    // rtao's compiled shape no longer matches, and its name-resolved
    // composition requires the live field names to equal the supplied ones,
    // so rtao refuses this call: that is the deferral, for this call alone.
    let readiness = evaluate_operations(&runtime);
    assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Deferred);
    assert_eq!(
        readiness[COMMIT_ID].deferral.as_ref().unwrap().code,
        "upstream_compose_refused"
    );
    assert_eq!(readiness[SET_ID].state, ReadinessState::Ready);
}

#[test]
fn integer_widening_is_adapted_and_narrowing_defers() {
    let mut widened = metadata();
    let u128_id = primitive(&widened, TypeDefPrimitive::U128);
    call_fields(&mut widened, COMMIT)[3].ty = u128_id.into();
    let runtime = handle(widened);
    let encoded = surface::encode(
        runtime.core(),
        &operation(COMMIT_ID).expected,
        commit_values(),
    )
    .unwrap();
    assert_eq!(
        encoded.resolution.adaptations,
        [Adaptation::Widened {
            argument: "reveal_round".into(),
            from: "u64".into(),
            to: "u128".into()
        }]
    );

    // rtao's compiled shape no longer matches; its by-name composition
    // accepts the wider integer, so the call is adapted, not deferred.
    let readiness = evaluate_operations(&runtime);
    assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Adapted);
    assert_eq!(
        readiness[COMMIT_ID].adaptations.last(),
        Some(&Adaptation::UpstreamComposedByName)
    );

    let mut narrowed = metadata();
    let u8_id = primitive(&narrowed, TypeDefPrimitive::U8);
    call_fields(&mut narrowed, COMMIT)[3].ty = u8_id.into();
    let runtime = handle(narrowed);
    let deferral = surface::encode(
        runtime.core(),
        &operation(COMMIT_ID).expected,
        commit_values(),
    )
    .unwrap_err();
    assert_eq!(deferral.code, "argument_type_changed");
}

#[test]
fn semantic_changes_defer_only_the_affected_operation() {
    type Mutation = Box<dyn Fn(&mut RuntimeMetadataV15)>;
    let cases: Vec<(&str, Mutation)> = vec![
        (
            "call_absent",
            Box::new(|m| {
                let id = calls_type(m);
                if let Some(TypeDef::Variant(v)) = m
                    .types
                    .types
                    .iter_mut()
                    .find(|ty| ty.id == id)
                    .map(|ty| &mut ty.ty.type_def)
                {
                    v.variants.retain(|variant| variant.name != COMMIT);
                }
            }),
        ),
        (
            "argument_renamed_or_removed",
            Box::new(|m| call_fields(m, COMMIT)[3].name = Some("reveal_epoch".into())),
        ),
        (
            "argument_reordered",
            Box::new(|m| call_fields(m, COMMIT).swap(3, 4)),
        ),
        (
            "argument_removed",
            Box::new(|m| {
                call_fields(m, COMMIT).pop();
            }),
        ),
        (
            "required_argument_added",
            Box::new(|m| {
                let u32_id = primitive(m, TypeDefPrimitive::U32);
                call_fields(m, COMMIT).push(field("salt", u32_id));
            }),
        ),
    ];
    for (code, mutate) in cases {
        let mut changed = metadata();
        mutate(&mut changed);
        let runtime = handle(changed);
        let readiness = evaluate_operations(&runtime);
        assert_eq!(
            readiness[COMMIT_ID].state,
            ReadinessState::Deferred,
            "{code}"
        );
        assert_eq!(readiness[COMMIT_ID].deferral.as_ref().unwrap().code, code);
        assert_eq!(readiness[SET_ID].state, ReadinessState::Ready, "{code}");
        let set_values = vec![
            Value::Int(7),
            Value::Int(0),
            Value::List(vec![Value::Int(1)]),
            Value::List(vec![Value::Int(65_535)]),
            Value::Int(3),
        ];
        assert!(surface::encode(runtime.core(), &operation(SET_ID).expected, set_values).is_ok());
    }
}

#[test]
fn a_metadata_change_mid_operation_re_resolves_before_signing() {
    let composed_under = handle_at(metadata(), 486);
    let call = rtao_commit(&composed_under);
    assert_eq!(check_live_call(&composed_under, &call), Ok(()));
    // The chain upgrades between composition and signing.
    let upgraded = handle_at(metadata(), 487);
    assert_eq!(
        check_live_call(&upgraded, &call).unwrap_err().code,
        "runtime_changed_before_signing"
    );
    // Re-composition against the live runtime then passes the same check.
    assert_eq!(check_live_call(&upgraded, &rtao_commit(&upgraded)), Ok(()));
    // An upgrade that removes the call refuses the attempt, never signs stale bytes.
    let mut removed = metadata();
    let id = calls_type(&removed);
    if let Some(TypeDef::Variant(v)) = removed
        .types
        .types
        .iter_mut()
        .find(|ty| ty.id == id)
        .map(|ty| &mut ty.ty.type_def)
    {
        v.variants.retain(|variant| variant.name != COMMIT);
    }
    let removed = handle_at(removed, 488);
    assert!(check_live_call(&removed, &call).is_err());
}

#[test]
fn foreign_or_truncated_call_bytes_never_pass_the_live_check() {
    let runtime = handle(metadata());
    let call = rtao_commit(&runtime);
    for cut in [0, 1, 2, call.as_bytes().len() - 1] {
        let bytes = &call.as_bytes()[..cut];
        assert!(
            surface::decode_named(runtime.core(), bytes).is_err(),
            "{cut}"
        );
    }
    let mut extended = call.as_bytes().to_vec();
    extended.push(0);
    assert!(surface::decode_named(runtime.core(), &extended).is_err());
}

#[test]
fn lowering_flattens_wrappers_and_describes_types() {
    let runtime = handle(metadata());
    let resolution = surface::resolve(runtime.core(), &operation(SET_ID).expected).unwrap();
    assert!(resolution.adaptations.is_empty());
    assert_eq!(Ty::seq(Ty::Uint(16)).to_string(), "Vec<u16>");
    assert_eq!(
        Ty::Option(Box::new(Ty::Array(32, Box::new(Ty::Uint(8))))).to_string(),
        "Option<[u8; 32]>"
    );
}

/// Deterministic xorshift; no new dependency and reproducible failures.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound.max(1) as u64) as usize
    }
}

/// Exercise every live-surface entry point; none may panic on any input.
fn exercise(runtime: &RuntimeHandle, probe: &[u8]) {
    let _ = evaluate_operations(runtime);
    for operation in surface::weight_operations() {
        let _ = surface::resolve(runtime.core(), &operation.expected);
        let values = operation
            .expected
            .args
            .iter()
            .map(|_| Value::Int(1))
            .collect();
        let _ = surface::encode(runtime.core(), &operation.expected, values);
    }
    let _ = surface::decode_named(runtime.core(), probe);
    if let Some(ty) = runtime.core().types.types.first() {
        let _ = surface::lower(runtime.core(), ty.id);
    }
}

#[test]
fn corrupted_metadata_bytes_never_panic() {
    let mut rng = Rng(0x5eed_1234_abcd_0001);
    for round in 0..48 {
        let mut bytes = METADATA.to_vec();
        match round % 3 {
            0 => {
                for _ in 0..1 + rng.below(16) {
                    let at = rng.below(bytes.len());
                    bytes[at] = rng.next() as u8;
                }
            }
            1 => bytes.truncate(rng.below(bytes.len())),
            _ => {
                let at = rng.below(bytes.len());
                let len = rng.below(64);
                let junk: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                bytes.splice(at..at, junk);
            }
        }
        let probe: Vec<u8> = (0..rng.below(96)).map(|_| rng.next() as u8).collect();
        let outcome = std::panic::catch_unwind(|| {
            if let Ok(runtime) = RuntimeHandle::parse(&bytes, 486, 1, 42) {
                exercise(&runtime, &probe);
            }
        });
        assert!(outcome.is_ok(), "panic at round {round}");
    }
}

#[test]
fn randomly_rewired_type_registries_never_panic() {
    let mut rng = Rng(0x0dd_ba11_c0ff_ee01);
    let base = metadata();
    let types = base.types.types.len();
    for round in 0..48 {
        let mut changed = base.clone();
        let commit_fields = call_fields(&mut changed, COMMIT).len();
        for _ in 0..1 + rng.below(6) {
            match rng.below(5) {
                // Point an argument at any type, including out-of-range IDs.
                0 => {
                    let at = rng.below(commit_fields);
                    let target = rng.below(types + 8) as u32;
                    if let Some(field) = call_fields(&mut changed, COMMIT).get_mut(at) {
                        field.ty = target.into();
                    }
                }
                // Make a composite or sequence refer to itself.
                1 => {
                    let at = rng.below(types);
                    let id = changed.types.types[at].id;
                    match &mut changed.types.types[at].ty.type_def {
                        TypeDef::Sequence(sequence) => sequence.type_param = id.into(),
                        TypeDef::Composite(composite) => {
                            if let Some(field) = composite.fields.first_mut() {
                                field.ty = id.into();
                            }
                        }
                        TypeDef::Array(array) => array.type_param = id.into(),
                        _ => {}
                    }
                }
                // Drop a type entirely.
                2 => {
                    let at = rng.below(changed.types.types.len());
                    changed.types.types.remove(at);
                }
                // Scramble argument names.
                3 => {
                    let at = rng.below(commit_fields);
                    if let Some(field) = call_fields(&mut changed, COMMIT).get_mut(at) {
                        field.name = Some(format!("x{}", rng.next() % 1000));
                    }
                }
                // Move the pallet to a random index.
                _ => {
                    if let Some(pallet) = changed.pallets.iter_mut().find(|p| p.name == PALLET) {
                        pallet.index = rng.next() as u8;
                    }
                }
            }
        }
        let bytes = bytes_of(changed);
        let probe: Vec<u8> = (0..rng.below(96)).map(|_| rng.next() as u8).collect();
        let outcome = std::panic::catch_unwind(|| {
            if let Ok(runtime) = RuntimeHandle::parse(&bytes, 486, 1, 42) {
                exercise(&runtime, &probe);
            }
        });
        assert!(outcome.is_ok(), "panic at round {round}");
    }
}

#[test]
fn random_call_bytes_against_valid_metadata_never_panic() {
    let runtime = handle(metadata());
    let real = rtao_commit(&runtime);
    let mut rng = Rng(0xfeed_face_0000_0007);
    for _ in 0..2_000 {
        let mut bytes = real.as_bytes().to_vec();
        for _ in 0..1 + rng.below(4) {
            let at = rng.below(bytes.len());
            bytes[at] = rng.next() as u8;
        }
        bytes.truncate(rng.below(bytes.len() + 1));
        let _ = surface::decode_named(runtime.core(), &bytes);
    }
}

fn without_commit() -> RuntimeMetadataV15 {
    let mut removed = metadata();
    let id = calls_type(&removed);
    if let Some(TypeDef::Variant(v)) = removed
        .types
        .types
        .iter_mut()
        .find(|ty| ty.id == id)
        .map(|ty| &mut ty.ty.type_def)
    {
        v.variants.retain(|variant| variant.name != COMMIT);
    }
    removed
}

#[test]
fn deferral_is_durable_across_restart_and_clears_when_metadata_is_fixed() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    {
        let adapter = open(&path, config())?;
        let summary = adapter.evaluate_runtime(&handle_at(without_commit(), 487), 100)?;
        assert!(summary.alert);
        assert_eq!(summary.deferred.len(), 1);
        assert_eq!(summary.deferred[0].operation, COMMIT_ID);
        assert_eq!(summary.deferred[0].code, "call_absent");
        assert_eq!(summary.deferred[0].since_block, 100);
        // Unchanged evaluations on later blocks do not rewrite the journal.
        let revision = adapter.controls.journal.snapshot()?.revision;
        adapter.evaluate_runtime(&handle_at(without_commit(), 487), 101)?;
        assert_eq!(adapter.controls.journal.snapshot()?.revision, revision);
        // Other work is unaffected: the journal is not paused by a deferral.
        enable_storage_fixture(&adapter)?;
        assert!(!adapter.controls.inspect()?.paused);
    }
    // Restart: the deferral and its first block are retained.
    let adapter = open(&path, config())?;
    let readiness = adapter.controls().readiness()?;
    assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Deferred);
    assert_eq!(readiness[COMMIT_ID].since_block, 100);
    assert_eq!(readiness[SET_ID].state, ReadinessState::Ready);
    let status = adapter.controls().inspect()?;
    assert!(status.readiness.alert);
    // The chain fixes or reverts the metadata: the deferral clears by itself.
    let summary = adapter.evaluate_runtime(&handle_at(metadata(), 488), 102)?;
    assert!(!summary.alert);
    assert!(summary.deferred.is_empty());
    let transitions = adapter.controls().readiness_transitions()?;
    let commits: Vec<_> = transitions
        .iter()
        .filter(|t| t.operation == COMMIT_ID)
        .map(|t| (t.block, t.state, t.code.clone()))
        .collect();
    assert_eq!(
        commits,
        [
            (100, ReadinessState::Deferred, Some("call_absent".into())),
            (102, ReadinessState::Ready, None)
        ]
    );
    Ok(())
}

#[test]
fn adaptations_are_journaled_with_each_state_change() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let adapter = open(&path, config())?;
    let mut evaluated = evaluate_operations(&handle(metadata()));
    let adapted = evaluated.get_mut(COMMIT_ID).context("commit")?;
    adapted.state = ReadinessState::Adapted;
    adapted.adaptations = vec![Adaptation::TrailingOptionNone {
        argument: "memo".into(),
    }];
    let summary = adapter.controls().record_readiness(7, evaluated)?;
    assert_eq!(summary.adapted, [COMMIT_ID]);
    assert!(!summary.alert);
    drop(adapter);
    let adapter = open(&path, config())?;
    let transitions = adapter.controls().readiness_transitions()?;
    let recorded = transitions
        .iter()
        .find(|t| t.operation == COMMIT_ID)
        .context("transition")?;
    assert_eq!(recorded.state, ReadinessState::Adapted);
    assert_eq!(recorded.adaptations.len(), 1);
    assert_eq!(recorded.observed.spec_version, 486);
    Ok(())
}

#[test]
fn readiness_never_enters_operation_identity() -> Result<()> {
    // The same cohort work has the same operation ID before and after any
    // number of runtime upgrades; no runtime field participates.
    let id = work().operation_id(&identity())?;
    for spec_version in [486, 487, 9_999] {
        let _ = evaluate_operations(&handle_at(metadata(), spec_version));
        assert_eq!(work().operation_id(&identity())?, id);
    }
    Ok(())
}

fn storage_entries<'a>(
    metadata: &'a mut RuntimeMetadataV15,
    pallet: &str,
) -> &'a mut Vec<frame_metadata::v15::StorageEntryMetadata<PortableForm>> {
    &mut metadata
        .pallets
        .iter_mut()
        .find(|candidate| candidate.name == pallet)
        .and_then(|candidate| candidate.storage.as_mut())
        .expect("pallet has storage")
        .entries
}

#[test]
fn recorded_metadata_matches_every_storage_item_rtao_reads() {
    let runtime = handle(metadata());
    assert_eq!(surface::evaluate_upstream_storage(&runtime), Ok(Vec::new()));
    assert_eq!(surface::upstream_storage().len(), 23);
    let readiness = evaluate_operations(&runtime);
    assert_eq!(readiness[OBSERVATION_ID].state, ReadinessState::Ready);
}

#[test]
fn a_storage_item_rtao_cannot_read_by_name_defers_observation_and_both_operations() {
    // Removed entry: neither rtao's typed read nor its by-name read finds it.
    let mut removed = metadata();
    storage_entries(&mut removed, PALLET).retain(|entry| entry.name != "StakeThreshold");
    // Narrowed value type: the typed read refuses the shape, and the by-name
    // read refuses a value that no longer fits its class.
    let mut retyped = metadata();
    let narrow = primitive(&retyped, TypeDefPrimitive::U32);
    for entry in storage_entries(&mut retyped, "Drand") {
        if entry.name == "LastStoredRound" {
            entry.ty = frame_metadata::v15::StorageEntryType::Plain(narrow.into());
        }
    }
    for (changed, item) in [
        (removed, "SubtensorModule.StakeThreshold"),
        (retyped, "Drand.LastStoredRound"),
    ] {
        let runtime = handle_at(changed, 487);
        let refusal = surface::evaluate_upstream_storage(&runtime).unwrap_err();
        assert_eq!(refusal.code, "upstream_storage_refused");
        assert!(refusal.detail.contains(item), "{item}: {}", refusal.detail);
        let readiness = evaluate_operations(&runtime);
        for name in [OBSERVATION_ID, COMMIT_ID, SET_ID] {
            assert_eq!(readiness[name].state, ReadinessState::Deferred, "{name}");
            assert_eq!(
                readiness[name].deferral.as_ref().unwrap().code,
                "upstream_storage_refused",
                "{name}"
            );
        }
        // The call descriptors still match this metadata, so call-level
        // evaluation alone says Ready; the storage check is what defers.
        for id in [COMMIT_ID, SET_ID] {
            assert_eq!(surface::evaluate(&runtime, &operation(id)), Ok(Vec::new()));
        }
    }
}

#[test]
fn a_widened_storage_item_is_read_by_name_and_nothing_waits() {
    let mut widened = metadata();
    let wide = primitive(&widened, TypeDefPrimitive::U64);
    // Only the value type widens: a live type of any unsigned width fits the
    // class rtao reads `Tempo` as, and each decoded value is range checked.
    for entry in storage_entries(&mut widened, PALLET) {
        if entry.name == "Tempo"
            && let frame_metadata::v15::StorageEntryType::Map { value, .. } = &mut entry.ty
        {
            *value = wide.into();
            // The same default value, in the wider little-endian encoding.
            entry.default.resize(8, 0);
        }
    }
    let runtime = handle_at(widened, 487);
    assert_eq!(
        surface::evaluate_upstream_storage(&runtime),
        Ok(vec![Adaptation::UpstreamReadByName {
            items: vec!["storage:SubtensorModule.Tempo".into()]
        }])
    );
    let readiness = evaluate_operations(&runtime);
    assert_eq!(readiness[OBSERVATION_ID].state, ReadinessState::Adapted);
    assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Ready);
    assert_eq!(readiness[SET_ID].state, ReadinessState::Ready);
}

/// The real 487 metadata (new HashedAccounts pallet, ProxyType `Lock` and
/// `GammaSpend`, gamma calls) changes nothing we sign or read: every signed
/// operation is ready, the commit call is byte-identical to 486's, and the
/// stored native tag needs nothing more.
#[test]
fn the_real_487_runtime_keeps_every_operation_ready() {
    let old = RuntimeHandle::parse(METADATA, 486, 1, 42).expect("486 parses");
    let new = RuntimeHandle::parse(REAL_487, 487, 1, 42).expect("487 parses");
    let native = u128::from(rtao_neuron::weights::NATIVE_CR_PAYLOAD_VERSION);
    let readiness = evaluate_operations_with(&new, &LiveFacts { commit_reveal_version: Some(native), runtime_code: RuntimeCode::NotNeeded });
    for (operation, state) in &readiness {
        assert_eq!(state.state, ReadinessState::Ready, "{operation}: {state:?}");
        assert_eq!(state.observed.spec_version, 487);
    }
    assert_eq!(surface::evaluate_upstream_storage(&new), Ok(vec![]));
    let commit = |rt: &RuntimeHandle| {
        rtao_runtime::calls::subtensor_module::commit_timelocked_mechanism_weights(NetUid(601), MechId(0), vec![7u8; 96], 123_456, 4)
            .compose(rt)
            .expect("composes")
    };
    assert_eq!(commit(&old).as_bytes(), commit(&new).as_bytes());
    check_live_call(&new, &commit(&new)).expect("the live-call guard admits it under 487");
}

#[test]
fn a_changed_commit_reveal_version_is_recorded_as_rtao_will_treat_it() {
    let runtime = handle(metadata());
    let native = u128::from(rtao_neuron::weights::NATIVE_CR_PAYLOAD_VERSION);
    for version in [None, Some(native)] {
        let readiness = evaluate_operations_with(
            &runtime,
            &LiveFacts {
                commit_reveal_version: version,
                runtime_code: RuntimeCode::NotNeeded,
            },
        );
        assert!(
            readiness
                .values()
                .all(|state| state.state == ReadinessState::Ready),
            "{version:?}"
        );
    }
    // Under runtime code rtao reviewed, rtao follows a changed tag: an
    // adaptation of the commit.
    let reviewed = rtao_neuron::weights::REVIEWED_CR_RUNTIME_CODE
        .iter()
        .map(|(_, code)| <[u8; 32]>::try_from(hex::decode(code.trim_start_matches("0x")).unwrap()).unwrap())
        .collect::<Vec<_>>();
    for code in &reviewed {
        for stored in [native + 1, 0, u128::from(u16::MAX)] {
            let readiness = evaluate_operations_with(
                &runtime,
                &LiveFacts {
                    commit_reveal_version: Some(stored),
                    runtime_code: RuntimeCode::Hash(*code),
                },
            );
            assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Adapted);
            assert_eq!(
                readiness[COMMIT_ID].adaptations,
                [Adaptation::CommitRevealTagChanged {
                    stored: u16::try_from(stored).unwrap()
                }]
            );
            assert_eq!(readiness[SET_ID].state, ReadinessState::Ready);
            assert_eq!(readiness[OBSERVATION_ID].state, ReadinessState::Ready);
        }
    }
    // Under unreviewed code (any runtime after 486, such as 487), rtao at this
    // pin follows only with reveal evidence and otherwise pauses timelocked
    // commits: recorded as a deferral, an alert, never as an adaptation, and
    // nothing else is affected. A code that was not read is unknown, a
    // deferral of its own (the record keeps an earlier state over it).
    let readiness_487 = handle_at(metadata(), 487);
    for (code, expected) in [
        (RuntimeCode::Hash([0x48; 32]), "commit_reveal_tag_unproven"),
        (RuntimeCode::Unread, RUNTIME_CODE_UNREAD),
        (RuntimeCode::NotNeeded, RUNTIME_CODE_UNREAD),
    ] {
        let readiness = evaluate_operations_with(
            &readiness_487,
            &LiveFacts {
                commit_reveal_version: Some(native + 1),
                runtime_code: code,
            },
        );
        assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Deferred, "{code:?}");
        let deferral = readiness[COMMIT_ID].deferral.as_ref().unwrap();
        assert_eq!(deferral.code, expected);
        assert!(deferral.detail.contains("version 5"), "{}", deferral.detail);
        assert_eq!(readiness[SET_ID].state, ReadinessState::Ready);
        assert_eq!(readiness[OBSERVATION_ID].state, ReadinessState::Ready);
    }
    // The native tag needs no code identity under any runtime.
    let readiness = evaluate_operations_with(
        &readiness_487,
        &LiveFacts {
            commit_reveal_version: Some(native),
            runtime_code: RuntimeCode::Hash([0x48; 32]),
        },
    );
    assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Ready);
    // A stored version no u16 tag can carry has no encoding.
    let readiness = evaluate_operations_with(
        &runtime,
        &LiveFacts {
            commit_reveal_version: Some(u128::from(u16::MAX) + 1),
            runtime_code: RuntimeCode::Hash(reviewed[0]),
        },
    );
    assert_eq!(readiness[COMMIT_ID].state, ReadinessState::Deferred);
    assert_eq!(
        readiness[COMMIT_ID].deferral.as_ref().unwrap().code,
        "commit_reveal_version_unencodable"
    );
    assert_eq!(readiness[SET_ID].state, ReadinessState::Ready);
}

/// A failed `:code` read under a non-native tag is unknown, never
/// unreviewed: the record keeps the commit's previous state, so a transient
/// RPC failure neither writes a transition nor raises an alert. Only a read
/// that names an unlisted code defers with the alert, and with no earlier
/// state an unread code is recorded as such.
#[test]
fn an_unread_runtime_code_keeps_the_previous_readiness() -> Result<()> {
    let root = scratch()?;
    let adapter = open(&root.path().join("journal.json"), config())?;
    let runtime = handle(metadata());
    let native = u128::from(rtao_neuron::weights::NATIVE_CR_PAYLOAD_VERSION);
    let reviewed = rtao_neuron::weights::REVIEWED_CR_RUNTIME_CODE
        .iter()
        .map(|(_, code)| <[u8; 32]>::try_from(hex::decode(code.trim_start_matches("0x")).unwrap()).unwrap())
        .next()
        .unwrap();
    let facts = |code| LiveFacts {
        commit_reveal_version: Some(native + 1),
        runtime_code: code,
    };
    // No earlier state: the unread code is recorded as unknown.
    let first = adapter.evaluate_runtime_with(&runtime, &facts(RuntimeCode::Unread), 10)?;
    assert_eq!(first.deferred.len(), 1);
    assert_eq!(first.deferred[0].code, RUNTIME_CODE_UNREAD);
    // Reviewed code: adapted. Then the code read fails: still adapted, no
    // transition, no alert.
    let adapted = adapter.evaluate_runtime_with(&runtime, &facts(RuntimeCode::Hash(reviewed)), 11)?;
    assert!(adapted.deferred.is_empty() && !adapted.alert, "{adapted:?}");
    let transitions = adapter.controls.readiness_transitions()?.len();
    let unread = adapter.evaluate_runtime_with(&runtime, &facts(RuntimeCode::Unread), 12)?;
    assert_eq!(unread, adapted);
    assert_eq!(adapter.controls.readiness_transitions()?.len(), transitions, "no transition written");
    // Unlisted code: deferred with the alert. A failed read keeps that too.
    let unproven = adapter.evaluate_runtime_with(&runtime, &facts(RuntimeCode::Hash([0x48; 32])), 13)?;
    assert!(unproven.alert);
    assert_eq!(unproven.deferred[0].code, COMMIT_REVEAL_TAG_UNPROVEN);
    let transitions = adapter.controls.readiness_transitions()?.len();
    let unread = adapter.evaluate_runtime_with(&runtime, &facts(RuntimeCode::Unread), 14)?;
    assert_eq!(unread, unproven);
    assert_eq!(adapter.controls.readiness_transitions()?.len(), transitions);
    // The tag returns to native: ready, whatever the code read did.
    let ready = adapter.evaluate_runtime_with(
        &runtime,
        &LiveFacts {
            commit_reveal_version: Some(native),
            runtime_code: RuntimeCode::NotNeeded,
        },
        15,
    )?;
    assert!(ready.deferred.is_empty() && ready.adapted.is_empty() && !ready.alert, "{ready:?}");
    Ok(())
}

#[test]
fn an_unreadable_readiness_record_never_gates_inspection_or_recovery() -> Result<()> {
    let root = scratch()?;
    let path = root.path().join("journal.json");
    let adapter = open(&path, config())?;
    adapter.evaluate_runtime(&handle(metadata()), 10)?;
    // A later build added a field: still decodes, nothing is quarantined.
    adapter.controls.journal.update(|image| {
        let log = image.extensions.get_mut(READINESS_KEY).expect("recorded");
        log["operations"][COMMIT_ID]["added_by_a_later_build"] = serde_json::json!(1);
        Ok(())
    })?;
    assert!(!adapter.controls().inspect()?.readiness.unreadable);
    // A later build added an adaptation kind, then was rolled back.
    let raw = adapter.controls.journal.update(|image| {
        let log = image.extensions.get_mut(READINESS_KEY).expect("recorded");
        log["operations"][COMMIT_ID]["adaptations"] =
            serde_json::json!([{ "kind": "a_future_adaptation", "argument": "x" }]);
        Ok(log.clone())
    })?;
    drop(adapter);
    let adapter = open(&path, config())?;
    let status = adapter.controls().inspect()?;
    assert!(status.readiness.unreadable && status.readiness.alert);
    assert!(adapter.controls().readiness()?.is_empty());
    // Recovery's journal steps still run: persist pause, then inspect.
    adapter.controls().pause()?;
    assert!(adapter.controls().inspect()?.paused);
    let summary = adapter.evaluate_runtime(&handle(metadata()), 11)?;
    assert!(!summary.unreadable && !summary.alert);
    let image = adapter.controls.journal.snapshot()?;
    assert_eq!(image.extensions.get(READINESS_QUARANTINE_KEY), Some(&raw));
    assert!(!adapter.controls().inspect()?.readiness.unreadable);
    // A different schema is treated the same way.
    adapter.controls.journal.update(|image| {
        let log = image.extensions.get_mut(READINESS_KEY).expect("recorded");
        log["schema"] = serde_json::json!(2);
        Ok(())
    })?;
    assert!(adapter.controls().inspect()?.readiness.unreadable);
    assert!(
        !adapter
            .evaluate_runtime(&handle(metadata()), 12)?
            .unreadable
    );
    Ok(())
}

#[test]
fn a_scheduler_error_keeps_recovery_only_when_nothing_was_signed() {
    let summary = |attempts, unresolved_attempts, finalized_failures| JournalSummary {
        schema: 3,
        generation: 9,
        paused: false,
        operations: 1,
        attempts,
        unresolved_attempts,
        finalized_failures,
        failures_awaiting_decode: 0,
        reviewed_failures: 0,
        reviewed_contradictions: 0,
        review_required: Vec::new(),
        awaiting_decode: Vec::new(),
        semantic_refusals: 0,
        readiness: ReadinessSummary::default(),
    };
    // A transient RPC error or a storage-surface refusal before any signature:
    // recovery holds whatever readiness says, so the next block retries.
    assert!(keeps_recovery_after_error(0, &summary(0, 0, 0)));
    assert!(keeps_recovery_after_error(2, &summary(2, 0, 0)));
    // Anything signed in that call, or left uncertain, needs a new recovery.
    assert!(!keeps_recovery_after_error(0, &summary(1, 1, 0)));
    assert!(!keeps_recovery_after_error(0, &summary(1, 0, 0)));
    assert!(!keeps_recovery_after_error(1, &summary(1, 1, 0)));
    assert!(!keeps_recovery_after_error(1, &summary(1, 0, 1)));
}
