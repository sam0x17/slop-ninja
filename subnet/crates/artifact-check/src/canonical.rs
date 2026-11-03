//! Canonical JSON layouts of the role files.
//!
//! Each JSON role file must be byte-identical to the canonical serialization
//! of its own parsed value under the layout its writer uses. No insignificant
//! whitespace, number spelling, escape or key order is free, so a file's
//! length is a function of its content, and padding (trailing spaces, long
//! number literals, `\u0061` for `a`, duplicate keys) is refused.
//!
//! - [`Layout::PythonCompact`] (`heads.json`, `manifest.json`): the release
//!   exporter's `json.dumps(v, sort_keys=True, separators=(",", ":"),
//!   ensure_ascii=True, allow_nan=False)` plus a final newline
//!   (`grammar-eval/python/v6_embeddings.py` `canonical`). Floats are Python
//!   `repr`.
//! - [`Layout::SerdePretty`] (`contract.json`, `calibration.json`):
//!   `serde_json::to_vec_pretty` of a `Value` (keys sorted, two-space indent,
//!   raw UTF-8) plus a final newline, as `grammar-a-baseline`'s contract and
//!   calibration tools write them. Floats are spelled as `serde_json` 1.0.146
//!   and later write them (`zmij`: `1e+16`, `1e-7`); older `serde_json`
//!   (`ryu`: `1e16`) differs for exponents of 16 and more.
//!
//! The float spellings are defined here from the shortest round-trip digits
//! ([`float_str`]), not taken from a library, so a dependency upgrade cannot
//! change the layout; tests compare them with CPython and `serde_json`.
//!
//! [`check`] validates a file in one pass over its bytes, without building a
//! value: it holds only the previous key of each open object, for the key
//! order check. Nesting is bounded by `serde_json`'s recursion limit, which
//! the scorer's parser applies to the same files. Integers must fit `i64` or
//! `u64` (the range in which `serde_json` keeps them integers).
//!
//! The same pass prices the file ([`check_cost`], model [`VALUE_COST_ID`]): an
//! upper bound on the heap a `serde_json::Value` of the whole file can hold,
//! the costliest form any reader of the file builds (typed schemas hold no
//! more for the same bytes). A byte budget alone does not bound memory: a
//! small object costs a map node of hundreds of bytes for a few text bytes.

use serde_json::Value;

/// The layouts. [`Layout::id`] names each one in the spec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    PythonCompact,
    SerdePretty,
}

impl Layout {
    pub fn id(self) -> &'static str {
        match self {
            Layout::PythonCompact => "python_json_dumps_sorted_compact_ascii_repr_newline_v1",
            Layout::SerdePretty => "serde_json_pretty_sorted_utf8_zmij_floats_newline_v1",
        }
    }
}

/// Deepest nesting accepted: `serde_json`'s recursion limit.
pub const MAX_DEPTH: usize = 128;

/// The value cost model [`check_cost`] applies.
pub const VALUE_COST_ID: &str = "slop_ninja_json_value_cost_v1";

