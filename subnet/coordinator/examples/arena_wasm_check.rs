//! Checks a built coordinator module (the cdylib or worker-build's
//! `build/index_bg.wasm`) against the memory model it was built with:
//!
//! - one linear memory, defined in the module, with `maximum =
//!   slop_ninja_arena::model::LINEAR_MEMORY_PAGES` (the ceiling `build.rs`
//!   links) and, in a build whose arena enforces, `initial` equal to it (the
//!   fixed memory: it never grows at run time); an `observe` build's initial
//!   memory is at most the ceiling (it grows under it);
//! - the hand-written Worker exports (`fetch`, `scheduled`, `queue`) and the
//!   hand-written Durable Object classes (`ChainJournal`, `EvalCohort`, as
//!   wasm-bindgen's `<class>_new`, `<class>_fetch` exports) are present;
//! - the module's static data and stack end (`__heap_base`, where the arena's
//!   pool starts, when exported; else the end of its highest data segment, a
//!   lower bound, which is what worker-build's output allows: the smoke run
//!   checks the instance's own `static_bytes`) at
//!   or below the static size the route table's fit check assumes
//!   (`arena_routes::STATIC_UPPER_BYTES`), so growth in static data fails
//!   here instead of silently invalidating the build-time fit.
//!
//! Usage: `cargo run --example arena_wasm_check -- <module.wasm>`. Exit 0 on
//! success, 1 on a failed check, 2 on a malformed module. Reads only the file.

use std::process::ExitCode;

fn leb_u64(b: &[u8], at: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0u32);
    loop {
        let byte = *b.get(*at)?;
        *at += 1;
        v |= u64::from(byte & 0x7f).checked_shl(shift)?;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

struct Module {
    /// `(flags, initial, maximum)` per memory defined in the module.
    memories: Vec<(u8, u64, Option<u64>)>,
    /// Imported memories (a module that imports its memory cannot be checked).
    imported_memories: u32,
    exports: Vec<String>,
    /// `(name, kind, index)` of every export.
    export_refs: Vec<(String, u8, u64)>,
    /// The `i32.const` initializer of each defined global (`None` otherwise).
    globals: Vec<Option<u64>>,
    /// The highest end (`offset + length`) of an active data segment.
    data_end: u64,
    imported_globals: u64,
}

/// A constant expression `i32.const <v> end` (or any other expression,
/// skipped to its `end`): its value when it is that constant.
fn const_expr(b: &[u8], at: &mut usize) -> Option<Option<u64>> {
    let op = *b.get(*at)?;
    if op == 0x41 {
        *at += 1;
        // Signed LEB128 i32: an address is non-negative.
        let (mut v, mut shift) = (0i64, 0u32);
        loop {
            let byte = *b.get(*at)?;
            *at += 1;
            v |= i64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                if shift < 64 && byte & 0x40 != 0 {
                    v |= -1i64 << shift;
                }
                break;
            }
            if shift > 35 {
                return None;
            }
        }
        if *b.get(*at)? != 0x0b {
            return None;
        }
        *at += 1;
        return Some(u64::try_from(v).ok());
    }
    // Not a plain constant: skip to the expression's `end`.
    while *b.get(*at)? != 0x0b {
        *at += 1;
    }
    *at += 1;
    Some(None)
}

fn parse(b: &[u8]) -> Option<Module> {
    if b.get(..8)? != b"\0asm\x01\0\0\0" {
        return None;
    }
    let mut at = 8;
    let mut m = Module { memories: Vec::new(), imported_memories: 0, exports: Vec::new(), export_refs: Vec::new(), globals: Vec::new(), data_end: 0, imported_globals: 0 };
    while at < b.len() {
        let id = *b.get(at)?;
        at += 1;
        let len = usize::try_from(leb_u64(b, &mut at)?).ok()?;
        let end = at.checked_add(len)?;
        let body = b.get(at..end)?;
        let mut p = 0;
        match id {
            2 => {
                let n = leb_u64(body, &mut p)?;
                for _ in 0..n {
                    for _ in 0..2 {
                        let l = usize::try_from(leb_u64(body, &mut p)?).ok()?;
                        p = p.checked_add(l)?;
                    }
                    let kind = *body.get(p)?;
                    p += 1;
                    match kind {
                        0 => {
                            leb_u64(body, &mut p)?;
                        }
                        1 => {
                            p += 1;
                            let flags = *body.get(p)?;
                            p += 1;
                            leb_u64(body, &mut p)?;
                            if flags & 1 == 1 {
                                leb_u64(body, &mut p)?;
                            }
                        }
                        2 => {
                            let flags = *body.get(p)?;
                            p += 1;
                            leb_u64(body, &mut p)?;
                            if flags & 1 == 1 {
                                leb_u64(body, &mut p)?;
                            }
                            m.imported_memories += 1;
                        }
                        3 => {
                            p += 2;
                            m.imported_globals += 1;
                        }
                        4 => {
                            p += 1;
                            leb_u64(body, &mut p)?;
                        }
                        _ => return None,
                    }
                }
            }
            5 => {
                let n = leb_u64(body, &mut p)?;
                for _ in 0..n {
                    let flags = *body.get(p)?;
                    p += 1;
                    let initial = leb_u64(body, &mut p)?;
                    let maximum = if flags & 1 == 1 { Some(leb_u64(body, &mut p)?) } else { None };
                    m.memories.push((flags, initial, maximum));
                }
            }
            6 => {
                let n = leb_u64(body, &mut p)?;
                for _ in 0..n {
                    p += 2; // value type, mutability
                    m.globals.push(const_expr(body, &mut p)?);
                }
            }
            7 => {
                let n = leb_u64(body, &mut p)?;
                for _ in 0..n {
                    let l = usize::try_from(leb_u64(body, &mut p)?).ok()?;
                    let name = std::str::from_utf8(body.get(p..p.checked_add(l)?)?).ok()?;
                    m.exports.push(name.to_owned());
                    p += l;
                    let kind = *body.get(p)?;
                    p += 1;
                    let index = leb_u64(body, &mut p)?;
                    m.export_refs.push((name.to_owned(), kind, index));
                }
            }
            11 => {
                let n = leb_u64(body, &mut p)?;
                for _ in 0..n {
                    let flags = leb_u64(body, &mut p)?;
                    let offset = match flags {
                        0 => const_expr(body, &mut p)?,
                        1 => None,
                        2 => {
                            leb_u64(body, &mut p)?;
                            const_expr(body, &mut p)?
                        }
                        _ => return None,
                    };
                    let l = leb_u64(body, &mut p)?;
                    p = p.checked_add(usize::try_from(l).ok()?)?;
                    if let Some(o) = offset {
                        m.data_end = m.data_end.max(o.checked_add(l)?);
                    }
                }
            }
            _ => {}
        }
        at = end;
    }
    Some(m)
}

