//! Streaming ONNX pre-parse: a protobuf walk over the graph file that runs
//! before any runtime opens it.
//!
//! The walk knows the subset of `onnx.proto` an allow-listed graph uses and
//! refuses everything else: unknown fields, custom operator domains, local
//! functions, training information, external or sparse tensor data,
//! subgraph attributes, operators and element types outside the allow-list,
//! and tensors whose payload disagrees with their shape. It counts the
//! floating-point parameters and refuses as soon as the count passes the
//! bound, before the payload of the offending tensor is read.
//!
//! **Structure stream** (`slop_ninja_onnx_structure_stream_v1`): the graph
//! file's bytes with the payload of every `FLOAT` *initializer* data field
//! (`raw_data` or `float_data` of a `GraphProto.initializer`) removed. The
//! field's tag and length prefix stay, so the stream is unambiguous and fixes
//! the file length. Two graphs have the same structure stream exactly when
//! they are byte-identical apart from their initializer float values: same
//! operators, order, names, attributes, shapes, integer constants, opsets and
//! producer fields, and the same float constants (`Constant` node tensors such
//! as rotary tables, band masks, mask-polarity scalars and the attention
//! scale). Only the weights are free.
//!
//! The graph class check compares the stream byte for byte with the class's
//! pinned reference stream ([`GraphScan::against`]) as the walk produces it.
//! Nothing is hashed: the walk holds no hasher, and the removed payloads are
//! never passed to the comparison (the walk still reads them, to check that
//! every value is finite). Spec v1 to v3 state the same rule as a digest,
//! `slop_ninja_onnx_structure_v2`: SHA-256 of [`STRUCTURE_DIGEST_TAG`]
//! followed by the stream. Each class's digest is its reference stream's
//! (a spec test recomputes it from the shipped stream), so the comparison
//! accepts exactly the graphs the digest accepts.
//!
//! The walk is a push parser: [`GraphScan::update`] takes chunks of any size
//! and holds only a frame stack (its depth is bounded by the schema, since
//! subgraphs are refused), a few scalars per open message and the operator
//! names it compares with the allow-list. No payload is buffered.

use std::collections::BTreeMap;

use crate::Refusal;

/// Name of the structure digest that spec v1 to v3 state the class rule by.
pub const STRUCTURE_DIGEST_ID: &str = "slop_ninja_onnx_structure_v2";

/// Domain tag of that digest: the digest is SHA-256 of the tag followed by
/// the structure stream. Nothing here computes it; the spec tests recompute
/// each class's from its reference stream.
pub const STRUCTURE_DIGEST_TAG: &[u8] = b"slop_ninja_onnx_structure_v2\0";

/// Name of the structure stream, which spec v4 states the class rule by.
pub const STRUCTURE_STREAM_ID: &str = "slop_ninja_onnx_structure_stream_v1";

/// The initializer value rule ([`GraphPolicy::refuse_subnormal`]): every
/// `FLOAT` initializer value is finite and not a float32 subnormal. A
/// subnormal weight gives a model a constant lever on its arithmetic speed
/// on CPUs without flush-to-zero; refusing them removes that lever only
/// (small normal weights can still produce subnormal intermediate values).
/// Constant node tensors are fixed by the class's structure stream, so the
/// rule reads initializers alone. The rule applies at every load, not only
/// at admission, so a scorer build carrying it would refuse the load of an
/// already admitted artifact that ships one: such a build is deployed only
/// when no admitted artifact is still waiting to be scored (a cohort gap),
/// after every admitted artifact has been measured to pass it. Every later
/// scorer build carries it.
pub const INITIALIZER_RULE: &str = "slop_ninja_onnx_no_subnormal_initializers_v1";

/// ONNX `TensorProto.DataType` values.
pub mod dtype {
    pub const FLOAT: i32 = 1;
    pub const UINT8: i32 = 2;
    pub const INT8: i32 = 3;
    pub const INT32: i32 = 6;
    pub const INT64: i32 = 7;
    pub const BOOL: i32 = 9;
    pub const DOUBLE: i32 = 11;

    /// Bytes per element in `raw_data`, for the types the walk can check.
    pub fn size(t: i32) -> Option<u64> {
        match t {
            FLOAT | INT32 => Some(4),
            UINT8 | INT8 | BOOL => Some(1),
            INT64 | DOUBLE => Some(8),
            _ => None,
        }
    }
}

/// ONNX `AttributeProto.AttributeType` values the walk accepts.
pub mod attr_type {
    pub const FLOAT: i64 = 1;
    pub const INT: i64 = 2;
    pub const STRING: i64 = 3;
    pub const TENSOR: i64 = 4;
    pub const FLOATS: i64 = 6;
    pub const INTS: i64 = 7;
    pub const STRINGS: i64 = 8;
}