/// Per-node prices of [`VALUE_COST_ID`], in bytes, for a 64-bit
/// `serde_json` 1.x `Value` in either build: the default (a 32-byte `Value`,
/// `BTreeMap` objects, inline numbers) and the one a build that links rtao
/// gets by feature unification (`preserve_order`: a 72-byte `Value` and
/// `IndexMap` objects; `arbitrary_precision`: numbers kept as strings). Each
/// price is the larger of the two. Each heap block is priced with glibc's
/// chunk overhead (at most 24 bytes, at least 32 bytes a chunk).
pub mod value_cost {
    /// The root `Value`.
    pub const ROOT: u64 = 80;
    /// A non-empty array, per slot of its final capacity: a `Value` of at
    /// most 80 bytes, doubled for the buffer a growing vector moves out of.
    pub const ARRAY_SLOT: u64 = 160;
    /// A non-empty array's block overhead.
    pub const ARRAY: u64 = 16;
    /// Smallest capacity of a non-empty vector of `Value`s.
    pub const ARRAY_MIN_CAPACITY: u64 = 4;
    /// A non-empty object's first entry: one B-tree node (eleven 24-byte keys
    /// and 32-byte values, twelve edges, links and lengths: 728 bytes, a
    /// 736-byte chunk); an index map's first entries take about 430.
    pub const OBJECT: u64 = 744;
    /// Each further entry: an index map's 104-byte bucket three times over
    /// while its entries vector grows (the new vector at twice the length and
    /// the old one), plus its hash table share; a B-tree needs 148.8 (a node
    /// per five entries, since nodes other than the root hold at least five).
    pub const OBJECT_ENTRY: u64 = 352;
    /// A non-empty string, key or (under `arbitrary_precision`) number,
    /// besides its text bytes: the chunk of a heap allocation of that length.
    pub const TEXT: u64 = 32;
    /// The parser's scratch buffer, which holds a string with escapes while it
    /// is decoded: this many times the longest string text, once per file
    /// (a growing vector's capacity).
    pub const SCRATCH_PER_LONGEST_TEXT: u64 = 2;
}

/// Checks that `bytes` are exactly the canonical serialization of the JSON
/// value they hold, under `layout`.
pub fn check(bytes: &[u8], layout: Layout) -> Result<(), String> {
    check_cost(bytes, layout).map(|_| ())
}

/// [`check`], returning the file's value cost ([`VALUE_COST_ID`]): an upper
/// bound on the heap bytes a `serde_json::Value` of the file holds. It is a
/// function of the bytes only, the same in every build.
pub fn check_cost(bytes: &[u8], layout: Layout) -> Result<u64, String> {
    std::str::from_utf8(bytes).map_err(|e| format!("not UTF-8 at byte {}", e.valid_up_to()))?;
    let mut c = Cursor {
        b: bytes,
        i: 0,
        layout,
        cost: value_cost::ROOT,
        longest: 0,
    };
    c.value(0)?;
    c.expect(b"\n")?;
    if c.i != bytes.len() {
        return c.err("bytes after the final newline");
    }
    let scratch = c.longest.saturating_mul(value_cost::SCRATCH_PER_LONGEST_TEXT);
    Ok(c.cost.saturating_add(scratch))
}

/// The value cost of a non-empty text of `len` bytes (a key, a string or a
/// number's literal).
fn text_cost(len: usize) -> u64 {
    if len == 0 {
        0
    } else {
        value_cost::TEXT + len as u64
    }
}

struct Cursor<'a> {
    b: &'a [u8],
    i: usize,
    layout: Layout,
    /// The value cost so far (saturating; a file is at most a few GiB, so it
    /// never saturates in practice).
    cost: u64,
    /// The longest string text (key or value) so far, in file bytes.
    longest: u64,
}

