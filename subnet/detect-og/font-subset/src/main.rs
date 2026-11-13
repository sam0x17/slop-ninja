//! Builds the card renderer's font subset (`src/fonts/inter-400.ttf` and
//! `src/fonts/inter-700.ttf`) from two static Inter instances.
//!
//! ```sh
//! cargo run --release -p slop-ninja-detect-og-font-subset -- <Inter 400 .ttf> <Inter 700 .ttf> <out dir>
//! ```
//!
//! The sources are the static TrueType instances of Inter 4.001 (weights 400
//! and 700, optical size 14) by The Inter Project Authors, SIL Open Font
//! License 1.1; their SHA-256 digests are [`SOURCES`], and the tool refuses
//! any other file, so the same inputs always give the same bytes.
//!
//! The subset keeps the glyphs of [`chars`] (printable ASCII, the middle dot
//! and the ellipsis), the components of any composite among them and
//! `.notdef`, renumbered in their original order. It writes `cmap` (one
//! format 4 subtable), `glyf` and `loca` (long offsets), `head`, `hhea`,
//! `hmtx`, `maxp`, `name` and `OS/2` (copied; counts updated), `post`
//! (version 3, no glyph names) and a format 0 `kern` table holding the
//! horizontal advance adjustments that the source's GPOS `kern` feature
//! makes for the Latin script between every pair of kept glyphs. GSUB, GPOS,
//! GDEF, STAT, `gasp` and `prep` are left out: the renderer shapes with no
//! substitutions, kerns from `kern` and does not hint.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The accepted sources: weight, SHA-256 of the file.
pub const SOURCES: [(u16, &str); 2] = [
    (400, "1b08e7fc267a5c7e1d614100f604b83e7e8a0be241f0f288faa2b3ac93a683ba"),
    (700, "b37284b5701b6b168dfc770aa1a4ac492106422fd3ba76bc7641e37434e8019c"),
];

/// The characters a card can print.
pub fn chars() -> Vec<char> {
    let mut v: Vec<char> = (' '..='~').collect();
    v.push('\u{00B7}'); // middle dot (model line, secret label)
    v.push('\u{2026}'); // ellipsis (a long host or model line cut short)
    v
}

type R<T> = Result<T, String>;

fn u16_at(b: &[u8], o: usize) -> R<u16> {
    b.get(o..o + 2).map(|s| u16::from_be_bytes([s[0], s[1]])).ok_or_else(|| format!("read u16 at {o}"))
}

fn i16_at(b: &[u8], o: usize) -> R<i16> {
    u16_at(b, o).map(|v| v as i16)
}

fn u32_at(b: &[u8], o: usize) -> R<u32> {
    b.get(o..o + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]])).ok_or_else(|| format!("read u32 at {o}"))
}

fn slice(b: &[u8], o: usize, n: usize) -> R<&[u8]> {
    b.get(o..o + n).ok_or_else(|| format!("slice {o}+{n} of {}", b.len()))
}

/// A parsed source font: its tables by tag.
struct Font<'a> {
    tables: BTreeMap<[u8; 4], &'a [u8]>,
}

impl<'a> Font<'a> {
    fn parse(data: &'a [u8]) -> R<Font<'a>> {
        if u32_at(data, 0)? != 0x0001_0000 {
            return Err("not a TrueType font (sfnt version)".into());
        }
        let n = u16_at(data, 4)? as usize;
        let mut tables = BTreeMap::new();
        for i in 0..n {
            let rec = 12 + 16 * i;
            let tag: [u8; 4] = slice(data, rec, 4)?.try_into().map_err(|_| "tag")?;
            let off = u32_at(data, rec + 8)? as usize;
            let len = u32_at(data, rec + 12)? as usize;
            tables.insert(tag, slice(data, off, len)?);
        }
        Ok(Font { tables })
    }

    fn table(&self, tag: &[u8; 4]) -> R<&'a [u8]> {
        self.tables.get(tag).copied().ok_or_else(|| format!("no {} table", String::from_utf8_lossy(tag)))
    }