fn main() -> ExitCode {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: arena_wasm_check <module.wasm>");
        return ExitCode::from(2);
    };
    let Ok(bytes) = std::fs::read(&path) else {
        eprintln!("cannot read {path}");
        return ExitCode::from(2);
    };
    let Some(m) = parse(&bytes) else {
        eprintln!("{path}: not a well-formed wasm module");
        return ExitCode::from(2);
    };
    let want = slop_ninja_arena::model::LINEAR_MEMORY_PAGES as u64;
    let fixed = env!("SLOP_NINJA_LINEAR_MEMORY_FIXED") == "true";
    let mut failed = Vec::new();
    match m.memories.as_slice() {
        [(_, initial, Some(maximum))]
            if *maximum == want && (if fixed { *initial == want } else { *initial <= want }) && m.imported_memories == 0 => {}
        other => failed.push(format!(
            "memory {other:?} (imported {}), want one defined memory with maximum = {want} pages and initial {} it",
            m.imported_memories,
            if fixed { "equal to" } else { "at most" }
        )),
    }
    for name in ["fetch", "scheduled", "queue", "chainjournal_new", "chainjournal_fetch", "evalcohort_new", "evalcohort_fetch", "evalcohort_alarm"] {
        // The raw cdylib's wasm-bindgen exports carry a hash suffix
        // (`fetch_<hex>`); worker-build's processed module has bare names.
        let matches = |e: &str| {
            e == name || e.strip_prefix(name).and_then(|r| r.strip_prefix('_')).is_some_and(|h| !h.is_empty() && h.bytes().all(|b| b.is_ascii_hexdigit()))
        };
        if !m.exports.iter().any(|e| matches(e)) {
            failed.push(format!("missing export {name}"));
        }
    }
    // Where the pool starts: `__heap_base` when the module exports it (it
    // includes zero-initialised statics), else the data segments' end.
    let heap_base = m
        .export_refs
        .iter()
        .find(|(n, kind, _)| n == "__heap_base" && *kind == 3)
        .and_then(|(_, _, i)| i.checked_sub(m.imported_globals))
        .and_then(|i| m.globals.get(usize::try_from(i).ok()?).copied().flatten());
    let static_end = heap_base.unwrap_or(m.data_end);
    let upper = slop_ninja_coordinator::arena_routes::STATIC_UPPER_BYTES as u64;
    if static_end > upper {
        failed.push(format!("static data and stack end at {static_end} B, above the {upper} B the route table's fit check assumes"));
    }
    println!(
        "{{\"module\":{path:?},\"bytes\":{},\"memory_pages\":{want},\"fixed\":{fixed},\"initial_pages\":{},\"linear_memory_bytes\":{},\"model\":{:?},\"exports\":{},\"static_end_bytes\":{static_end},\"static_end_source\":{:?},\"static_upper_bytes\":{upper},\"failed\":{}}}",
        bytes.len(),
        m.memories.first().map_or(0, |(_, initial, _)| *initial),
        slop_ninja_arena::model::LINEAR_MEMORY_BYTES,
        slop_ninja_arena::model::VERSION,
        m.exports.len(),
        if heap_base.is_some() { "__heap_base" } else { "data_segments" },
        failed.len()
    );
    for f in &failed {
        eprintln!("FAIL {f}");
    }
    if failed.is_empty() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}