impl Cursor<'_> {
    fn err<T>(&self, what: &str) -> Result<T, String> {
        Err(format!("{what} at byte {}", self.i))
    }

    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn expect(&mut self, s: &[u8]) -> Result<(), String> {
        if self.b[self.i..].starts_with(s) {
            self.i += s.len();
            Ok(())
        } else {
            self.err("not the canonical layout")
        }
    }

    /// The separator before an item or a closing bracket at `depth`.
    fn indent(&mut self, depth: usize) -> Result<(), String> {
        if self.layout == Layout::SerdePretty {
            self.expect(b"\n")?;
            for _ in 0..depth {
                self.expect(b"  ")?;
            }
        }
        Ok(())
    }

    fn value(&mut self, depth: usize) -> Result<(), String> {
        if depth >= MAX_DEPTH {
            return self.err("nesting deeper than the recursion limit");
        }
        match self.peek() {
            Some(b'{') => self.object(depth),
            Some(b'[') => self.array(depth),
            Some(b'"') => {
                let start = self.i;
                self.string(None)?;
                // The text between the quotes bounds the decoded length.
                self.text(self.i - start - 2);
                Ok(())
            }
            Some(b't') => self.expect(b"true"),
            Some(b'f') => self.expect(b"false"),
            Some(b'n') => self.expect(b"null"),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.i;
                self.number()?;
                self.add(text_cost(self.i - start));
                Ok(())
            }
            _ => self.err("not a JSON value"),
        }
    }

    fn add(&mut self, bytes: u64) {
        self.cost = self.cost.saturating_add(bytes);
    }

    /// A string or key whose text is `len` file bytes.
    fn text(&mut self, len: usize) {
        self.add(text_cost(len));
        self.longest = self.longest.max(len as u64);
    }

    fn object(&mut self, depth: usize) -> Result<(), String> {
        self.expect(b"{")?;
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(());
        }
        let mut prev: Vec<u8> = Vec::new();
        let mut key: Vec<u8> = Vec::new();
        let mut first = true;
        self.add(value_cost::OBJECT);
        loop {
            self.indent(depth + 1)?;
            key.clear();
            let start = self.i;
            self.string(Some(&mut key))?;
            if !first && key <= prev {
                return self.err("keys not strictly sorted");
            }
            if !first {
                self.add(value_cost::OBJECT_ENTRY);
            }
            self.text(self.i - start - 2);
            first = false;
            std::mem::swap(&mut prev, &mut key);
            self.expect(match self.layout {
                Layout::PythonCompact => b":",
                Layout::SerdePretty => b": ",
            })?;
            self.value(depth + 1)?;
            if self.peek() == Some(b',') {
                self.i += 1;
            } else {
                break;
            }
        }
        self.indent(depth)?;
        self.expect(b"}")
    }

    fn array(&mut self, depth: usize) -> Result<(), String> {
        self.expect(b"[")?;
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(());
        }
        let mut n = 0u64;
        loop {
            self.indent(depth + 1)?;
            self.value(depth + 1)?;
            n += 1;
            if self.peek() == Some(b',') {
                self.i += 1;
            } else {
                break;
            }
        }
        let capacity = n.next_power_of_two().max(value_cost::ARRAY_MIN_CAPACITY);
        self.add(value_cost::ARRAY.saturating_add(capacity.saturating_mul(value_cost::ARRAY_SLOT)));
        self.indent(depth)?;
        self.expect(b"]")
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let Some(h) = self.b.get(self.i..self.i + 4) else {
            return self.err("short \\u escape");
        };
        let mut v = 0u32;
        for &c in h {
            v = v * 16
                + match c {
                    b'0'..=b'9' => u32::from(c - b'0'),
                    b'a'..=b'f' => u32::from(c - b'a' + 10),
                    _ => return self.err("\\u escape is not four lowercase hex digits"),
                };
        }
        self.i += 4;
        Ok(v)
    }

    /// One string, with its decoded bytes appended to `out` when given.
    fn string(&mut self, mut out: Option<&mut Vec<u8>>) -> Result<(), String> {
        self.expect(b"\"")?;
        loop {
            let Some(c) = self.peek() else {
                return self.err("unterminated string");
            };
            self.i += 1;
            match c {
                b'"' => return Ok(()),
                b'\\' => {
                    let Some(e) = self.peek() else {
                        return self.err("unterminated escape");
                    };
                    self.i += 1;
                    let short = match e {
                        b'"' => Some(b'"'),
                        b'\\' => Some(b'\\'),
                        b'b' => Some(0x08),
                        b'f' => Some(0x0c),
                        b'n' => Some(b'\n'),
                        b'r' => Some(b'\r'),
                        b't' => Some(b'\t'),
                        b'u' => None,
                        _ => return self.err("escape outside the canonical set"),
                    };
                    let ch = match short {
                        Some(b) => char::from(b),
                        None => self.unicode_escape()?,
                    };
                    if let Some(o) = out.as_deref_mut() {
                        let mut buf = [0u8; 4];
                        o.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                    }
                }
                _ => {
                    let raw_ok = match self.layout {
                        // ensure_ascii: printable ASCII only.
                        Layout::PythonCompact => (0x20..=0x7e).contains(&c),
                        // Everything but control characters, as UTF-8.
                        Layout::SerdePretty => c >= 0x20,
                    };
                    if !raw_ok {
                        self.i -= 1;
                        return self.err("character must be escaped in this layout");
                    }
                    if let Some(o) = out.as_deref_mut() {
                        o.push(c);
                    }
                }
            }
        }
    }

    /// A `\u` escape after its `u`: canonical only for characters the layout
    /// cannot write raw or with a short escape.
    fn unicode_escape(&mut self) -> Result<char, String> {
        let short_escaped = |u: u32| matches!(u, 0x08 | 0x09 | 0x0a | 0x0c | 0x0d);
        let u = self.hex4()?;
        match self.layout {
            Layout::SerdePretty => {
                if u < 0x20 && !short_escaped(u) {
                    Ok(char::from_u32(u).expect("control character"))
                } else {
                    self.err("\\u escape not used by serde_json for this character")
                }
            }
            Layout::PythonCompact => {
                if (0xd800..0xdc00).contains(&u) {
                    self.expect(b"\\u")?;
                    let lo = self.hex4()?;
                    if !(0xdc00..0xe000).contains(&lo) {
                        return self.err("high surrogate without a low surrogate");
                    }
                    let cp = 0x10000 + ((u - 0xd800) << 10) + (lo - 0xdc00);
                    Ok(char::from_u32(cp).expect("astral code point"))
                } else if (0xdc00..0xe000).contains(&u) {
                    self.err("lone low surrogate")
                } else if (u < 0x20 && !short_escaped(u)) || u >= 0x7f {
                    Ok(char::from_u32(u).expect("not a surrogate"))
                } else {
                    self.err("\\u escape for a character Python writes otherwise")
                }
            }
        }
    }

    fn number(&mut self) -> Result<(), String> {
        let start = self.i;
        let digits = |c: &mut Self| {
            let s = c.i;
            while matches!(c.peek(), Some(b'0'..=b'9')) {
                c.i += 1;
            }
            c.i - s
        };
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        let int_start = self.i;
        let n = digits(self);
        if n == 0 || (n > 1 && self.b[int_start] == b'0') {
            return self.err("integer part is not canonical");
        }
        let mut float = false;
        if self.peek() == Some(b'.') {
            self.i += 1;
            if digits(self) == 0 {
                return self.err("empty fraction");
            }
            float = true;
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.i += 1;
            }
            if digits(self) == 0 {
                return self.err("empty exponent");
            }
            float = true;
        }
        let lit = std::str::from_utf8(&self.b[start..self.i]).expect("ASCII");
        if !float {
            let fits = if let Some(abs) = lit.strip_prefix('-') {
                abs != "0" && lit.parse::<i64>().is_ok()
            } else {
                lit.parse::<u64>().is_ok()
            };
            if !fits {
                self.i = start;
                return self.err("integer outside i64/u64 or negative zero");
            }
            return Ok(());
        }
        let v: f64 = lit.parse().expect("JSON number grammar");
        let mut buf = [0u8; FLOAT_BYTES];
        if float_str(v, self.layout, &mut buf) != Some(lit) {
            self.i = start;
            return self.err("float is not spelled canonically");
        }
        Ok(())
    }
}

