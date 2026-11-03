//! The ONNX pre-parse on synthetic graphs: accepted structure, the structure
//! digest's invariance to initializer float values only, and every refusal
//! class.

mod common;

use common::*;
use slop_ninja_artifact_check::onnx::{self, GraphScan};
use slop_ninja_artifact_check::{Refusal, spec};

fn scan(bytes: &[u8]) -> Result<onnx::GraphReport, Refusal> {
    onnx::scan(bytes, spec::GRAPH_POLICY)
}

#[test]
fn small_graph_passes_and_counts_parameters() {
    let r = scan(&small_graph([1., 2., 3., 4.], 0.5)).unwrap();
    assert_eq!(r.parameters, 5);
    assert_eq!(r.nodes, 3);
    assert_eq!(r.initializers, 1);
    assert_eq!((r.inputs, r.outputs), (1, 1));
    assert_eq!(r.opsets, vec![(String::new(), 17)]);
    assert_eq!(r.ops.get("MatMul"), Some(&1));
    assert_eq!(r.ir_version, 8);
}

#[test]
fn structure_digest_ignores_initializer_float_values_only() {
    let a = scan(&small_graph([1., 2., 3., 4.], 0.5)).unwrap();
    let b = scan(&small_graph([-7., 0., 1e-30, 9.], 0.5)).unwrap();
    assert_eq!(a.structure_sha256, b.structure_sha256);
    // A float constant (a Constant node tensor, like a band mask or a mask
    // polarity scalar) is structure.
    let constant = scan(&small_graph([1., 2., 3., 4.], -2.)).unwrap();
    assert_ne!(constant.structure_sha256, a.structure_sha256);
    // Constants still count as parameters and must be finite.
    assert_eq!(constant.parameters, a.parameters);
    // A shape, an integer constant, a name or an opset changes it.
    let shape = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers = vec![float_tensor("w", &[4, 1], &[1., 2., 3., 4.])],
        &[("", 17)],
        &[],
    );
    let name = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers = vec![float_tensor("v", &[2, 2], &[1., 2., 3., 4.])],
        &[("", 17)],
        &[],
    );
    let opset = small_graph_with([1., 2., 3., 4.], 0.5, |_| {}, &[("", 18)], &[]);
    let int_const = |v: i64| {
        small_graph_with(
            [1., 2., 3., 4.],
            0.5,
            |g| g.initializers.push(int_tensor("k", &[1], &[v])),
            &[("", 17)],
            &[],
        )
    };
    for other in [shape, name, opset, int_const(1)] {
        assert_ne!(scan(&other).unwrap().structure_sha256, a.structure_sha256);
    }
    assert_ne!(
        scan(&int_const(1)).unwrap().structure_sha256,
        scan(&int_const(2)).unwrap().structure_sha256
    );
}

#[test]
fn chunking_does_not_change_the_result() {
    let bytes = small_graph([1., 2., 3., 4.], 0.5);
    let whole = scan(&bytes).unwrap();
    for size in [1, 2, 3, 5, 7, 64] {
        let mut s = GraphScan::new(bytes.len() as u64, spec::GRAPH_POLICY);
        for c in bytes.chunks(size) {
            s.update(c).unwrap();
        }
        assert_eq!(s.finish().unwrap(), whole, "chunk {size}");
    }
}

