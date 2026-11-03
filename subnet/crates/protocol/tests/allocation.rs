//! Untrusted length prefixes must not reserve memory out of proportion to the
//! input. The allocator records the largest single request on this thread.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use slop_ninja_protocol::fixtures::{self, cohort, package, trusted_keys};
use slop_ninja_protocol::{
    Canonical, Decoder, Error, MAX_PREALLOC_BYTES, MAX_SEQUENCE_LEN, MinerScore, Record, Result,
    ScorePackage, Signed, sign,
};

struct Largest;

thread_local! {
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

fn note(size: usize) {
    let _ = LARGEST.try_with(|l| l.set(l.get().max(size)));
}

unsafe impl GlobalAlloc for Largest {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Largest = Largest;

/// Largest single allocation `f` makes on this thread.
fn largest_allocation<T>(f: impl FnOnce() -> T) -> (T, usize) {
    LARGEST.with(|l| l.set(0));
    let out = f();
    (out, LARGEST.with(Cell::get))
}

#[test]
fn sequence_claim_reserves_at_most_the_preallocation_cap() {
    // A maximal count over 64 KiB whose first item is invalid (score above 1).
    let mut input = MAX_SEQUENCE_LEN.to_le_bytes().to_vec();
    input.extend([0xff; 64 << 10]);
    let (decoded, largest) =
        largest_allocation(|| Vec::<MinerScore>::decode_from(&mut Decoder::new(&input)));
    assert_eq!(decoded.unwrap_err(), Error::OutOfRange("score"));
    assert!(largest <= MAX_PREALLOC_BYTES, "reserved {largest} bytes");
}

#[test]
fn megabyte_package_claim_stays_proportional_to_input() -> Result<()> {
    let package = package(&cohort()?)?;
    let input = fixtures::sequence_claim(&package, 1 << 20);
    let (decoded, largest) = largest_allocation(|| ScorePackage::from_bytes(&input));
    assert_eq!(decoded.unwrap_err(), Error::UnexpectedEnd);
    // Decoded items may grow the vector to about twice their in-memory size.
    assert!(largest <= 3 * input.len(), "allocated {largest} bytes");
    Ok(())
}

#[test]
fn open_authenticates_before_decoding() -> Result<()> {
    let package = package(&cohort()?)?;
    let signed = sign(package.clone(), &fixtures::package_key())?.to_bytes()?;
    // `str("signed") || u16 || u8` is 13 bytes; key and signature are the last 96.
    let (header, tail) = (&signed[..13], &signed[signed.len() - 96..]);
    let claim = fixtures::sequence_claim(&package, 1 << 20);
    let mut envelope = header.to_vec();
    envelope.extend((claim.len() as u32).to_le_bytes());
    envelope.extend(&claim);
    envelope.extend(tail);
    let (opened, largest) =
        largest_allocation(|| Signed::<ScorePackage>::open(&envelope, &trusted_keys()));
    assert_eq!(opened.unwrap_err(), Error::BadSignature);
    assert!(
        largest < 4096,
        "allocated {largest} bytes before authentication"
    );
    Ok(())
}