/// Longest float spelling: sign, 17 digits, point, up to 4 zeros, exponent.
const FLOAT_BYTES: usize = 32;

/// Spells a finite float as `layout` does; `None` if it is not finite.
///
/// Both layouts use the shortest digits that round-trip (the closest of them
/// when several have that length), as CPython's `repr` and `serde_json` do.
/// They differ in notation:
///
/// - Python (`repr`): fixed when the decimal point position `p` (value
///   `0.d1d2… × 10^p`) is in -3..=16, otherwise `d[.ddd]e±XX` with at least
///   two exponent digits (`1e-05`, `1e+16`).
/// - `serde_json` 1.0.146 and later (`zmij`): fixed when `p` is in -4..=16,
///   otherwise `d[.ddd]e±X` with no padding (`1e-7`, `1e+16`).
///
/// Fixed notation always has a fraction (`100.0`, `0.0001`, `-0.0`).
fn float_str(v: f64, layout: Layout, out: &mut [u8; FLOAT_BYTES]) -> Option<&str> {
    if !v.is_finite() {
        return None;
    }
    // Shortest digits and point position, from ryu's spelling.
    let mut ryu = ryu::Buffer::new();
    let r = ryu.format_finite(v.abs());
    let (mant, exp) = match r.find('e') {
        Some(k) => (&r[..k], r[k + 1..].parse::<i32>().expect("ryu exponent")),
        None => (r, 0),
    };
    let (int, frac) = mant.split_once('.').unwrap_or((mant, ""));
    let mut all = [0u8; 40];
    let mut n = 0usize;
    for &d in int.as_bytes().iter().chain(frac.as_bytes()) {
        all[n] = d;
        n += 1;
    }
    let lead = all[..n].iter().take_while(|&&d| d == b'0').count();
    let trail = all[lead..n]
        .iter()
        .rev()
        .take_while(|&&d| d == b'0')
        .count();
    let digits = &all[lead..n - trail];
    let point = int.len() as i32 + exp - lead as i32;
    let k = digits.len() as i32;

    let mut len = 0usize;
    let mut push = |b: u8| {
        out[len] = b;
        len += 1;
    };
    if v.is_sign_negative() {
        push(b'-');
    }
    let (fixed, pad) = match layout {
        Layout::PythonCompact => ((-3..=16).contains(&point), 2),
        Layout::SerdePretty => ((-4..=16).contains(&point), 1),
    };
    if digits.is_empty() {
        b"0.0".iter().for_each(|&b| push(b));
    } else if fixed {
        if point <= 0 {
            push(b'0');
            push(b'.');
            (0..-point).for_each(|_| push(b'0'));
            digits.iter().for_each(|&d| push(d));
        } else if point < k {
            digits[..point as usize].iter().for_each(|&d| push(d));
            push(b'.');
            digits[point as usize..].iter().for_each(|&d| push(d));
        } else {
            digits.iter().for_each(|&d| push(d));
            (0..point - k).for_each(|_| push(b'0'));
            push(b'.');
            push(b'0');
        }
    } else {
        push(digits[0]);
        if k > 1 {
            push(b'.');
            digits[1..].iter().for_each(|&d| push(d));
        }
        let e = point - 1;
        push(b'e');
        push(if e < 0 { b'-' } else { b'+' });
        let a = e.unsigned_abs();
        let width = if a >= 100 {
            3
        } else if a >= 10 {
            2
        } else {
            pad.max(1)
        };
        for w in (0..width).rev() {
            push(b'0' + (a / 10u32.pow(w) % 10) as u8);
        }
    }
    std::str::from_utf8(&out[..len]).ok()
}