#[test]
fn custom_operator_domains_and_functions_are_refused() {
    let custom_node = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| {
            g.nodes.push(node(&Node {
                op: "Add",
                domain: Some("com.example"),
                inputs: &["y", "y"],
                outputs: &["z"],
                attributes: vec![],
            }))
        },
        &[("", 17), ("com.example", 1)],
        &[],
    );
    assert!(matches!(
        scan(&custom_node),
        Err(Refusal::GraphCustomDomain(_))
    ));
    // A custom opset import alone, and a node in the ai.onnx alias, which no
    // allow-listed graph uses.
    let custom_opset = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |_| {},
        &[("", 17), ("com.microsoft", 1)],
        &[],
    );
    assert!(matches!(
        scan(&custom_opset),
        Err(Refusal::GraphCustomDomain(_))
    ));
    let alias = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| {
            g.nodes.push(node(&Node {
                op: "Add",
                domain: Some("ai.onnx"),
                inputs: &["y", "y"],
                outputs: &["z"],
                attributes: vec![],
            }))
        },
        &[("", 17)],
        &[],
    );
    assert!(matches!(scan(&alias), Err(Refusal::GraphCustomDomain(_))));
    // Model-local functions define operators of their own.
    let mut function = Vec::new();
    bytes(&mut function, 25, b"\x0a\x03Foo");
    let with_function = small_graph_with([1., 2., 3., 4.], 0.5, |_| {}, &[("", 17)], &function);
    assert!(matches!(
        scan(&with_function),
        Err(Refusal::GraphCustomDomain(_))
    ));
}

#[test]
fn operators_outside_the_allow_list_are_refused() {
    for op in [
        "Loop",
        "MatMulInteger",
        "LayerNormalizationX",
        "AddAddAddAddAddAddAddAdd",
    ] {
        let g = small_graph_with(
            [1., 2., 3., 4.],
            0.5,
            |g| {
                g.nodes.push(node(&Node {
                    op,
                    domain: None,
                    inputs: &["y"],
                    outputs: &["z"],
                    attributes: vec![],
                }))
            },
            &[("", 17)],
            &[],
        );
        assert!(
            matches!(scan(&g), Err(Refusal::GraphOpNotAllowed(_))),
            "{op}"
        );
    }
}

#[test]
fn external_data_is_refused() {
    // data_location = EXTERNAL.
    let mut t = Vec::new();
    uint(&mut t, 1, 2);
    uint(&mut t, 2, 1);
    bytes(&mut t, 8, b"w");
    uint(&mut t, 14, 1);
    let located = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers = vec![t.clone()],
        &[("", 17)],
        &[],
    );
    assert_eq!(scan(&located), Err(Refusal::GraphExternalData));
    // An external_data entry.
    let mut t = Vec::new();
    uint(&mut t, 1, 2);
    uint(&mut t, 2, 1);
    bytes(&mut t, 8, b"w");
    let mut entry = Vec::new();
    bytes(&mut entry, 1, b"location");
    bytes(&mut entry, 2, b"weights.bin");
    bytes(&mut t, 13, &entry);
    let entry = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers = vec![t.clone()],
        &[("", 17)],
        &[],
    );
    assert_eq!(scan(&entry), Err(Refusal::GraphExternalData));
}

#[test]
fn unknown_fields_subgraphs_and_other_types_are_refused() {
    let mut unknown = Vec::new();
    bytes(&mut unknown, 99, b"padding");
    let g = small_graph_with([1., 2., 3., 4.], 0.5, |_| {}, &[("", 17)], &unknown);
    assert!(matches!(
        scan(&g),
        Err(Refusal::GraphUnknownField { field: 99, .. })
    ));
    let g = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| bytes(&mut g.extra, 3, b"x"),
        &[("", 17)],
        &[],
    );
    assert!(matches!(
        scan(&g),
        Err(Refusal::GraphUnknownField { field: 3, .. })
    ));
    // A subgraph attribute.
    let mut attr = Vec::new();
    bytes(&mut attr, 1, b"body");
    bytes(&mut attr, 6, b"");
    uint(&mut attr, 20, 5);
    let g = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| {
            g.nodes.push(node(&Node {
                op: "Add",
                domain: None,
                inputs: &["y"],
                outputs: &["z"],
                attributes: vec![attr],
            }))
        },
        &[("", 17)],
        &[],
    );
    assert!(matches!(scan(&g), Err(Refusal::GraphUnsupported(_))));
    // Training info, sparse initializers.
    let mut training = Vec::new();
    bytes(&mut training, 20, b"");
    let g = small_graph_with([1., 2., 3., 4.], 0.5, |_| {}, &[("", 17)], &training);
    assert!(matches!(scan(&g), Err(Refusal::GraphUnsupported(_))));
    let g = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| bytes(&mut g.extra, 15, b""),
        &[("", 17)],
        &[],
    );
    assert!(matches!(scan(&g), Err(Refusal::GraphUnsupported(_))));
    // A DOUBLE initializer: not an allow-listed element type.
    let mut t = Vec::new();
    uint(&mut t, 1, 1);
    uint(&mut t, 2, 11);
    bytes(&mut t, 9, &1f64.to_le_bytes());
    let g = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers.push(t.clone()),
        &[("", 17)],
        &[],
    );
    assert_eq!(scan(&g), Err(Refusal::GraphDtypeNotAllowed(11)));
}

