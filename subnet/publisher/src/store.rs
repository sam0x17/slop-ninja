//! The object-store seam: R2 buckets in the Worker (`cf.rs`), an in-memory
//! store in native tests ([`MemStore`]).
//!
//! Reads are bounded before the body is read: [`Store::get`] refuses an object
//! whose stored size exceeds the caller's bound, so no buffer is ever sized
//! from an object the publisher has not admitted (see [`crate::arena`]).
//! Copies between buckets stream inside the runtime and never pass through
//! Wasm memory; the destination store checks the received bytes against the
//! SHA-256 and length the publisher verified, so a source that changed after
//! verification is refused instead of published.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

use slop_ninja_protocol::{Digest, object_sha256};

use crate::layout::Cache;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectMeta {
    pub size: u64,
    /// The SHA-256 the store recorded at upload, when one was supplied.
    pub sha256: Option<Digest>,
}

/// Content-free store failures: a code and the key involved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreError {
    /// The store could not be reached or answered with an error.
    Unavailable(&'static str),
    /// The stored size exceeds the caller's bound; nothing was read.
    TooLarge { size: u64, bound: u64 },
    /// The body's length differs from the stored size.
    Short,
    /// A copy's received bytes did not match the verified digest or length.
    Integrity,
}

impl core::fmt::Display for StoreError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Unavailable(code) => write!(f, "store unavailable ({code})"),
            Self::TooLarge { size, bound } => {
                write!(f, "object of {size} bytes exceeds its bound of {bound}")
            }
            Self::Short => f.write_str("object body shorter or longer than its stored size"),
            Self::Integrity => f.write_str("copied bytes differ from the verified digest"),
        }
    }
}

/// Metadata of a write.
#[derive(Clone, Copy, Debug)]
pub struct Put<'a> {
    pub content_type: &'a str,
    pub cache: Cache,
    /// Checked by the store against the received bytes when present.
    pub sha256: Option<&'a Digest>,
}

#[allow(async_fn_in_trait)]
pub trait Store {
    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, StoreError>;

    /// The object's bytes, when it exists and its stored size is at most
    /// `bound`. A larger object is refused before its body is read.
    async fn get(&self, key: &str, bound: u64) -> Result<Option<Vec<u8>>, StoreError>;

    async fn put(&self, key: &str, bytes: Vec<u8>, put: &Put<'_>) -> Result<(), StoreError>;

    /// Streams `src_key` of `src` into `dst_key` of this store. The store
    /// refuses the write unless the bytes received have exactly `len` bytes
    /// and SHA-256 `sha256`.
    async fn copy_from(
        &self,
        src: &Self,
        src_key: &str,
        dst_key: &str,
        len: u64,
        put: &Put<'_>,
    ) -> Result<bool, StoreError>;

    /// The immediate sub-directories of `prefix` (delimiter `/`), each with
    /// its trailing `/`.
    async fn list_dirs(&self, prefix: &str) -> Result<Vec<String>, StoreError>;

    /// Deletes `key`; deleting a missing key succeeds.
    async fn delete(&self, key: &str) -> Result<(), StoreError>;
}

// ------------------------------------------------------------------ memory

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub bytes: Vec<u8>,
    pub content_type: String,
    pub cache: Option<Cache>,
    pub sha256: Option<Digest>,
}

/// An in-memory store for native tests, with read counters and fault hooks.
#[derive(Default)]
pub struct MemStore {
    pub objects: RefCell<BTreeMap<String, Stored>>,
    /// Keys whose reads fail as unavailable.
    pub unavailable: RefCell<BTreeSet<String>>,
    /// Source keys whose bytes change between verification and copy (the
    /// copy delivers them with one byte flipped).
    pub tamper_on_copy: RefCell<BTreeSet<String>>,
    /// Bytes read through `get`, and the largest single read.
    pub bytes_read: Cell<u64>,
    pub largest_read: Cell<u64>,
    pub puts: Cell<u64>,
    pub deletes: Cell<u64>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&self, key: &str, bytes: Vec<u8>) {
        self.objects.borrow_mut().insert(
            key.to_owned(),
            Stored {
                bytes,
                content_type: "application/octet-stream".into(),
                cache: None,
                sha256: None,
            },
        );
    }

    pub fn bytes(&self, key: &str) -> Option<Vec<u8>> {
        self.objects.borrow().get(key).map(|s| s.bytes.clone())
    }

    pub fn keys(&self) -> Vec<String> {
        self.objects.borrow().keys().cloned().collect()
    }

    pub fn remove(&self, key: &str) {
        self.objects.borrow_mut().remove(key);
    }
}