/// The canonical bytes of `v` under `layout` (for tools that write role
/// files, and for tests).
pub fn encode(v: &Value, layout: Layout) -> Vec<u8> {
    let mut out = Vec::new();
    write_value(v, layout, 0, &mut out);
    out.push(b'\n');
    out
}

fn write_indent(layout: Layout, depth: usize, o: &mut Vec<u8>) {
    if layout == Layout::SerdePretty {
        o.push(b'\n');
        (0..depth).for_each(|_| o.extend_from_slice(b"  "));
    }
}

fn write_value(v: &Value, layout: Layout, depth: usize, o: &mut Vec<u8>) {
    match v {
        Value::Null => o.extend_from_slice(b"null"),
        Value::Bool(b) => o.extend_from_slice(if *b { b"true" } else { b"false" }),
        Value::Number(n) => match n.as_f64().filter(|_| n.is_f64()) {
            Some(f) => {
                let mut buf = [0u8; FLOAT_BYTES];
                o.extend_from_slice(float_str(f, layout, &mut buf).expect("finite").as_bytes());
            }
            None => o.extend_from_slice(n.to_string().as_bytes()),
        },
        Value::String(s) => write_string(s, layout, o),
        Value::Array(a) if a.is_empty() => o.extend_from_slice(b"[]"),
        Value::Array(a) => {
            o.push(b'[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    o.push(b',');
                }
                write_indent(layout, depth + 1, o);
                write_value(x, layout, depth + 1, o);
            }
            write_indent(layout, depth, o);
            o.push(b']');
        }
        Value::Object(m) if m.is_empty() => o.extend_from_slice(b"{}"),
        Value::Object(m) => {
            // Key bytes order is code point order, as Python's sort_keys.
            // Sorted here rather than trusted from the map, which keeps
            // insertion order in a build with serde_json's preserve_order.
            let mut entries: Vec<(&String, &Value)> = m.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            o.push(b'{');
            for (i, (k, x)) in entries.into_iter().enumerate() {
                if i > 0 {
                    o.push(b',');
                }
                write_indent(layout, depth + 1, o);
                write_string(k, layout, o);
                o.extend_from_slice(match layout {
                    Layout::PythonCompact => b":",
                    Layout::SerdePretty => b": ",
                });
                write_value(x, layout, depth + 1, o);
            }
            write_indent(layout, depth, o);
            o.push(b'}');
        }
    }
}