#[test]
fn malformed_payloads_are_refused() {
    // Shape [2, 2] with 3 values.
    let mut t = float_tensor("w", &[2, 2], &[1., 2., 3., 4.]);
    t.truncate(t.len() - 4 - 2);
    let mut short = Vec::new();
    uint(&mut short, 1, 2);
    uint(&mut short, 1, 2);
    uint(&mut short, 2, 1);
    bytes(&mut short, 8, b"w");
    bytes(&mut short, 9, &[0u8; 12]);
    let g = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers = vec![short.clone()],
        &[("", 17)],
        &[],
    );
    assert!(matches!(scan(&g), Err(Refusal::GraphMalformed(_))));
    // A non-finite weight.
    let g = small_graph([1., f32::NAN, 3., 4.], 0.5);
    assert!(matches!(scan(&g), Err(Refusal::GraphMalformed(_))));
    let g = small_graph([1., 2., 3., 4.], f32::INFINITY);
    assert!(matches!(scan(&g), Err(Refusal::GraphMalformed(_))));
    // Truncated file, trailing bytes, a length past its message.
    let good = small_graph([1., 2., 3., 4.], 0.5);
    assert!(matches!(
        scan(&good[..good.len() - 1]),
        Err(Refusal::GraphMalformed(_))
    ));
    let mut long = good.clone();
    long.push(0);
    assert!(scan(&long).is_err());
    let mut s = GraphScan::new(good.len() as u64 - 1, spec::GRAPH_POLICY);
    assert!(s.update(&good).is_err());
    // An empty file.
    assert!(matches!(scan(&[]), Err(Refusal::GraphMalformed(_))));
    // A repeated opset import.
    let twice = small_graph_with([1., 2., 3., 4.], 0.5, |_| {}, &[("", 17), ("", 17)], &[]);
    assert!(matches!(scan(&twice), Err(Refusal::GraphMalformed(_))));
    // An element count that overflows.
    let mut t = Vec::new();
    for _ in 0..3 {
        uint(&mut t, 1, u64::MAX >> 2);
    }
    uint(&mut t, 2, 1);
    let overflow = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers.push(t.clone()),
        &[("", 17)],
        &[],
    );
    assert!(matches!(scan(&overflow), Err(Refusal::GraphMalformed(_))));
}

/// A FLOAT initializer whose packed `float_data` is split over two fields.
fn split_float_data(dims: &[u64], first: &[f32], second: &[f32]) -> Vec<u8> {
    let mut t = Vec::new();
    for &d in dims {
        uint(&mut t, 1, d);
    }
    uint(&mut t, 2, 1);
    bytes(&mut t, 8, b"s");
    for part in [first, second] {
        let raw: Vec<u8> = part.iter().flat_map(|v| v.to_le_bytes()).collect();
        bytes(&mut t, 4, &raw);
    }
    t
}

#[test]
fn split_float_data_is_counted_once() {
    let base = scan(&small_graph([1., 2., 3., 4.], 0.5)).unwrap();
    let g = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| {
            g.initializers
                .push(split_float_data(&[4], &[1., 2.], &[3., 4.]))
        },
        &[("", 17)],
        &[],
    );
    let r = scan(&g).unwrap();
    assert_eq!(r.parameters, base.parameters + 4);
    // The split payload is still checked against the shape.
    let g = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| {
            g.initializers
                .push(split_float_data(&[4], &[1., 2.], &[3.]))
        },
        &[("", 17)],
        &[],
    );
    assert!(matches!(scan(&g), Err(Refusal::GraphMalformed(_))));
}