/// What the walk accepts. Every value comes from the versioned spec.
#[derive(Clone, Copy, Debug)]
pub struct GraphPolicy<'a> {
    /// Operator domains accepted on nodes and opset imports.
    pub domains: &'a [&'a str],
    /// Operator types accepted in those domains.
    pub ops: &'a [&'a str],
    /// Tensor element types accepted in initializers, constants and the
    /// graph interface.
    pub dtypes: &'a [i32],
    /// Attribute types accepted.
    pub attr_types: &'a [i64],
    /// Largest number of floating-point parameters.
    pub max_parameters: u64,
    /// Refuse a float32 subnormal value in a `FLOAT` initializer
    /// ([`INITIALIZER_RULE`]).
    pub refuse_subnormal: bool,
}

/// Content-free facts about a graph that passed the walk.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct GraphReport {
    /// Length of the structure stream: the file without its initializer
    /// float payloads.
    pub structure_bytes: u64,
    /// Whether the structure stream equals the reference stream the walk
    /// compared it with ([`GraphScan::against`]); `None` without one.
    pub structure_match: Option<bool>,
    /// The stream offset of the first byte that differs from the reference
    /// (or where the shorter of the two ends), when they differ.
    pub structure_difference: Option<u64>,
    /// `FLOAT` tensor elements in initializers and constant attributes.
    pub parameters: u64,
    pub ir_version: u64,
    /// `(domain, version)` pairs, in file order.
    pub opsets: Vec<(String, u64)>,
    pub nodes: u64,
    pub initializers: u64,
    pub inputs: u64,
    pub outputs: u64,
    /// Operator counts by type.
    pub ops: BTreeMap<String, u64>,
    /// Elements by data type, over initializers and constant attributes.
    pub elements_by_dtype: BTreeMap<i32, u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Msg {
    Model,
    Opset,
    Graph,
    Node,
    Attribute,
    Tensor,
    ValueInfo,
    Type,
    TypeTensor,
    Shape,
    Dim,
    Entry,
}

impl Msg {
    fn name(self) -> &'static str {
        match self {
            Msg::Model => "ModelProto",
            Msg::Opset => "OperatorSetIdProto",
            Msg::Graph => "GraphProto",
            Msg::Node => "NodeProto",
            Msg::Attribute => "AttributeProto",
            Msg::Tensor => "TensorProto",
            Msg::ValueInfo => "ValueInfoProto",
            Msg::Type => "TypeProto",
            Msg::TypeTensor => "TypeProto.Tensor",
            Msg::Shape => "TensorShapeProto",
            Msg::Dim => "TensorShapeProto.Dimension",
            Msg::Entry => "StringStringEntryProto",
        }
    }
}

/// How one field of one message is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    /// A nested message.
    Sub(Msg),
    /// A string or bytes value that is compared and otherwise ignored.
    Skip,
    /// A string compared with the allow-list (domain or operator type).
    Capture(Slot),
    /// A single varint scalar, kept in a slot.
    Scalar(Slot),
    /// A varint scalar that is only compared.
    Varint,
    /// A repeated varint: packed or not.
    RepeatedVarint(Slot),
    /// One fixed 32-bit value (attribute `f`).
    Fixed32,
    /// A repeated fixed-width number: packed or not.
    RepeatedFixed(Slot, u8),
    /// `raw_data`.
    Raw,
    /// A field whose presence alone is a refusal.
    Refuse(&'static str),
}

/// Where a scalar or captured value goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    None,
    IrVersion,
    OpsetDomain,
    OpsetVersion,
    NodeDomain,
    NodeOp,
    AttrType,
    TensorDims,
    TensorDataType,
    TensorLocation,
    /// Typed tensor data: `float_data`, `int32_data`, `int64_data`, `double_data`, `uint64_data`.
    TensorTyped(i32),
    ElemType,
}