fn write_string(s: &str, layout: Layout, o: &mut Vec<u8>) {
    fn unit(o: &mut Vec<u8>, u: u16) {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        o.extend_from_slice(b"\\u");
        for k in [12, 8, 4, 0] {
            o.push(HEX[usize::from((u >> k) & 0xf)]);
        }
    }
    o.push(b'"');
    for c in s.chars() {
        match c {
            '"' => o.extend_from_slice(b"\\\""),
            '\\' => o.extend_from_slice(b"\\\\"),
            '\u{08}' => o.extend_from_slice(b"\\b"),
            '\u{0c}' => o.extend_from_slice(b"\\f"),
            '\n' => o.extend_from_slice(b"\\n"),
            '\r' => o.extend_from_slice(b"\\r"),
            '\t' => o.extend_from_slice(b"\\t"),
            ' '..='~' => o.push(c as u8),
            _ if c < ' ' => unit(o, c as u16),
            _ => match layout {
                Layout::SerdePretty => {
                    let mut buf = [0u8; 4];
                    o.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                Layout::PythonCompact => {
                    let mut buf = [0u16; 2];
                    for &u in c.encode_utf16(&mut buf).iter() {
                        unit(o, u);
                    }
                }
            },
        }
    }
    o.push(b'"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repr(v: f64) -> String {
        float_str(v, Layout::PythonCompact, &mut [0; FLOAT_BYTES])
            .unwrap()
            .to_string()
    }

    fn serde(v: f64) -> String {
        float_str(v, Layout::SerdePretty, &mut [0; FLOAT_BYTES])
            .unwrap()
            .to_string()
    }

    #[test]
    fn serde_float_spelling_matches_serde_json() {
        for v in [
            0.0,
            -0.0,
            1.0,
            100.0,
            0.1,
            123.456,
            1e-4,
            1e-5,
            1.2e-5,
            1e-6,
            1.5e-7,
            9.42558328795764e-6,
            1e15,
            1234567890123456.0,
            1e16,
            1.2345678901234568e17,
            2.5e20,
            5e-324,
            1.7976931348623157e308,
            -2.2250738585072014e-308,
            0.0004385964912280702,
        ] {
            assert_eq!(serde(v), serde_json::to_string(&v).unwrap(), "{v:e}");
        }
        assert_eq!(serde(1e16), "1e+16");
        assert_eq!(serde(1.5e-7), "1.5e-7");
        assert_eq!(serde(1e-5), "0.00001");
        assert_eq!(repr(1e-5), "1e-05");
    }

    #[test]
    fn python_float_repr_matches_cpython() {
        // Values and their CPython 3.12 repr.
        for (v, r) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1.0, "1.0"),
            (100.0, "100.0"),
            (0.1, "0.1"),
            (123.456, "123.456"),
            (0.0001, "0.0001"),
            (0.00012, "0.00012"),
            (1e-05, "1e-05"),
            (1.5e-07, "1.5e-07"),
            (8.226390259953932e-05, "8.226390259953932e-05"),
            (1e15, "1000000000000000.0"),
            (1234567890123456.0, "1234567890123456.0"),
            (1e16, "1e+16"),
            (1.2345678901234568e17, "1.2345678901234568e+17"),
            (2.5e20, "2.5e+20"),
            (5e-324, "5e-324"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (-2.2250738585072014e-308, "-2.2250738585072014e-308"),
            (0.0004385964912280702, "0.0004385964912280702"),
            (1.8776105792643432, "1.8776105792643432"),
        ] {
            assert_eq!(repr(v), r, "{v:e}");
        }
    }

    #[test]
    fn both_layouts_round_trip_and_refuse_padding() {
        let v: Value = serde_json::from_str(
            r#"{"b":[1,-2,0.5,1e-7,1e16,"x\u00e9\n\u0001\"\\\ud83d\ude00"],"a":{},"c":[],"d":null,"e":true}"#,
        )
        .unwrap();
        for layout in [Layout::PythonCompact, Layout::SerdePretty] {
            let bytes = encode(&v, layout);
            check(&bytes, layout).unwrap();
            // Compared by value: with serde_json's arbitrary_precision a
            // number keeps its spelling, and `1e-7` is written `1e-07`.
            let back = serde_json::from_slice::<Value>(&bytes).unwrap();
            let by_value = crate::guard::as_the_scorer_reads;
            assert_eq!(by_value(back.clone()), by_value(v.clone()));
            assert_eq!(encode(&back, layout), bytes);
            let text = String::from_utf8(bytes.clone()).unwrap();
            // Padding and respelling are refused.
            for bad in [
                format!("{text}\n"),
                format!(" {text}"),
                text.replacen("0.5", "0.50", 1),
                text.replacen("true", "true ", 1),
                text.replacen("\"x", "\"\\u0078", 1),
                text.replacen("{", "{\"a\":0,", 1),
                text.trim_end().to_string(),
            ] {
                assert!(
                    check(bad.as_bytes(), layout).is_err(),
                    "{layout:?}: {bad:?}"
                );
            }
        }
        let py = String::from_utf8(encode(&v, Layout::PythonCompact)).unwrap();
        assert!(py.contains(r#""x\u00e9\n\u0001\"\\\ud83d\ude00""#), "{py}");
        assert!(py.contains("1e-07") && py.contains("1e+16"));
        let pretty = String::from_utf8(encode(&v, Layout::SerdePretty)).unwrap();
        assert!(pretty.contains("1e-7") && pretty.contains("1e+16"));
        // The pretty layout is serde_json's own writer, keys sorted. A build
        // with serde_json's arbitrary_precision writes each number's source
        // text instead, so the comparison holds only where serde_json
        // formats floats itself.
        let arbitrary_precision = serde_json::from_str::<serde_json::Number>("1.50")
            .unwrap()
            .to_string()
            == "1.50";
        if !arbitrary_precision {
            assert_eq!(
                encode(&v, Layout::SerdePretty),
                [
                    serde_json::to_vec_pretty(&crate::sorted_keys(v.clone())).unwrap(),
                    b"\n".to_vec()
                ]
                .concat()
            );
        }
        // Duplicate and unsorted keys; uppercase hex; integers past u64.
        assert!(check(b"{\"a\":1,\"a\":1}\n", Layout::PythonCompact).is_err());
        assert!(check(b"{\"b\":1,\"a\":1}\n", Layout::PythonCompact).is_err());
        assert!(check(b"\"\\u00E9\"\n", Layout::PythonCompact).is_err());
        assert!(check(b"18446744073709551616\n", Layout::PythonCompact).is_err());
        assert!(check(b"-0\n", Layout::PythonCompact).is_err());
        assert!(check(b"18446744073709551615\n", Layout::PythonCompact).is_ok());
        // Depth beyond serde_json's recursion limit.
        let deep = format!(
            "{}{}\n",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        assert!(check(deep.as_bytes(), Layout::PythonCompact).is_err());
    }
}