    fn num_glyphs(&self) -> R<u16> {
        u16_at(self.table(b"maxp")?, 4)
    }

    /// The glyph of `c` through the best Unicode cmap subtable.
    fn glyph_of(&self, c: char) -> R<u16> {
        let cmap = self.table(b"cmap")?;
        let n = u16_at(cmap, 2)? as usize;
        let mut f4 = None;
        let mut f12 = None;
        for i in 0..n {
            let rec = 4 + 8 * i;
            let (pid, eid) = (u16_at(cmap, rec)?, u16_at(cmap, rec + 2)?);
            let off = u32_at(cmap, rec + 4)? as usize;
            let format = u16_at(cmap, off)?;
            match (pid, eid, format) {
                (3, 10, 12) | (0, 4, 12) | (0, 6, 12) => f12 = Some(off),
                (3, 1, 4) | (0, 3, 4) => f4 = Some(off),
                _ => {}
            }
        }
        let code = c as u32;
        if let Some(off) = f12 {
            let groups = u32_at(cmap, off + 12)? as usize;
            for g in 0..groups {
                let p = off + 16 + 12 * g;
                let (start, end, gid) = (u32_at(cmap, p)?, u32_at(cmap, p + 4)?, u32_at(cmap, p + 8)?);
                if (start..=end).contains(&code) {
                    return u16::try_from(gid + (code - start)).map_err(|_| "gid".into());
                }
            }
            return Ok(0);
        }
        let off = f4.ok_or("no Unicode cmap subtable")?;
        if code > 0xFFFF {
            return Ok(0);
        }
        let seg_x2 = u16_at(cmap, off + 6)? as usize;
        let ends = off + 14;
        let starts = ends + seg_x2 + 2;
        let deltas = starts + seg_x2;
        let ranges = deltas + seg_x2;
        for s in 0..seg_x2 / 2 {
            let end = u16_at(cmap, ends + 2 * s)? as u32;
            let start = u16_at(cmap, starts + 2 * s)? as u32;
            if code > end || code < start {
                continue;
            }
            let delta = u16_at(cmap, deltas + 2 * s)? as u32;
            let ro = u16_at(cmap, ranges + 2 * s)? as usize;
            if ro == 0 {
                return Ok(((code + delta) & 0xFFFF) as u16);
            }
            let addr = ranges + 2 * s + ro + 2 * (code - start) as usize;
            let g = u16_at(cmap, addr)? as u32;
            return Ok(if g == 0 { 0 } else { ((g + delta) & 0xFFFF) as u16 });
        }
        Ok(0)
    }

    /// The glyf bytes of `gid`.
    fn glyph_bytes(&self, gid: u16) -> R<&'a [u8]> {
        let head = self.table(b"head")?;
        let loca = self.table(b"loca")?;
        let glyf = self.table(b"glyf")?;
        let long = i16_at(head, 50)? == 1;
        let g = gid as usize;
        let (a, b) = if long {
            (u32_at(loca, 4 * g)? as usize, u32_at(loca, 4 * g + 4)? as usize)
        } else {
            (2 * u16_at(loca, 2 * g)? as usize, 2 * u16_at(loca, 2 * g + 2)? as usize)
        };
        if b < a {
            return Err(format!("loca of glyph {gid} decreases"));
        }
        slice(glyf, a, b - a)
    }

    /// The advance width and left side bearing of `gid`.
    fn h_metrics(&self, gid: u16) -> R<(u16, i16)> {
        let hhea = self.table(b"hhea")?;
        let hmtx = self.table(b"hmtx")?;
        let n = u16_at(hhea, 34)? as usize;
        let g = gid as usize;
        if g < n {
            Ok((u16_at(hmtx, 4 * g)?, i16_at(hmtx, 4 * g + 2)?))
        } else {
            Ok((u16_at(hmtx, 4 * (n - 1))?, i16_at(hmtx, 4 * n + 2 * (g - n))?))
        }
    }
}