fn field(msg: Msg, number: u64) -> Option<Field> {
    use Field::*;
    Some(match (msg, number) {
        (Msg::Model, 1) => Scalar(Slot::IrVersion),
        (Msg::Model, 2 | 3 | 4 | 6) => Skip,
        (Msg::Model, 5) => Varint,
        (Msg::Model, 7) => Sub(Msg::Graph),
        (Msg::Model, 8) => Sub(Msg::Opset),
        (Msg::Model, 14) => Sub(Msg::Entry),
        (Msg::Model, 20) => Refuse("training_info"),
        (Msg::Model, 25) => Refuse("functions"),
        (Msg::Model, 26) => Refuse("configuration"),
        (Msg::Opset, 1) => Capture(Slot::OpsetDomain),
        (Msg::Opset, 2) => Scalar(Slot::OpsetVersion),
        (Msg::Graph, 1) => Sub(Msg::Node),
        (Msg::Graph, 2 | 10) => Skip,
        (Msg::Graph, 5) => Sub(Msg::Tensor),
        (Msg::Graph, 11..=13) => Sub(Msg::ValueInfo),
        (Msg::Graph, 14) => Refuse("quantization_annotation"),
        (Msg::Graph, 15) => Refuse("sparse_initializer"),
        (Msg::Graph, 16) => Sub(Msg::Entry),
        (Msg::Node, 1 | 2 | 3 | 6) => Skip,
        (Msg::Node, 4) => Capture(Slot::NodeOp),
        (Msg::Node, 5) => Sub(Msg::Attribute),
        (Msg::Node, 7) => Capture(Slot::NodeDomain),
        (Msg::Node, 8) => Refuse("node overload"),
        (Msg::Node, 9) => Sub(Msg::Entry),
        (Msg::Node, 10) => Refuse("device_configurations"),
        (Msg::Attribute, 1 | 4 | 9 | 13) => Skip,
        (Msg::Attribute, 2) => Fixed32,
        (Msg::Attribute, 3) => Varint,
        (Msg::Attribute, 5 | 10) => Sub(Msg::Tensor),
        (Msg::Attribute, 6 | 11) => Refuse("subgraph attribute"),
        (Msg::Attribute, 7) => RepeatedFixed(Slot::None, 4),
        (Msg::Attribute, 8) => RepeatedVarint(Slot::None),
        (Msg::Attribute, 14 | 15) => Refuse("type_proto attribute"),
        (Msg::Attribute, 20) => Scalar(Slot::AttrType),
        (Msg::Attribute, 21) => Refuse("ref_attr_name"),
        (Msg::Attribute, 22 | 23) => Refuse("sparse tensor attribute"),
        (Msg::Tensor, 1) => RepeatedVarint(Slot::TensorDims),
        (Msg::Tensor, 2) => Scalar(Slot::TensorDataType),
        (Msg::Tensor, 3) => Refuse("tensor segment"),
        (Msg::Tensor, 4) => RepeatedFixed(Slot::TensorTyped(4), 4),
        (Msg::Tensor, 5) => RepeatedVarint(Slot::TensorTyped(5)),
        (Msg::Tensor, 6) => Refuse("string tensor"),
        (Msg::Tensor, 7) => RepeatedVarint(Slot::TensorTyped(7)),
        (Msg::Tensor, 8 | 12) => Skip,
        (Msg::Tensor, 9) => Raw,
        (Msg::Tensor, 10) => RepeatedFixed(Slot::TensorTyped(10), 8),
        (Msg::Tensor, 11) => RepeatedVarint(Slot::TensorTyped(11)),
        (Msg::Tensor, 13) => Refuse("external_data"),
        (Msg::Tensor, 14) => Scalar(Slot::TensorLocation),
        (Msg::Tensor, 16) => Sub(Msg::Entry),
        (Msg::ValueInfo, 1 | 3) => Skip,
        (Msg::ValueInfo, 2) => Sub(Msg::Type),
        (Msg::ValueInfo, 4) => Sub(Msg::Entry),
        (Msg::Type, 1) => Sub(Msg::TypeTensor),
        (Msg::Type, 6) => Skip,
        (Msg::Type, 4 | 5 | 8 | 9) => Refuse("non-tensor value type"),
        (Msg::TypeTensor, 1) => Scalar(Slot::ElemType),
        (Msg::TypeTensor, 2) => Sub(Msg::Shape),
        (Msg::Shape, 1) => Sub(Msg::Dim),
        (Msg::Dim, 1) => Varint,
        (Msg::Dim, 2 | 3) => Skip,
        (Msg::Entry, 1 | 2) => Skip,
        _ => return None,
    })
}

/// Whether `wire` is a valid wire type for the field. Repeated numbers accept
/// both the packed (2) and the unpacked form.
fn wire_ok(f: Field, wire: u8) -> bool {
    match f {
        Field::Sub(_) | Field::Skip | Field::Capture(_) | Field::Raw => wire == 2,
        Field::Scalar(_) | Field::Varint => wire == 0,
        Field::RepeatedVarint(_) => wire == 0 || wire == 2,
        Field::Fixed32 => wire == 5,
        Field::RepeatedFixed(_, 4) => wire == 5 || wire == 2,
        Field::RepeatedFixed(_, _) => wire == 1 || wire == 2,
        Field::Refuse(_) => true,
    }
}

/// A short string kept for comparison: at most `cap` bytes, plus a flag for
/// anything longer (which can match no allow-listed value).
#[derive(Clone, Debug, Default)]
struct Captured {
    bytes: Vec<u8>,
    overflow: bool,
}

#[derive(Debug)]
struct TensorAcc {
    data_type: Option<i32>,
    /// Product of the dimensions seen so far (1 for a scalar). Kept as a
    /// running product, so a long `dims` list holds no memory.
    numel: u64,
    location: i64,
    /// The data field used, by field number (4, 5, 7, 9, 10, 11).
    data_field: Option<u64>,
    /// Elements in typed data fields, or bytes of `raw_data`.
    data_count: u64,
    /// Parameters counted for this tensor at its first payload field.
    counted: Option<u64>,
    /// A `GraphProto.initializer`: its float payload is left out of the
    /// structure stream. Constant attribute tensors are in it.
    initializer: bool,
}

impl TensorAcc {
    fn new(initializer: bool) -> Self {
        Self {
            data_type: None,
            numel: 1,
            location: 0,
            data_field: None,
            data_count: 0,
            counted: None,
            initializer,
        }
    }
}

#[derive(Debug, Default)]
struct FrameAcc {
    domain: Captured,
    op: Captured,
    has_domain: bool,
    version: Option<u64>,
    attr_type: Option<i64>,
    elem_type: Option<i64>,
    tensor: Option<TensorAcc>,
}

#[derive(Debug)]
struct Frame {
    msg: Msg,
    end: u64,
    acc: FrameAcc,
}