impl Store for MemStore {
    async fn head(&self, key: &str) -> Result<Option<ObjectMeta>, StoreError> {
        if self.unavailable.borrow().contains(key) {
            return Err(StoreError::Unavailable("mem head"));
        }
        Ok(self.objects.borrow().get(key).map(|s| ObjectMeta {
            size: s.bytes.len() as u64,
            sha256: s.sha256,
        }))
    }

    async fn get(&self, key: &str, bound: u64) -> Result<Option<Vec<u8>>, StoreError> {
        if self.unavailable.borrow().contains(key) {
            return Err(StoreError::Unavailable("mem get"));
        }
        let objects = self.objects.borrow();
        let Some(s) = objects.get(key) else {
            return Ok(None);
        };
        let size = s.bytes.len() as u64;
        if size > bound {
            return Err(StoreError::TooLarge { size, bound });
        }
        self.bytes_read.set(self.bytes_read.get() + size);
        self.largest_read.set(self.largest_read.get().max(size));
        Ok(Some(s.bytes.clone()))
    }

    async fn put(&self, key: &str, bytes: Vec<u8>, put: &Put<'_>) -> Result<(), StoreError> {
        if let Some(want) = put.sha256
            && object_sha256(&bytes) != *want
        {
            return Err(StoreError::Integrity);
        }
        self.puts.set(self.puts.get() + 1);
        self.objects.borrow_mut().insert(
            key.to_owned(),
            Stored {
                bytes,
                content_type: put.content_type.to_owned(),
                cache: Some(put.cache),
                sha256: put.sha256.copied(),
            },
        );
        Ok(())
    }

    async fn copy_from(
        &self,
        src: &Self,
        src_key: &str,
        dst_key: &str,
        len: u64,
        put: &Put<'_>,
    ) -> Result<bool, StoreError> {
        if src.unavailable.borrow().contains(src_key) {
            return Err(StoreError::Unavailable("mem copy"));
        }
        let Some(mut bytes) = src.objects.borrow().get(src_key).map(|s| s.bytes.clone()) else {
            return Ok(false);
        };
        if src.tamper_on_copy.borrow().contains(src_key)
            && let Some(b) = bytes.last_mut()
        {
            *b ^= 1;
        }
        if bytes.len() as u64 != len {
            return Err(StoreError::Integrity);
        }
        self.put(dst_key, bytes, put).await?;
        Ok(true)
    }

    async fn list_dirs(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        let mut dirs = BTreeSet::new();
        for key in self.objects.borrow().keys() {
            if let Some(rest) = key.strip_prefix(prefix)
                && let Some((dir, _)) = rest.split_once('/')
            {
                dirs.insert(format!("{prefix}{dir}/"));
            }
        }
        Ok(dirs.into_iter().collect())
    }

    async fn delete(&self, key: &str) -> Result<(), StoreError> {
        self.deletes.set(self.deletes.get() + 1);
        self.objects.borrow_mut().remove(key);
        Ok(())
    }
}

/// Runs a future that never waits on outside events (every [`MemStore`]
/// future is ready when polled) to completion on the current thread.
pub fn block_on<F: Future>(future: F) -> F::Output {
    fn noop(_: *const ()) {}
    fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(core::ptr::null(), &VTABLE)
    }
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
    // SAFETY: every vtable function ignores the data pointer.
    let waker = unsafe { Waker::from_raw(RawWaker::new(core::ptr::null(), &VTABLE)) };
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        if let Poll::Ready(value) = future.as_mut().poll(&mut cx) {
            return value;
        }
    }
}
