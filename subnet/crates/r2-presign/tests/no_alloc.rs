//! Signing and checking a URL allocate nothing (the crate is `no_std`; this
//! is the allocation witness for code shared with the Workers' arenas).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;
thread_local! { static COUNT: Cell<usize> = const { Cell::new(0) }; }
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let _ = COUNT.try_with(|c| c.set(c.get() + 1));
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
}
#[global_allocator]
static A: Counting = Counting;

#[test]
fn presign_and_check_allocate_nothing() {
    use slop_ninja_r2_presign::{check, layout, policy, presign, wire};
    let key = layout::ObjectId { cohort: 1, commitment: [3; 32], kind: layout::ObjectKind::File([4; 32]) }.key();
    let before = COUNT.with(Cell::get);
    let v = policy::valid_secs(954_576_228).unwrap_or(0);
    let p = presign::presign_put(
        &presign::Credentials { access_key_id: "CFEXAMPLEKEY12345", secret_access_key: b"s3cr3t" },
        &presign::Target { host: "a.r2.cloudflarestorage.com", bucket: "slop-ninja-admission-testnet", key: key.as_str() },
        &presign::Binding { len: 10, sha256: [4; 32] },
        1_764_612_312,
        v,
    );
    let ok = p.as_ref().map(|p| check::check_put_url(p.url.as_str(), None, None, key.as_str()).is_ok());
    let parsed = layout::ObjectId::parse(key.as_str()).is_some();
    let mut req = [0u8; wire::request_len(1)];
    let e = wire::Entry { op: wire::Op::File, cohort: 1, commitment: [3; 32], sha256: [4; 32], len: 10, valid_secs: v };
    let n = wire::encode_request(&[e], &mut req);
    let decoded = wire::decode_request(&req, 7).map(|mut it| it.next().flatten() == Some(e));
    let after = COUNT.with(Cell::get);
    assert_eq!(after - before, 0, "allocations during signing");
    assert_eq!(ok, Ok(true));
    assert!(parsed && n.is_some() && decoded == Some(true));
}