#[derive(Debug)]
enum Sink {
    /// Structure bytes, otherwise dropped.
    Skip,
    /// Structure bytes, of which up to `cap` are kept in the slot.
    Capture(Slot, usize),
    /// Packed varints: structure bytes, each value decoded into the slot.
    PackedVarint(Slot, u64, u32),
    /// Packed fixed-width values or `raw_data`: `float` checks that every
    /// value is finite, `elide` leaves the bytes out of the structure stream.
    Data { float: bool, elide: bool },
}

#[derive(Debug)]
enum State {
    Tag(u64, u32),
    Varint(Field, u64, u32),
    Fixed(Field, u8, u8),
    Len(Field, u64, u32),
    Bytes(Sink, u64),
}

/// Where the structure stream goes. It is fed every byte of the file except
/// the initializer float payloads, in file order, and nothing else: it is
/// compared with a reference stream, kept for a tool that writes one, or only
/// counted.
#[derive(Debug, Default)]
struct Structure<'p> {
    /// Bytes of the stream so far.
    len: u64,
    /// The reference stream it is compared with.
    reference: Option<&'p [u8]>,
    /// The stream offset of the first byte that differs from the reference
    /// (or where one of the two ends first), once found.
    difference: Option<u64>,
    /// The stream itself.
    kept: Option<Vec<u8>>,
}

impl Structure<'_> {
    fn feed(&mut self, part: &[u8]) {
        let at = self.len;
        self.len += part.len() as u64;
        if let (Some(reference), None) = (self.reference, self.difference) {
            // No difference so far, so `at` is within the reference.
            let rest = &reference[at as usize..];
            let n = part.len().min(rest.len());
            if part[..n] != rest[..n] {
                let i = part.iter().zip(rest).position(|(a, b)| a != b).unwrap_or(n);
                self.difference = Some(at + i as u64);
            } else if n < part.len() {
                self.difference = Some(at + n as u64);
            }
        }
        if let Some(kept) = &mut self.kept {
            kept.extend_from_slice(part);
        }
    }

    /// Whether the whole stream equals the reference, if there is one.
    fn matches(&mut self) -> Option<bool> {
        let reference = self.reference?;
        if self.difference.is_none() && self.len != reference.len() as u64 {
            self.difference = Some(self.len);
        }
        Some(self.difference.is_none())
    }
}

/// The streaming walk over one graph file of known length.
pub struct GraphScan<'p> {
    policy: GraphPolicy<'p>,
    len: u64,
    pos: u64,
    stack: Vec<Frame>,
    state: State,
    structure: Structure<'p>,
    capture_cap: usize,
    /// Bytes of a float value split across chunks.
    carry: [u8; 4],
    carry_len: usize,
    report: GraphReport,
    failed: Option<Refusal>,
}

fn malformed(what: &str) -> Refusal {
    Refusal::GraphMalformed(what.into())
}

impl<'p> GraphScan<'p> {
    /// Starts a walk over a graph file of `len` bytes. The report counts the
    /// structure stream and compares it with nothing.
    pub fn new(len: u64, policy: GraphPolicy<'p>) -> Self {
        Self::with_structure(len, policy, Structure::default())
    }

    /// Starts a walk that compares the structure stream byte for byte with
    /// `reference` as it goes ([`GraphReport::structure_match`]). A
    /// difference is not a refusal of the walk: the caller decides what it
    /// means once the walk has passed.
    pub fn against(len: u64, policy: GraphPolicy<'p>, reference: &'p [u8]) -> Self {
        Self::with_structure(len, policy, Structure { reference: Some(reference), ..Structure::default() })
    }

    /// Starts a walk that keeps the structure stream, for a tool that writes
    /// a class's reference stream ([`GraphScan::finish_keeping`]). It holds
    /// the whole stream in memory: the file less its initializer float
    /// payloads.
    pub fn keeping(len: u64, policy: GraphPolicy<'p>) -> Self {
        Self::with_structure(len, policy, Structure { kept: Some(Vec::new()), ..Structure::default() })
    }

    fn with_structure(len: u64, policy: GraphPolicy<'p>, structure: Structure<'p>) -> Self {
        let capture_cap = policy
            .ops
            .iter()
            .chain(policy.domains.iter())
            .map(|s| s.len())
            .max()
            .unwrap_or(0);
        Self {
            policy,
            len,
            pos: 0,
            stack: vec![Frame {
                msg: Msg::Model,
                end: len,
                acc: FrameAcc::default(),
            }],
            state: State::Tag(0, 0),
            structure,
            capture_cap,
            carry: [0; 4],
            carry_len: 0,
            report: GraphReport {
                structure_bytes: 0,
                structure_match: None,
                structure_difference: None,
                parameters: 0,
                ir_version: 0,
                opsets: Vec::new(),
                nodes: 0,
                initializers: 0,
                inputs: 0,
                outputs: 0,
                ops: BTreeMap::new(),
                elements_by_dtype: BTreeMap::new(),
            },
            failed: None,
        }
    }

