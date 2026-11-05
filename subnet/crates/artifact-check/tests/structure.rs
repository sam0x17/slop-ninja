//! Decision D5: the graph class check compares the graph's structure stream
//! byte for byte with the class's pinned reference stream, and hashes
//! nothing.
//!
//! - Each shipped reference stream is its class's record: its length and file
//!   digest are the spec v4 record's, and the spec v1 to v3 structure digest
//!   (SHA-256 of the digest tag and the stream) is the class's digest, so the
//!   comparison and the digest accept the same graphs.
//! - Over a corpus of synthetic graphs, every single-byte mutation of one and
//!   every chunking, the comparison accepts exactly what the digest rule
//!   accepts (the digest is computed here, in the test, from the walk's kept
//!   stream).
//! - A mutated structure byte is refused, at its stream offset.
//! - The comparison never sees a FLOAT initializer payload.
//!
//! `tests/r0.rs` and `tests/spec_v3.rs` run the comparison over the real
//! reference graphs (with the private model directories set).

mod common;

use common::*;
use sha2::{Digest as _, Sha256};
use slop_ninja_artifact_check::onnx::{self, GraphScan, STRUCTURE_DIGEST_TAG};
use slop_ninja_artifact_check::spec::{self, Version};
use slop_ninja_artifact_check::{Refusal, files};