/// The byte offsets of the component glyph ids in a composite glyph, and the
/// ids themselves.
fn components(glyph: &[u8]) -> R<Vec<(usize, u16)>> {
    if glyph.is_empty() || i16_at(glyph, 0)? >= 0 {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut p = 10;
    loop {
        let flags = u16_at(glyph, p)?;
        out.push((p + 2, u16_at(glyph, p + 2)?));
        p += 4;
        p += if flags & 0x0001 != 0 { 4 } else { 2 };
        if flags & 0x0008 != 0 {
            p += 2;
        } else if flags & 0x0040 != 0 {
            p += 4;
        } else if flags & 0x0080 != 0 {
            p += 8;
        }
        if flags & 0x0020 == 0 {
            return Ok(out);
        }
    }
}

// ------------------------------------------------------------------ GPOS kern

/// The coverage index of `gid`, if covered.
fn coverage(t: &[u8], off: usize, gid: u16) -> R<Option<usize>> {
    match u16_at(t, off)? {
        1 => {
            let n = u16_at(t, off + 2)? as usize;
            for i in 0..n {
                if u16_at(t, off + 4 + 2 * i)? == gid {
                    return Ok(Some(i));
                }
            }
            Ok(None)
        }
        2 => {
            let n = u16_at(t, off + 2)? as usize;
            for i in 0..n {
                let r = off + 4 + 6 * i;
                let (s, e, idx) = (u16_at(t, r)?, u16_at(t, r + 2)?, u16_at(t, r + 4)?);
                if (s..=e).contains(&gid) {
                    return Ok(Some(idx as usize + (gid - s) as usize));
                }
            }
            Ok(None)
        }
        f => Err(format!("coverage format {f}")),
    }
}

/// The class of `gid` (0 when not listed).
fn class_of(t: &[u8], off: usize, gid: u16) -> R<u16> {
    match u16_at(t, off)? {
        1 => {
            let start = u16_at(t, off + 2)?;
            let n = u16_at(t, off + 4)?;
            if gid >= start && gid - start < n { u16_at(t, off + 6 + 2 * (gid - start) as usize) } else { Ok(0) }
        }
        2 => {
            let n = u16_at(t, off + 2)? as usize;
            for i in 0..n {
                let r = off + 4 + 6 * i;
                let (s, e, c) = (u16_at(t, r)?, u16_at(t, r + 2)?, u16_at(t, r + 4)?);
                if (s..=e).contains(&gid) {
                    return Ok(c);
                }
            }
            Ok(0)
        }
        f => Err(format!("class definition format {f}")),
    }
}

fn value_record_len(format: u16) -> usize {
    2 * (format & 0x00FF).count_ones() as usize
}

/// The XAdvance of a value record, if its format carries one.
fn x_advance(t: &[u8], rec: usize, format: u16) -> R<i16> {
    if format & 0x0004 == 0 {
        return Ok(0);
    }
    i16_at(t, rec + 2 * (format & 0x0003).count_ones() as usize)
}

/// Whether a value record holds a non-zero field other than `except` (a
/// format bit), so it does more than the kern table can say.
fn other_values(t: &[u8], rec: usize, format: u16, except: Option<u16>) -> R<bool> {
    let mut p = rec;
    for bit in 0..8u16 {
        if format & (1 << bit) != 0 {
            if Some(bit) != except && u16_at(t, p)? != 0 {
                return Ok(true);
            }
            p += 2;
        }
    }
    Ok(false)
}

/// One PairPos subtable: `Some(advance)` when it applies to the pair.
fn pair_pos(t: &[u8], st: usize, a: u16, b: u16, strict: &mut Vec<String>) -> R<Option<i16>> {
    let format = u16_at(t, st)?;
    let cov = st + u16_at(t, st + 2)? as usize;
    let (vf1, vf2) = (u16_at(t, st + 4)?, u16_at(t, st + 6)?);
    let Some(ci) = coverage(t, cov, a)? else { return Ok(None) };
    let (l1, l2) = (value_record_len(vf1), value_record_len(vf2));
    match format {
        1 => {
            let set = st + u16_at(t, st + 10 + 2 * ci)? as usize;
            let n = u16_at(t, set)? as usize;
            let rec_len = 2 + l1 + l2;
            for i in 0..n {
                let r = set + 2 + i * rec_len;
                if u16_at(t, r)? == b {
                    if other_values(t, r + 2, vf1, Some(2))? || other_values(t, r + 2 + l1, vf2, None)? {
                        strict.push(format!("pair {a},{b}: a value beside the first advance"));
                    }
                    return Ok(Some(x_advance(t, r + 2, vf1)?));
                }
            }
            Ok(None)
        }
        2 => {
            let cd1 = st + u16_at(t, st + 8)? as usize;
            let cd2 = st + u16_at(t, st + 10)? as usize;
            let (n1, n2) = (u16_at(t, st + 12)?, u16_at(t, st + 14)?);
            let (c1, c2) = (class_of(t, cd1, a)?, class_of(t, cd2, b)?);
            if c1 >= n1 || c2 >= n2 {
                return Ok(None);
            }
            let rec_len = l1 + l2;
            let r = st + 16 + (c1 as usize * n2 as usize + c2 as usize) * rec_len;
            if other_values(t, r, vf1, Some(2))? || other_values(t, r + l1, vf2, None)? {
                strict.push(format!("pair {a},{b}: a value beside the first advance"));
            }
            Ok(Some(x_advance(t, r, vf1)?))
        }
        f => Err(format!("PairPos format {f}")),
    }
}

/// The `kern` feature's lookups for the Latin script (or the default
/// script), in lookup-list order.
fn kern_lookups(gpos: &[u8]) -> R<Vec<usize>> {
    let scripts = u16_at(gpos, 4)? as usize;
    let features = u16_at(gpos, 6)? as usize;
    let n = u16_at(gpos, scripts)? as usize;
    let mut chosen = None;
    for i in 0..n {
        let rec = scripts + 2 + 6 * i;
        let tag = slice(gpos, rec, 4)?;
        let off = scripts + u16_at(gpos, rec + 4)? as usize;
        // The Latin script, else the default script.
        if tag == b"latn" || (tag == b"DFLT" && chosen.is_none()) {
            chosen = Some(off);
        }
    }
    let script = chosen.ok_or("no latn or DFLT script")?;
    let lang = u16_at(gpos, script)? as usize;
    if lang == 0 {
        return Err("no default language system".into());
    }
    let lang = script + lang;
    let mut feature_indices: Vec<u16> = Vec::new();
    let required = u16_at(gpos, lang + 2)?;
    if required != 0xFFFF {
        feature_indices.push(required);
    }
    let k = u16_at(gpos, lang + 4)? as usize;
    for i in 0..k {
        feature_indices.push(u16_at(gpos, lang + 6 + 2 * i)?);
    }
    let mut lookups = BTreeSet::new();
    for fi in feature_indices {
        let rec = features + 2 + 6 * fi as usize;
        if slice(gpos, rec, 4)? != b"kern" {
            continue;
        }
        let f = features + u16_at(gpos, rec + 4)? as usize;
        let m = u16_at(gpos, f + 2)? as usize;
        for j in 0..m {
            lookups.insert(u16_at(gpos, f + 4 + 2 * j)? as usize);
        }
    }
    Ok(lookups.into_iter().collect())
}

/// The pair subtables of one lookup (extension lookups resolved), as
/// absolute offsets into GPOS.
fn pair_subtables(gpos: &[u8], lookup: usize) -> R<Vec<usize>> {
    let list = u16_at(gpos, 8)? as usize;
    let l = list + u16_at(gpos, list + 2 + 2 * lookup)? as usize;
    let kind = u16_at(gpos, l)?;
    let n = u16_at(gpos, l + 4)? as usize;
    let mut out = Vec::new();
    for i in 0..n {
        let st = l + u16_at(gpos, l + 6 + 2 * i)? as usize;
        match kind {
            2 => out.push(st),
            9 if u16_at(gpos, st + 2)? == 2 => out.push(st + u32_at(gpos, st + 4)? as usize),
            _ => {}
        }
    }
    Ok(out)
}

/// The summed advance adjustment GPOS `kern` makes between `a` and `b`: in
/// each lookup, the first subtable that applies.
fn gpos_kern(gpos: &[u8], lookups: &[(usize, Vec<usize>)], a: u16, b: u16, strict: &mut Vec<String>) -> R<i32> {
    let mut sum = 0i32;
    for (_, subtables) in lookups {
        for &st in subtables {
            if let Some(v) = pair_pos(gpos, st, a, b, strict)? {
                sum += i32::from(v);
                break;
            }
        }
    }
    Ok(sum)
}

// ------------------------------------------------------------------ writing

fn put16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}