    /// Consumes the next chunk. After a refusal every later call returns it.
    pub fn update(&mut self, chunk: &[u8]) -> Result<(), Refusal> {
        if let Some(r) = &self.failed {
            return Err(r.clone());
        }
        let result = self.consume(chunk);
        if let Err(r) = &result {
            self.failed = Some(r.clone());
        }
        result
    }

    /// Ends the walk: the whole file must have been consumed.
    pub fn finish(self) -> Result<GraphReport, Refusal> {
        self.finish_keeping().map(|(report, _)| report)
    }

    /// [`GraphScan::finish`], also returning the structure stream a
    /// [`GraphScan::keeping`] walk kept (empty for any other walk).
    pub fn finish_keeping(mut self) -> Result<(GraphReport, Vec<u8>), Refusal> {
        if let Some(r) = self.failed.take() {
            return Err(r);
        }
        if self.pos != self.len || !matches!(self.state, State::Tag(0, 0)) {
            return Err(malformed("graph file ends inside a field"));
        }
        // An empty file still has the model frame open.
        self.close_frames()?;
        if !self.stack.is_empty() {
            return Err(malformed("graph file ends inside a message"));
        }
        if self.report.ir_version == 0 {
            return Err(malformed("model without ir_version"));
        }
        if !self
            .report
            .opsets
            .iter()
            .any(|(d, _)| d.is_empty() || d == "ai.onnx")
        {
            return Err(malformed("model without a default-domain opset"));
        }
        self.report.structure_bytes = self.structure.len;
        self.report.structure_match = self.structure.matches();
        self.report.structure_difference = self.structure.difference;
        Ok((self.report, self.structure.kept.take().unwrap_or_default()))
    }

    fn top(&mut self) -> &mut Frame {
        self.stack
            .last_mut()
            .expect("a frame is open while bytes remain")
    }

    fn consume(&mut self, chunk: &[u8]) -> Result<(), Refusal> {
        let remaining_file = self.len - self.pos;
        if chunk.len() as u64 > remaining_file {
            return Err(malformed("more bytes than the declared graph length"));
        }
        let mut i = 0usize;
        while i < chunk.len() {
            if self.stack.is_empty() {
                return Err(malformed("bytes after the model"));
            }
            match &mut self.state {
                State::Bytes(_, remaining) => {
                    let take = (*remaining).min((chunk.len() - i) as u64) as usize;
                    let part = &chunk[i..i + take];
                    self.payload(part)?;
                    i += take;
                    self.pos += take as u64;
                    let State::Bytes(_, remaining) = &mut self.state else {
                        unreachable!()
                    };
                    *remaining -= take as u64;
                    if *remaining == 0 {
                        self.end_bytes()?;
                    }
                }
                _ => {
                    let b = chunk[i];
                    self.structure.feed(&[b]);
                    i += 1;
                    self.pos += 1;
                    self.byte(b)?;
                }
            }
        }
        Ok(())
    }

    /// Accumulates one varint byte; returns the value when complete.
    fn varint(v: &mut u64, shift: &mut u32, b: u8) -> Result<Option<u64>, Refusal> {
        if *shift >= 64 || (*shift == 63 && b > 1) {
            return Err(malformed("varint overflow"));
        }
        *v |= u64::from(b & 0x7f) << *shift;
        *shift += 7;
        Ok(if b & 0x80 == 0 { Some(*v) } else { None })
    }

    fn byte(&mut self, b: u8) -> Result<(), Refusal> {
        let state = std::mem::replace(&mut self.state, State::Tag(0, 0));
        match state {
            State::Tag(mut v, mut shift) => match Self::varint(&mut v, &mut shift, b)? {
                None => self.state = State::Tag(v, shift),
                Some(tag) => self.start_field(tag)?,
            },
            State::Varint(f, mut v, mut shift) => match Self::varint(&mut v, &mut shift, b)? {
                None => self.state = State::Varint(f, v, shift),
                Some(value) => {
                    self.scalar(f, value)?;
                    self.field_done()?;
                }
            },
            State::Fixed(f, need, got) => {
                if got + 1 < need {
                    self.state = State::Fixed(f, need, got + 1);
                } else {
                    if let Field::RepeatedFixed(slot, _) = f {
                        self.count_typed(slot, 1)?;
                    }
                    self.field_done()?;
                }
            }
            State::Len(f, mut v, mut shift) => match Self::varint(&mut v, &mut shift, b)? {
                None => self.state = State::Len(f, v, shift),
                Some(len) => self.start_len(f, len)?,
            },
            State::Bytes(..) => unreachable!("payload bytes are handled in bulk"),
        }
        Ok(())
    }