#[test]
fn a_repeated_data_type_is_refused() {
    // FLOAT then INT64: a payload hashed as one type and read as another.
    let mut t = Vec::new();
    uint(&mut t, 1, 2);
    uint(&mut t, 2, 1);
    bytes(&mut t, 8, b"k");
    bytes(&mut t, 9, &[0u8; 16]);
    uint(&mut t, 2, 7);
    let g = small_graph_with(
        [1., 2., 3., 4.],
        0.5,
        |g| g.initializers.push(t.clone()),
        &[("", 17)],
        &[],
    );
    assert!(matches!(scan(&g), Err(Refusal::GraphMalformed(_))));
}

/// A graph whose single initializer declares `n` FLOAT elements. Only the
/// bytes up to the payload are returned, with the declared total length.
fn huge_prefix(n: u64) -> (Vec<u8>, u64) {
    let payload = n * 4;
    let mut t = Vec::new();
    uint(&mut t, 1, n);
    uint(&mut t, 2, 1);
    bytes(&mut t, 8, b"w");
    tag(&mut t, 9, 2);
    varint(&mut t, payload);
    let tensor_len = t.len() as u64 + payload;
    let mut g = Vec::new();
    tag(&mut g, 5, 2);
    varint(&mut g, tensor_len);
    let graph_len = g.len() as u64 + t.len() as u64 + payload;
    let mut m = Vec::new();
    uint(&mut m, 1, 8);
    tag(&mut m, 7, 2);
    varint(&mut m, graph_len);
    let total = m.len() as u64 + g.len() as u64 + t.len() as u64 + payload;
    m.extend_from_slice(&g);
    m.extend_from_slice(&t);
    (m, total)
}

#[test]
fn the_parameter_bound_refuses_before_the_payload_is_read() {
    let (prefix, total) = huge_prefix(spec::MAX_PARAMETERS + 1);
    let mut s = GraphScan::new(total, spec::GRAPH_POLICY);
    assert_eq!(
        s.update(&prefix),
        Err(Refusal::TooManyParameters {
            count: spec::MAX_PARAMETERS + 1,
            max: spec::MAX_PARAMETERS
        })
    );
    // At the bound the header is accepted and the walk waits for the payload.
    let (prefix, total) = huge_prefix(spec::MAX_PARAMETERS);
    let mut s = GraphScan::new(total, spec::GRAPH_POLICY);
    s.update(&prefix).unwrap();
    // Two tensors that pass the bound together: the second is refused at its
    // payload start, after the first was read in full.
    let half = spec::MAX_PARAMETERS / 2 + 1;
    let mut th = Vec::new();
    uint(&mut th, 1, half);
    uint(&mut th, 2, 1);
    tag(&mut th, 9, 2);
    varint(&mut th, half * 4);
    let tensor = |out: &mut Vec<u8>| {
        tag(out, 5, 2);
        varint(out, th.len() as u64 + half * 4);
        out.extend_from_slice(&th);
    };
    let mut s = GraphScan::new(u64::MAX / 2, spec::GRAPH_POLICY);
    let mut first = Vec::new();
    tag(&mut first, 7, 2);
    varint(&mut first, u64::MAX / 4);
    tensor(&mut first);
    s.update(&first).unwrap();
    let zeros = vec![0u8; 1 << 20];
    let mut left = half * 4;
    while left > 0 {
        let k = left.min(zeros.len() as u64) as usize;
        s.update(&zeros[..k]).unwrap();
        left -= k as u64;
    }
    let mut second = Vec::new();
    tensor(&mut second);
    assert!(matches!(
        s.update(&second),
        Err(Refusal::TooManyParameters { .. })
    ));
}