fn set16(v: &mut [u8], o: usize, x: u16) {
    v[o..o + 2].copy_from_slice(&x.to_be_bytes());
}

fn set32(v: &mut [u8], o: usize, x: u32) {
    v[o..o + 4].copy_from_slice(&x.to_be_bytes());
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    for chunk in data.chunks(4) {
        let mut w = [0u8; 4];
        w[..chunk.len()].copy_from_slice(chunk);
        sum = sum.wrapping_add(u32::from_be_bytes(w));
    }
    sum
}

/// `(searchRange, entrySelector, rangeShift)` for `n` units of `size` bytes.
fn search_params(n: u16, size: u16) -> (u16, u16, u16) {
    let mut sel = 0u16;
    while (1u32 << (sel + 1)) <= u32::from(n) {
        sel += 1;
    }
    let range = (1u16 << sel) * size;
    (range, sel, n * size - range)
}

/// A format 4 cmap for `map` (code to new glyph id, all in the BMP), with
/// platform 0/3 and 3/1 records pointing at it.
fn build_cmap(map: &BTreeMap<u32, u16>) -> Vec<u8> {
    // Segments of consecutive codes with a constant delta.
    let mut segs: Vec<(u16, u16, u16)> = Vec::new(); // start, end, delta
    for (&code, &gid) in map {
        let code = code as u16;
        let delta = gid.wrapping_sub(code);
        match segs.last_mut() {
            Some((_, end, d)) if *end + 1 == code && *d == delta => *end = code,
            _ => segs.push((code, code, delta)),
        }
    }
    segs.push((0xFFFF, 0xFFFF, 1));
    let n = segs.len() as u16;
    let mut sub = Vec::new();
    put16(&mut sub, 4);
    put16(&mut sub, 16 + 8 * n);
    put16(&mut sub, 0);
    let (range, sel, shift) = search_params(n, 2);
    put16(&mut sub, 2 * n);
    put16(&mut sub, range);
    put16(&mut sub, sel);
    put16(&mut sub, shift);
    for s in &segs {
        put16(&mut sub, s.1);
    }
    put16(&mut sub, 0);
    for s in &segs {
        put16(&mut sub, s.0);
    }
    for s in &segs {
        put16(&mut sub, s.2);
    }
    for _ in &segs {
        put16(&mut sub, 0);
    }
    let mut out = Vec::new();
    put16(&mut out, 0);
    put16(&mut out, 2);
    for (pid, eid) in [(0u16, 3u16), (3, 1)] {
        put16(&mut out, pid);
        put16(&mut out, eid);
        put32(&mut out, 4 + 8 * 2);
    }
    out.extend_from_slice(&sub);
    out
}