fn sha(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// The spec v1 to v3 structure digest of a structure stream.
fn v2_digest(stream: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(STRUCTURE_DIGEST_TAG);
    h.update(stream);
    hex::encode(h.finalize())
}

/// The walk's kept structure stream, or the walk's refusal.
fn kept(bytes: &[u8], chunk: usize) -> Result<(onnx::GraphReport, Vec<u8>), Refusal> {
    let mut s = GraphScan::keeping(bytes.len() as u64, spec::GRAPH_POLICY);
    for c in bytes.chunks(chunk.max(1)) {
        s.update(c)?;
    }
    s.finish_keeping()
}

/// The walk comparing with `reference`.
fn against(bytes: &[u8], reference: &[u8], chunk: usize) -> Result<onnx::GraphReport, Refusal> {
    let mut s = GraphScan::against(bytes.len() as u64, spec::GRAPH_POLICY, reference);
    for c in bytes.chunks(chunk.max(1)) {
        s.update(c)?;
    }
    s.finish()
}

#[test]
fn each_shipped_reference_stream_is_its_classes_record() {
    for v in Version::ALL {
        for class in v.classes() {
            let r = spec::reference_structure(class).expect("every class ships its stream");
            assert_eq!(r.len() as u64, class.structure_bytes, "{}", class.id);
            assert_eq!(sha(r), class.structure_stream_sha256, "{}", class.id);
            // The digest spec v1 to v3 state the rule by is this stream's, so
            // the comparison accepts exactly the graphs the digest accepts.
            assert_eq!(v2_digest(r), class.structure_sha256, "{}", class.id);
            // The stream leaves out the initializer floats, so it is far
            // shorter than the file it fixes.
            assert!(class.structure_bytes < class.bytes / 100, "{}", class.id);
        }
        // Each class of a version has its own length, so a graph's length
        // names the one class whose reference it is compared with.
        let mut lens: Vec<u64> = v.classes().iter().map(|g| g.bytes).collect();
        lens.sort();
        lens.dedup();
        assert_eq!(lens.len(), v.classes().len(), "{}", v.id());
    }
    let undefined = spec::GraphClass { id: "a-other-v1", ..spec::GRAPHS[0] };
    assert_eq!(spec::reference_structure(&undefined), None);
}

/// Synthetic graphs: the base, other weights, and graphs that differ in a
/// float constant, a shape, a name, an opset, an integer constant or a
/// split `float_data`.
fn corpus() -> Vec<(&'static str, Vec<u8>)> {
    let int_const = |v: i64| {
        small_graph_with(
            [1., 2., 3., 4.],
            0.5,
            |g| g.initializers.push(int_tensor("k", &[1], &[v])),
            &[("", 17)],
            &[],
        )
    };
    vec![
        ("base", small_graph([1., 2., 3., 4.], 0.5)),
        ("weights", small_graph([-7., 0., 1e-30, 9.], 0.5)),
        ("weights-max", small_graph([f32::MAX, f32::MIN, -0., f32::MIN_POSITIVE], 0.5)),
        ("constant", small_graph([1., 2., 3., 4.], -2.)),
        (
            "shape",
            small_graph_with(
                [1., 2., 3., 4.],
                0.5,
                |g| g.initializers = vec![float_tensor("w", &[4, 1], &[1., 2., 3., 4.])],
                &[("", 17)],
                &[],
            ),
        ),
        (
            "name",
            small_graph_with(
                [1., 2., 3., 4.],
                0.5,
                |g| g.initializers = vec![float_tensor("v", &[2, 2], &[1., 2., 3., 4.])],
                &[("", 17)],
                &[],
            ),
        ),
        ("opset", small_graph_with([1., 2., 3., 4.], 0.5, |_| {}, &[("", 18)], &[])),
        ("int-1", int_const(1)),
        ("int-2", int_const(2)),
    ]
}

/// The two rules on one graph against one reference: `Err` for a walk
/// refusal (which both rules share), else whether each accepts.
fn rules(bytes: &[u8], reference: &[u8], chunk: usize) -> Result<(bool, bool), Refusal> {
    let (k, stream) = kept(bytes, chunk)?;
    let a = against(bytes, reference, chunk).expect("the walk does not depend on the reference");
    // The kept and the compared walks see the same stream.
    assert_eq!((a.structure_bytes, k.structure_bytes), (stream.len() as u64, stream.len() as u64));
    assert_eq!(k.structure_match, None);
    let by_digest = v2_digest(&stream) == v2_digest(reference);
    let by_bytes = a.structure_match == Some(true);
    assert_eq!(a.structure_difference.is_none(), by_bytes);
    Ok((by_digest, by_bytes))
}

#[test]
fn the_comparison_accepts_exactly_what_the_digest_accepts() {
    let corpus = corpus();
    let (mut accepted, mut refused, mut walk_refused) = (0, 0, 0);
    for (ref_name, ref_graph) in &corpus {
        let (_, reference) = kept(ref_graph, 1 << 20).unwrap();
        for (name, g) in &corpus {
            for chunk in [1, 3, 7, 1 << 20] {
                let (d, b) = rules(g, &reference, chunk).unwrap();
                assert_eq!(d, b, "{name} against {ref_name}, chunk {chunk}");
            }
            let (d, _) = rules(g, &reference, 1 << 20).unwrap();
            if d {
                accepted += 1;
            } else {
                refused += 1;
            }
        }
        // Every single-byte change of the reference graph: the walk refuses
        // it (both rules refuse), or both rules decide it alike.
        for i in 0..ref_graph.len() {
            for flip in [0x01u8, 0x80] {
                let mut m = ref_graph.clone();
                m[i] ^= flip;
                match rules(&m, &reference, 1 << 20) {
                    Ok((d, b)) => {
                        assert_eq!(d, b, "{ref_name} byte {i} ^ {flip:#x}");
                        if d { accepted += 1 } else { refused += 1 }
                    }
                    Err(_) => walk_refused += 1,
                }
            }
        }
    }
    // Weight-only variants match each other; everything else is its own class.
    assert!(accepted > corpus.len() && refused > corpus.len() && walk_refused > 0, "{accepted} {refused} {walk_refused}");
    eprintln!("{accepted} accepted, {refused} refused by both rules, {walk_refused} refused by the walk");
}

#[test]
fn a_mutated_structure_byte_is_refused_at_its_stream_offset() {
    let base = small_graph([1., 2., 3., 4.], 0.5);
    let (_, reference) = kept(&base, 1 << 20).unwrap();
    // Stream offset of each file byte, or None inside an elided payload.
    let payload = elided_range(&base);
    let mut offset = 0u64;
    let mut stream_at = Vec::with_capacity(base.len());
    for i in 0..base.len() {
        if payload.contains(&i) {
            stream_at.push(None);
        } else {
            stream_at.push(Some(offset));
            offset += 1;
        }
    }
    assert_eq!(offset, reference.len() as u64);
    let mut refused = 0;
    for (i, at) in stream_at.iter().enumerate() {
        let mut m = base.clone();
        m[i] ^= 0x01;
        let Ok(r) = against(&m, &reference, 5) else {
            continue;
        };
        match at {
            Some(at) => {
                // Same length, so the first difference is this byte.
                assert_eq!((r.structure_match, r.structure_difference), (Some(false), Some(*at)), "byte {i}");
                refused += 1;
            }
            None => assert_eq!(r.structure_match, Some(true), "payload byte {i}"),
        }
    }
    assert!(refused > 10, "{refused}");
    // A stream that ends early or runs on differs where the shorter ends.
    let r = against(&base, &reference[..reference.len() - 1], 7).unwrap();
    assert_eq!((r.structure_match, r.structure_difference), (Some(false), Some(reference.len() as u64 - 1)));
    let mut longer = reference.clone();
    longer.push(0);
    let r = against(&base, &longer, 7).unwrap();
    assert_eq!((r.structure_match, r.structure_difference), (Some(false), Some(reference.len() as u64)));
}

/// File range of the FLOAT initializer payload of a graph built by
/// `small_graph` (its one initializer's `raw_data`), found by its encoded
/// values.
fn elided_range(g: &[u8]) -> std::ops::Range<usize> {
    let raw: Vec<u8> = [1f32, 2., 3., 4.].iter().flat_map(|v| v.to_le_bytes()).collect();
    let at = g.windows(raw.len()).position(|w| w == raw).expect("the weights");
    assert_eq!(g.windows(raw.len()).filter(|w| *w == raw).count(), 1);
    at..at + raw.len()
}

#[test]
fn the_comparison_reads_no_float_initializer_payload() {
    // Weights with a byte pattern that occurs nowhere else in the file.
    let mark = f32::from_le_bytes([0x5a, 0xa5, 0x3c, 0x7e]);
    assert!(mark.is_finite());
    let marked = small_graph([mark; 4], 0.5);
    let pattern: Vec<u8> = [mark; 4].iter().flat_map(|v| v.to_le_bytes()).collect();
    assert!(marked.windows(pattern.len()).any(|w| w == pattern));
    let (r, stream) = kept(&marked, 3).unwrap();
    // The stream is the file less the 16 payload bytes, and holds none of them.
    assert_eq!(r.structure_bytes, marked.len() as u64 - 16);
    assert!(!stream.windows(4).any(|w| w == [0x5a, 0xa5, 0x3c, 0x7e]));
    // So a reference from other weights matches, and the reference holds
    // no float of either graph's weights.
    let (_, reference) = kept(&small_graph([0.; 4], 0.5), 1 << 20).unwrap();
    assert_eq!(reference, stream);
    assert_eq!(against(&marked, &reference, 1).unwrap().structure_match, Some(true));
    // The walk still reads the payload: a non-finite weight is refused.
    assert!(matches!(
        against(&small_graph([1., f32::NAN, 3., 4.], 0.5), &reference, 1),
        Err(Refusal::GraphMalformed(_))
    ));
    // A float constant is not an initializer: it is in the stream.
    let (_, constant) = kept(&small_graph([0.; 4], f32::from_le_bytes([0x5a, 0xa5, 0x3c, 0x3e])), 1 << 20).unwrap();
    assert!(constant.windows(4).any(|w| w == [0x5a, 0xa5, 0x3c, 0x3e]));
}

#[test]
fn a_walk_without_a_reference_compares_nothing_and_a_length_of_no_class_is_refused() {
    let g = small_graph([1., 2., 3., 4.], 0.5);
    let r = onnx::scan(&g, spec::GRAPH_POLICY).unwrap();
    assert_eq!((r.structure_match, r.structure_difference), (None, None));
    assert_eq!(r.structure_bytes, g.len() as u64 - 16);
    // The files check: a graph of no class's length has no reference and is
    // refused without a class; the walk under a version keeps the version's.
    for v in Version::ALL {
        let mut s = files::graph_walk(v, g.len() as u64);
        s.update(&g).unwrap();
        let r = s.finish().unwrap();
        assert_eq!(r.structure_match, None);
        assert_eq!(
            files::walked_class(v, g.len() as u64, &r),
            Err(Refusal::GraphNotAllowed { class: None, difference: None })
        );
        // At a class's length, a different structure is refused naming the
        // class and the first difference.
        for class in v.classes() {
            let at_len = onnx::GraphReport { structure_match: Some(false), structure_difference: Some(9), ..r.clone() };
            let e = files::walked_class(v, class.bytes, &at_len).unwrap_err();
            assert_eq!(e, Refusal::GraphNotAllowed { class: Some(class.id), difference: Some(9) });
            assert_eq!(e.code(), "graph_not_allowed");
            assert!(e.to_string().contains(class.id) && e.to_string().contains("stream byte 9"), "{e}");
            let matched = onnx::GraphReport { structure_match: Some(true), structure_difference: None, ..r.clone() };
            assert_eq!(files::walked_class(v, class.bytes, &matched), Ok(class));
        }
    }
}