    fn start_field(&mut self, tag: u64) -> Result<(), Refusal> {
        let wire = (tag & 7) as u8;
        let number = tag >> 3;
        let msg = self.top().msg;
        if number == 0 {
            return Err(malformed("field number 0"));
        }
        let Some(f) = field(msg, number) else {
            return Err(Refusal::GraphUnknownField {
                message: msg.name(),
                field: number,
            });
        };
        if let Field::Refuse(what) = f {
            return Err(match what {
                "functions" => Refusal::GraphCustomDomain("model-local functions".into()),
                "external_data" => Refusal::GraphExternalData,
                other => Refusal::GraphUnsupported(other.into()),
            });
        }
        if !wire_ok(f, wire) {
            return Err(malformed("wire type differs from the ONNX schema"));
        }
        if msg == Msg::Tensor && number == 4 && wire == 5 {
            // float_data is declared packed; only the packed form is accepted.
            return Err(malformed("unpacked float_data"));
        }
        if msg == Msg::Graph {
            match number {
                11 => self.report.inputs += 1,
                12 => self.report.outputs += 1,
                _ => {}
            }
        }
        self.state = match wire {
            0 => State::Varint(f, 0, 0),
            1 => State::Fixed(f, 8, 0),
            5 => State::Fixed(f, 4, 0),
            2 => State::Len(f, 0, 0),
            _ => return Err(malformed("group wire type")),
        };
        Ok(())
    }

    fn scalar(&mut self, f: Field, value: u64) -> Result<(), Refusal> {
        let slot = match f {
            Field::Scalar(s) | Field::RepeatedVarint(s) => s,
            _ => return Ok(()),
        };
        self.slot_value(slot, value)
    }