fn build_kern(pairs: &BTreeMap<(u16, u16), i16>) -> R<Vec<u8>> {
    let n = u16::try_from(pairs.len()).map_err(|_| "too many kern pairs")?;
    let len = 14 + 6 * pairs.len();
    let len16 = u16::try_from(len).map_err(|_| "kern subtable over 64 KiB")?;
    let mut out = Vec::new();
    put16(&mut out, 0); // version
    put16(&mut out, 1); // nTables
    put16(&mut out, 0); // subtable version
    put16(&mut out, len16);
    put16(&mut out, 0x0001); // horizontal, format 0
    put16(&mut out, n);
    let (range, sel, shift) = if n == 0 { (0, 0, 0) } else { search_params(n, 6) };
    put16(&mut out, range);
    put16(&mut out, sel);
    put16(&mut out, shift);
    for (&(l, r), &v) in pairs {
        put16(&mut out, l);
        put16(&mut out, r);
        put16(&mut out, v as u16);
    }
    Ok(out)
}

/// The subset of one source font.
fn subset(src: &[u8], label: &str) -> R<Vec<u8>> {
    let font = Font::parse(src)?;
    let num_glyphs = font.num_glyphs()?;
    // The kept glyphs: .notdef, every character's glyph and their components.
    let mut code_old: BTreeMap<u32, u16> = BTreeMap::new();
    for c in chars() {
        let g = font.glyph_of(c)?;
        if g == 0 {
            return Err(format!("{label}: no glyph for U+{:04X}", c as u32));
        }
        code_old.insert(c as u32, g);
    }
    let mut keep: BTreeSet<u16> = BTreeSet::from([0]);
    let mut todo: Vec<u16> = code_old.values().copied().collect();
    while let Some(g) = todo.pop() {
        if g >= num_glyphs {
            return Err(format!("{label}: glyph {g} out of range"));
        }
        for (_, comp) in components(font.glyph_bytes(g)?)? {
            if keep.insert(comp) {
                todo.push(comp);
            }
        }
        keep.insert(g);
    }
    let order: Vec<u16> = keep.iter().copied().collect();
    let new_of: BTreeMap<u16, u16> = order.iter().enumerate().map(|(i, &g)| (g, i as u16)).collect();
    let n = order.len() as u16;

    // glyf and loca (long), with component ids renumbered.
    let mut glyf = Vec::new();
    let mut loca = Vec::new();
    for &g in &order {
        put32(&mut loca, glyf.len() as u32);
        let bytes = font.glyph_bytes(g)?;
        let start = glyf.len();
        glyf.extend_from_slice(bytes);
        for (off, comp) in components(bytes)? {
            set16(&mut glyf, start + off, new_of[&comp]);
        }
        while glyf.len() % 4 != 0 {
            glyf.push(0);
        }
    }
    put32(&mut loca, glyf.len() as u32);

    // hmtx (a full metric per glyph) and hhea.
    let mut hmtx = Vec::new();
    let mut max_advance = 0u16;
    for &g in &order {
        let (adv, lsb) = font.h_metrics(g)?;
        max_advance = max_advance.max(adv);
        put16(&mut hmtx, adv);
        put16(&mut hmtx, lsb as u16);
    }
    let mut hhea = font.table(b"hhea")?.to_vec();
    set16(&mut hhea, 10, max_advance);
    set16(&mut hhea, 34, n);

    let mut maxp = font.table(b"maxp")?.to_vec();
    set16(&mut maxp, 4, n);

    let mut head = font.table(b"head")?.to_vec();
    set32(&mut head, 8, 0);
    set16(&mut head, 50, 1);

    let mut post = slice(font.table(b"post")?, 0, 32)?.to_vec();
    set32(&mut post, 0, 0x0003_0000);

    let code_new: BTreeMap<u32, u16> = code_old.iter().map(|(&c, g)| (c, new_of[g])).collect();
    let cmap = build_cmap(&code_new);

    // kern from GPOS.
    let gpos = font.table(b"GPOS")?;
    let lookups: Vec<(usize, Vec<usize>)> =
        kern_lookups(gpos)?.into_iter().map(|l| pair_subtables(gpos, l).map(|s| (l, s))).collect::<R<_>>()?;
    let mut strict = Vec::new();
    let mut pairs = BTreeMap::new();
    let mapped: Vec<u16> = code_old.values().copied().collect::<BTreeSet<_>>().into_iter().collect();
    for &a in &mapped {
        for &b in &mapped {
            let v = gpos_kern(gpos, &lookups, a, b, &mut strict)?;
            if v != 0 {
                let v = i16::try_from(v).map_err(|_| "kern value out of range")?;
                pairs.insert((new_of[&a], new_of[&b]), v);
            }
        }
    }
    if !strict.is_empty() {
        return Err(format!("{label}: {} pair values the kern table cannot hold, e.g. {}", strict.len(), strict[0]));
    }
    let kern = build_kern(&pairs)?;

    let mut tables: BTreeMap<[u8; 4], Vec<u8>> = BTreeMap::new();
    tables.insert(*b"OS/2", font.table(b"OS/2")?.to_vec());
    tables.insert(*b"cmap", cmap);
    tables.insert(*b"glyf", glyf);
    tables.insert(*b"head", head);
    tables.insert(*b"hhea", hhea);
    tables.insert(*b"hmtx", hmtx);
    tables.insert(*b"kern", kern);
    tables.insert(*b"loca", loca);
    tables.insert(*b"maxp", maxp);
    tables.insert(*b"name", font.table(b"name")?.to_vec());
    tables.insert(*b"post", post);

    let count = tables.len() as u16;
    let mut out = Vec::new();
    put32(&mut out, 0x0001_0000);
    put16(&mut out, count);
    let (range, sel, shift) = search_params(count, 16);
    put16(&mut out, range);
    put16(&mut out, sel);
    put16(&mut out, shift);
    let mut offset = 12 + 16 * tables.len();
    let mut head_at = 0;
    for (tag, data) in &tables {
        out.extend_from_slice(tag);
        put32(&mut out, checksum(data));
        put32(&mut out, offset as u32);
        put32(&mut out, data.len() as u32);
        if tag == b"head" {
            head_at = offset;
        }
        offset += data.len().div_ceil(4) * 4;
    }
    for data in tables.values() {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    let adjust = 0xB1B0_AFBAu32.wrapping_sub(checksum(&out));
    set32(&mut out, head_at + 8, adjust);
    eprintln!(
        "{label}: {} of {num_glyphs} glyphs, {} characters, {} kern pairs from {} lookups, {} bytes",
        n,
        code_new.len(),
        pairs.len(),
        lookups.len(),
        out.len()
    );
    Ok(out)
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [w400, w700, out] = args.as_slice() else {
        eprintln!("usage: font-subset <Inter 400 .ttf> <Inter 700 .ttf> <out dir>");
        std::process::exit(2);
    };
    for ((weight, digest), path) in SOURCES.iter().zip([w400, w700]) {
        let src = std::fs::read(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        let got = hex(&Sha256::digest(&src));
        assert_eq!(&got, digest, "{path} is not the accepted Inter {weight} source");
        let bytes = subset(&src, &format!("inter-{weight}")).unwrap_or_else(|e| panic!("inter-{weight}: {e}"));
        let dst = std::path::Path::new(out).join(format!("inter-{weight}.ttf"));
        std::fs::write(&dst, &bytes).unwrap_or_else(|e| panic!("write {}: {e}", dst.display()));
        println!("{}  inter-{weight}.ttf", hex(&Sha256::digest(&bytes)));
    }
}