    fn slot_value(&mut self, slot: Slot, value: u64) -> Result<(), Refusal> {
        match slot {
            Slot::IrVersion => {
                self.report.ir_version = value;
                return Ok(());
            }
            Slot::TensorTyped(_) => return self.count_typed(slot, 1),
            _ => {}
        }
        let acc = &mut self.stack.last_mut().expect("frame").acc;
        match slot {
            Slot::OpsetVersion => acc.version = Some(value),
            Slot::AttrType => acc.attr_type = Some(value as i64),
            Slot::ElemType => acc.elem_type = Some(value as i64),
            Slot::TensorDims => {
                let t = acc.tensor.as_mut().expect("tensor frame");
                // dims are int64; a negative dimension is invalid in a stored tensor.
                if (value as i64) < 0 {
                    return Err(malformed("negative tensor dimension"));
                }
                t.numel = t
                    .numel
                    .checked_mul(value)
                    .ok_or_else(|| malformed("tensor element count overflow"))?;
            }
            Slot::TensorDataType => {
                let t = acc.tensor.as_mut().expect("tensor frame");
                // A second data_type would relabel a payload after the walk
                // decided whether it belongs to the structure stream.
                if t.data_type.is_some() {
                    return Err(malformed("repeated tensor data_type"));
                }
                t.data_type = Some(value as i64 as i32);
            }
            Slot::TensorLocation => {
                let t = acc.tensor.as_mut().expect("tensor frame");
                t.location = value as i64;
                if t.location != 0 {
                    return Err(Refusal::GraphExternalData);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Records `n` elements of a typed data field.
    fn count_typed(&mut self, slot: Slot, n: u64) -> Result<(), Refusal> {
        let Slot::TensorTyped(number) = slot else {
            return Ok(());
        };
        let t = self
            .stack
            .last_mut()
            .expect("frame")
            .acc
            .tensor
            .as_mut()
            .expect("tensor frame");
        let dt = t
            .data_type
            .ok_or_else(|| malformed("tensor data before data_type"))?;
        let ok = match number {
            4 => dt == dtype::FLOAT,
            5 => matches!(dt, dtype::INT32 | dtype::INT8 | dtype::UINT8 | dtype::BOOL),
            7 => dt == dtype::INT64,
            10 => dt == dtype::DOUBLE,
            _ => false,
        };
        if !ok {
            return Err(malformed(
                "typed tensor data field does not match data_type",
            ));
        }
        match t.data_field {
            None => t.data_field = Some(u64::from(number as u8)),
            Some(f) if f == u64::from(number as u8) => {}
            Some(_) => return Err(malformed("tensor with two data fields")),
        }
        t.data_count = t
            .data_count
            .checked_add(n)
            .ok_or_else(|| malformed("tensor data count"))?;
        Ok(())
    }

    fn start_len(&mut self, f: Field, len: u64) -> Result<(), Refusal> {
        let end = self.top().end;
        // A length prefix can itself run past its message's end.
        if self.pos > end || len > end - self.pos {
            return Err(malformed("length past the end of its message"));
        }
        match f {
            Field::Sub(msg) => {
                let mut acc = FrameAcc::default();
                let parent = self.top().msg;
                if msg == Msg::Tensor {
                    acc.tensor = Some(TensorAcc::new(parent == Msg::Graph));
                }
                match (parent, msg) {
                    (Msg::Graph, Msg::Node) => self.report.nodes += 1,
                    (Msg::Graph, Msg::Tensor) => self.report.initializers += 1,
                    _ => {}
                }
                self.stack.push(Frame {
                    msg,
                    end: self.pos + len,
                    acc,
                });
                self.state = State::Tag(0, 0);
                // An empty message ends at once.
                self.close_frames()?;
                Ok(())
            }
            Field::Skip => self.begin_bytes(Sink::Skip, len),
            Field::Capture(slot) => {
                let acc = &mut self.top().acc;
                match slot {
                    Slot::OpsetDomain | Slot::NodeDomain => acc.has_domain = true,
                    _ => {}
                }
                self.begin_bytes(Sink::Capture(slot, self.capture_cap), len)
            }
            Field::RepeatedVarint(slot) => self.begin_bytes(Sink::PackedVarint(slot, 0, 0), len),
            Field::RepeatedFixed(slot, width) => {
                if !len.is_multiple_of(u64::from(width)) {
                    return Err(malformed("packed fixed-width field length"));
                }
                let float = slot == Slot::TensorTyped(4);
                let mut elide = false;
                if let Slot::TensorTyped(_) = slot {
                    self.count_typed(slot, len / u64::from(width))?;
                    if float {
                        elide = self.count_parameters_at_payload()?;
                    }
                }
                self.begin_bytes(Sink::Data { float, elide }, len)
            }
            Field::Raw => {
                let t = self
                    .stack
                    .last_mut()
                    .expect("frame")
                    .acc
                    .tensor
                    .as_mut()
                    .expect("tensor frame");
                let dt = t
                    .data_type
                    .ok_or_else(|| malformed("tensor raw_data before data_type"))?;
                if t.data_field.is_some() {
                    return Err(malformed("tensor with two data fields"));
                }
                t.data_field = Some(9);
                t.data_count = len;
                let float = dt == dtype::FLOAT;
                let elide = float && self.count_parameters_at_payload()?;
                self.begin_bytes(Sink::Data { float, elide }, len)
            }
            Field::Scalar(_) | Field::Varint | Field::Fixed32 | Field::Refuse(_) => {
                unreachable!("wire type checked")
            }
        }
    }

    /// Counts a `FLOAT` tensor's elements when its first payload field
    /// starts, so the bound is enforced before the payload is read. A tensor
    /// whose packed `float_data` is split over several fields is counted
    /// once. Returns whether the payload is left out of the structure stream
    /// (initializers only).
    fn count_parameters_at_payload(&mut self) -> Result<bool, Refusal> {
        let max = self.policy.max_parameters;
        let t = self
            .stack
            .last_mut()
            .expect("frame")
            .acc
            .tensor
            .as_mut()
            .expect("tensor frame");
        let elide = t.initializer;
        if t.counted.is_some() {
            return Ok(elide);
        }
        let numel = t.numel;
        t.counted = Some(numel);
        let total = self
            .report
            .parameters
            .checked_add(numel)
            .ok_or_else(|| malformed("parameter count overflow"))?;
        if total > max {
            return Err(Refusal::TooManyParameters { count: total, max });
        }
        self.report.parameters = total;
        Ok(elide)
    }

    fn begin_bytes(&mut self, sink: Sink, len: u64) -> Result<(), Refusal> {
        if len == 0 {
            self.state = State::Bytes(sink, 0);
            return self.end_bytes();
        }
        self.state = State::Bytes(sink, len);
        Ok(())
    }

    fn payload(&mut self, part: &[u8]) -> Result<(), Refusal> {
        let State::Bytes(sink, _) = &mut self.state else {
            unreachable!()
        };
        match sink {
            Sink::Skip => self.structure.feed(part),
            Sink::Data { float, elide } => {
                let (float, elide) = (*float, *elide);
                if float {
                    // Initializer payloads are the elided ones.
                    let subnormal = elide && self.policy.refuse_subnormal;
                    self.check_finite(part, subnormal)?;
                }
                // An elided payload never reaches the structure stream.
                if !elide {
                    self.structure.feed(part);
                }
            }
            Sink::Capture(slot, cap) => {
                self.structure.feed(part);
                let (slot, cap) = (*slot, *cap);
                let acc = &mut self.stack.last_mut().expect("frame").acc;
                let c = match slot {
                    Slot::OpsetDomain | Slot::NodeDomain => &mut acc.domain,
                    _ => &mut acc.op,
                };
                let room = cap.saturating_sub(c.bytes.len());
                c.bytes.extend_from_slice(&part[..part.len().min(room)]);
                if part.len() > room {
                    c.overflow = true;
                }
            }
            Sink::PackedVarint(slot, v, shift) => {
                self.structure.feed(part);
                let slot = *slot;
                let (mut v0, mut s0) = (*v, *shift);
                for &b in part {
                    if let Some(value) = Self::varint(&mut v0, &mut s0, b)? {
                        self.slot_value(slot, value)?;
                        v0 = 0;
                        s0 = 0;
                    }
                }
                let State::Bytes(Sink::PackedVarint(_, v, shift), _) = &mut self.state else {
                    unreachable!()
                };
                *v = v0;
                *shift = s0;
            }
        }
        Ok(())
    }

    /// Refuses a non-finite `FLOAT` value, and with `subnormal` a float32
    /// subnormal one ([`INITIALIZER_RULE`]).
    fn check_finite(&mut self, mut part: &[u8], subnormal: bool) -> Result<(), Refusal> {
        let check = |w: [u8; 4]| -> Result<(), Refusal> {
            let v = f32::from_le_bytes(w);
            if !v.is_finite() {
                return Err(malformed("non-finite float value"));
            }
            if subnormal && v.is_subnormal() {
                return Err(Refusal::GraphSubnormal);
            }
            Ok(())
        };
        if self.carry_len > 0 {
            let take = (4 - self.carry_len).min(part.len());
            self.carry[self.carry_len..self.carry_len + take].copy_from_slice(&part[..take]);
            self.carry_len += take;
            part = &part[take..];
            if self.carry_len < 4 {
                return Ok(());
            }
            check(self.carry)?;
            self.carry_len = 0;
        }
        let (words, tail) = part.as_chunks::<4>();
        for w in words {
            check(*w)?;
        }
        self.carry[..tail.len()].copy_from_slice(tail);
        self.carry_len = tail.len();
        Ok(())
    }

    fn end_bytes(&mut self) -> Result<(), Refusal> {
        if self.carry_len != 0 {
            return Err(malformed("float payload is not whole values"));
        }
        let state = std::mem::replace(&mut self.state, State::Tag(0, 0));
        if let State::Bytes(Sink::PackedVarint(_, _, shift), _) = state
            && shift != 0
        {
            return Err(malformed("packed varint ends mid-value"));
        }
        self.field_done()
    }

    fn field_done(&mut self) -> Result<(), Refusal> {
        self.state = State::Tag(0, 0);
        self.close_frames()
    }

    fn close_frames(&mut self) -> Result<(), Refusal> {
        while let Some(top) = self.stack.last() {
            if self.pos < top.end {
                break;
            }
            if self.pos > top.end {
                return Err(malformed("field crosses its message end"));
            }
            let frame = self.stack.pop().expect("checked");
            self.finalize(frame)?;
        }
        Ok(())
    }

    fn allowed_domain(&self, c: &Captured) -> Option<String> {
        if c.overflow {
            return None;
        }
        let s = std::str::from_utf8(&c.bytes).ok()?;
        self.policy.domains.contains(&s).then(|| s.to_string())
    }

    fn finalize(&mut self, frame: Frame) -> Result<(), Refusal> {
        let acc = frame.acc;
        match frame.msg {
            Msg::Opset => {
                let domain = if acc.has_domain {
                    self.allowed_domain(&acc.domain)
                        .ok_or_else(|| Refusal::GraphCustomDomain(lossy(&acc.domain)))?
                } else {
                    String::new()
                };
                let version = acc
                    .version
                    .ok_or_else(|| malformed("opset without version"))?;
                // One import per domain, so the list is bounded by the allowed domains.
                if self.report.opsets.iter().any(|(d, _)| *d == domain) {
                    return Err(malformed("duplicate opset domain"));
                }
                self.report.opsets.push((domain, version));
            }
            Msg::Node => {
                if acc.has_domain && self.allowed_domain(&acc.domain).is_none() {
                    return Err(Refusal::GraphCustomDomain(lossy(&acc.domain)));
                }
                let op = std::str::from_utf8(&acc.op.bytes)
                    .ok()
                    .filter(|op| !acc.op.overflow && self.policy.ops.contains(op))
                    .ok_or_else(|| Refusal::GraphOpNotAllowed(lossy(&acc.op)))?;
                *self.report.ops.entry(op.to_string()).or_default() += 1;
            }
            Msg::Attribute => {
                let ty = acc
                    .attr_type
                    .ok_or_else(|| malformed("attribute without type"))?;
                if !self.policy.attr_types.contains(&ty) {
                    return Err(Refusal::GraphUnsupported(format!("attribute type {ty}")));
                }
            }
            Msg::Tensor => {
                let t = acc.tensor.expect("tensor frame");
                let dt = t
                    .data_type
                    .ok_or_else(|| malformed("tensor without data_type"))?;
                if !self.policy.dtypes.contains(&dt) {
                    return Err(Refusal::GraphDtypeNotAllowed(dt));
                }
                let n = t.numel;
                let stored = match t.data_field {
                    None => 0,
                    Some(9) => {
                        let size = dtype::size(dt).ok_or(Refusal::GraphDtypeNotAllowed(dt))?;
                        if !t.data_count.is_multiple_of(size) {
                            return Err(malformed("raw_data length is not whole elements"));
                        }
                        t.data_count / size
                    }
                    Some(_) => t.data_count,
                };
                if stored != n {
                    return Err(malformed("tensor payload length differs from its shape"));
                }
                if dt == dtype::FLOAT && n != 0 && t.counted != Some(n) {
                    // A shape field after the payload.
                    return Err(malformed("tensor shape after its payload"));
                }
                *self.report.elements_by_dtype.entry(dt).or_default() += n;
            }
            Msg::TypeTensor => {
                let et = acc
                    .elem_type
                    .ok_or_else(|| malformed("tensor type without elem_type"))?;
                if !self.policy.dtypes.contains(&(et as i32)) {
                    return Err(Refusal::GraphDtypeNotAllowed(et as i32));
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn lossy(c: &Captured) -> String {
    let mut s = String::from_utf8_lossy(&c.bytes).into_owned();
    if c.overflow {
        s.push_str("...");
    }
    s
}

/// Walks a whole graph held in memory.
pub fn scan(bytes: &[u8], policy: GraphPolicy<'_>) -> Result<GraphReport, Refusal> {
    let mut s = GraphScan::new(bytes.len() as u64, policy);
    s.update(bytes)?;
    s.finish()
}
