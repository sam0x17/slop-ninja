//! The registration read: the subnet's registered hotkeys at the finalized
//! head, read-only, with every storage key composed **by name**
//! (`SubtensorModule.SubnetworkN(netuid)` and `SubtensorModule.Keys(netuid,
//! uid)`) from live V15 metadata, as the coordinator composes every chain
//! read. Nothing here bakes a pallet index, a hasher or a runtime version.
//!
//! The JSON-RPC encoding and parsing is storage-neutral (natively tested);
//! the Worker (`cf`) makes the calls.
//!
//! **Roster.** An isolate keeps the subnet's whole hotkey set at one
//! finalized head ([`roster`]): `SubnetworkN` and then `Keys(netuid, uid)`
//! for every uid below it, read with `state_queryStorageAt` at that head in
//! chunks whose replies fit the small-reply bound ([`keys_per_call`]). It
//! is read once a block per isolate (the head serves [`Head::REUSE_MS`]),
//! by one request at a time; requests that arrive while it loads wait for
//! that load, and after a failed load the isolate answers `unavailable`
//! for one block without reading again. A registration check is then a
//! lookup in memory, so a request signed by an unregistered key costs no
//! I/O of its own: the RPC load is per block and isolate, never per
//! request. The roster lives in a fixed static array of every UID a `u16`
//! can name ([`roster::CAPACITY`]: `SubnetworkN` and the uid are `u16` in
//! the runtime, checked by encoding when the layout is derived), so it
//! never holds arena pages.
//!
//! **Layout cache.** Parsing metadata costs one large read and a parse, so
//! the Worker keeps, per isolate, only the derived key layout ([`Layout`]):
//! the `SubnetworkN(netuid)` key, the `Keys` prefix with the netuid
//! component already hashed, the hasher of the uid component, and the
//! digest of the runtime upgrade marker it was derived under, in a
//! fixed-size `Copy` value. The marker is the raw `System.LastRuntimeUpgrade`
//! value, as the coordinator's commitment reader uses it: its key is
//! `twox128("System") ++ twox128("LastRuntimeUpgrade")` in every runtime, so
//! it is read before any metadata is known. Each roster load reads the
//! marker with `SubnetworkN`; a marker other than the layout's means another
//! runtime wrote the state, so the layout is derived again at that head and
//! the read repeated once. A runtime upgrade therefore never makes a
//! registered hotkey look unregistered, and no version is ever compared.

use rtao_core::codec::Value;
use rtao_core::codec::storage::hash_param;
use rtao_core::runtime::Runtime;
use serde_json::{Value as Json, json};

pub const SUBTENSOR: &str = "SubtensorModule";
pub const SUBNETWORK_N: &str = "SubnetworkN";
pub const KEYS: &str = "Keys";

/// A small JSON-RPC reply's bound (the coordinator's
/// `rpc::MAX_RESPONSE_BYTES`): a block hash, a storage value, a runtime
/// version.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// The metadata reply's bound (the coordinator's
/// `rpc::MAX_LARGE_RESPONSE_BYTES`, the bound its reader and signing journal
/// read the same reply with).
pub const MAX_LARGE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Per-call timeouts, inherited from the coordinator's RPC client
/// (`rpc::CALL_TIMEOUT_MS`, `rpc::METADATA_TIMEOUT_MS`), not measured on
/// this Worker.
pub const CALL_TIMEOUT_MS: u64 = 8_000;
pub const METADATA_TIMEOUT_MS: u64 = 20_000;

/// Why a read failed (never a refusal of the request: the caller answers
/// `unavailable`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadError(pub &'static str);

/// The storage hashers metadata can name, as a `Copy` code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hasher {
    Blake2_128,
    Blake2_256,
    Blake2_128Concat,
    Twox128,
    Twox256,
    Twox64Concat,
    Identity,
}

impl Hasher {
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "Blake2_128" => Self::Blake2_128,
            "Blake2_256" => Self::Blake2_256,
            "Blake2_128Concat" => Self::Blake2_128Concat,
            "Twox128" => Self::Twox128,
            "Twox256" => Self::Twox256,
            "Twox64Concat" => Self::Twox64Concat,
            "Identity" => Self::Identity,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Blake2_128 => "Blake2_128",
            Self::Blake2_256 => "Blake2_256",
            Self::Blake2_128Concat => "Blake2_128Concat",
            Self::Twox128 => "Twox128",
            Self::Twox256 => "Twox256",
            Self::Twox64Concat => "Twox64Concat",
            Self::Identity => "Identity",
        }
    }
}

/// Room for a storage key prefix: the item prefix (32 bytes) and a hashed
/// netuid component (the largest hasher output, 32, plus the encoded netuid
/// itself, which a concat hasher appends), whatever its encoded length up to
/// the rest.
pub const PREFIX_CAPACITY: usize = 128;

/// Encodes `v` by the metadata type `ty` and checks the encoding is
/// `want` (the layout composes keys and decodes values without the runtime
/// afterwards, so their encodings are checked once here).
fn encodes_as(rt: &Runtime, ty: u32, v: &Value, want: &[u8], what: &'static str) -> Result<(), ReadError> {
    let mut enc = Vec::new();
    rt.encode_id(ty, v, &mut enc).map_err(|_| ReadError(what))?;
    if enc != want {
        return Err(ReadError(what));
    }
    Ok(())
}

/// A fixed-size key prefix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Prefix {
    bytes: [u8; PREFIX_CAPACITY],
    len: usize,
}

impl Prefix {
    fn of(v: &[u8]) -> Result<Self, ReadError> {
        let mut bytes = [0u8; PREFIX_CAPACITY];
        bytes.get_mut(..v.len()).ok_or(ReadError("prefix"))?.copy_from_slice(v);
        Ok(Self { bytes, len: v.len() })
    }

    fn get(&self) -> Result<&[u8], ReadError> {
        self.bytes.get(..self.len).ok_or(ReadError("prefix"))
    }
}

/// The derived roster key layout of one runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// SHA-256 of the upgrade marker's raw value it was derived under.
    pub marker: [u8; 32],
    /// The full `SubnetworkN(netuid)` key.
    n_key: Prefix,
    /// `Keys`' item prefix with the hashed netuid component.
    keys: Prefix,
    /// The hasher of the uid component of `Keys`.
    uid: Hasher,
}

impl Layout {
    /// Derives the layout from parsed metadata. `SubnetworkN` must be a map
    /// of the netuid whose value encodes as a little-endian `u16`; `Keys` a
    /// two-key map of the netuid and a uid that encodes as a little-endian
    /// `u16`, whose value encodes an account as its raw 32 bytes (each
    /// checked by encoding a probe by its metadata type), else the read
    /// cannot be composed and the layout is refused.
    pub fn derive(rt: &Runtime, netuid: u16, marker: [u8; 32]) -> Result<Self, ReadError> {
        let probe_u16 = Value::Int(0x0102);
        let n = rt.storage_entry(SUBTENSOR, SUBNETWORK_N).ok_or(ReadError("n_absent"))?;
        let (Some(&n_ty), Some(n_h), 1, 1) = (n.key_types.first(), n.hashers.first(), n.key_types.len(), n.hashers.len()) else {
            return Err(ReadError("n_shape"));
        };
        encodes_as(rt, n.value_type, &probe_u16, &[0x02, 0x01], "n_value")?;
        let mut net = Vec::new();
        rt.encode_id(n_ty, &Value::Int(i128::from(netuid)), &mut net).map_err(|_| ReadError("n_netuid"))?;
        let mut n_key = rtao_core::codec::storage::storage_prefix(n);
        n_key.extend_from_slice(&hash_param(n_h, &net).map_err(|_| ReadError("n_hasher"))?);

        let k = rt.storage_entry(SUBTENSOR, KEYS).ok_or(ReadError("keys_absent"))?;
        let (Some(&net_ty), Some(&uid_ty), Some(h0), Some(h1), 2, 2) =
            (k.key_types.first(), k.key_types.get(1), k.hashers.first(), k.hashers.get(1), k.key_types.len(), k.hashers.len())
        else {
            return Err(ReadError("keys_shape"));
        };
        let uid = Hasher::parse(h1).ok_or(ReadError("keys_hasher"))?;
        encodes_as(rt, uid_ty, &probe_u16, &[0x02, 0x01], "keys_uid")?;
        let probe = [0x5au8; 32];
        encodes_as(rt, k.value_type, &Value::Bytes(probe.to_vec()), &probe, "keys_value")?;
        let mut net = Vec::new();
        rt.encode_id(net_ty, &Value::Int(i128::from(netuid)), &mut net).map_err(|_| ReadError("keys_netuid"))?;
        let mut keys = rtao_core::codec::storage::storage_prefix(k);
        keys.extend_from_slice(&hash_param(h0, &net).map_err(|_| ReadError("keys_hasher"))?);
        Ok(Self {
            marker,
            n_key: Prefix::of(&n_key)?,
            keys: Prefix::of(&keys)?,
            uid,
        })
    }

    /// The `SubnetworkN(netuid)` key, as `0x` hex.
    pub fn n_key_hex(&self) -> Result<String, ReadError> {
        Ok(format!("0x{}", hex::encode(self.n_key.get()?)))
    }

    /// The full storage key of `Keys(netuid, uid)`, as `0x` hex.
    pub fn key_hex(&self, uid: u16) -> Result<String, ReadError> {
        let mut key = self.keys.get()?.to_vec();
        key.extend_from_slice(&hash_param(self.uid.name(), &uid.to_le_bytes()).map_err(|_| ReadError("keys_hasher"))?);
        Ok(format!("0x{}", hex::encode(key)))
    }

    /// The length of a `Keys` key in bytes (its hex is twice that).
    pub fn key_len(&self) -> Result<usize, ReadError> {
        Ok((self.key_hex(0)?.len() - 2) / 2)
    }
}

/// `SubnetworkN`'s value (a little-endian `u16`, checked by
/// [`Layout::derive`]); absent is the runtime's default, 0.
pub fn parse_n(raw: Option<&str>) -> Result<u16, ReadError> {
    let Some(raw) = raw else { return Ok(0) };
    let mut b = [0u8; 2];
    hex::decode_to_slice(raw.trim_start_matches("0x"), &mut b).map_err(|_| ReadError("n_value"))?;
    Ok(u16::from_le_bytes(b))
}

/// A `Keys` value: the account's raw 32 bytes (checked by
/// [`Layout::derive`]). A uid below `SubnetworkN` always has one.
pub fn parse_account(raw: Option<&str>) -> Result<[u8; 32], ReadError> {
    let raw = raw.ok_or(ReadError("keys_missing"))?;
    let mut b = [0u8; 32];
    hex::decode_to_slice(raw.trim_start_matches("0x"), &mut b).map_err(|_| ReadError("keys_value"))?;
    Ok(b)
}

/// How many `Keys` one `state_queryStorageAt` call reads so that its reply
/// stays within the small-reply bound ([`MAX_RESPONSE_BYTES`]): the reply's
/// wrapper at its largest, then per key its change entry (`["0x<key>",
/// "0x<account>"]` and the comma before it), each measured on the format
/// itself.
pub fn keys_per_call(key_len: usize) -> usize {
    let block = format!("0x{}", "f".repeat(64));
    let wrapper = json!({"jsonrpc": "2.0", "id": u64::MAX, "result": [{"block": block, "changes": []}]}).to_string().len();
    let entry = json!([format!("0x{}", "f".repeat(2 * key_len)), format!("0x{}", "f".repeat(64))]).to_string().len() + 1;
    MAX_RESPONSE_BYTES.saturating_sub(wrapper) / entry
}

/// `System.LastRuntimeUpgrade`'s key (a plain value: `twox128` of the
/// pallet and item names in every runtime), as `0x` hex.
pub fn marker_key() -> String {
    let mut k = rtao_core::codec::storage::hash_param("Twox128", b"System").unwrap_or_default();
    k.extend_from_slice(&rtao_core::codec::storage::hash_param("Twox128", b"LastRuntimeUpgrade").unwrap_or_default());
    format!("0x{}", hex::encode(k))
}

/// The digest of a marker's raw value (`None`: absent).
pub fn marker_digest(raw: Option<&str>) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    Sha256::digest(raw.unwrap_or("").to_ascii_lowercase().as_bytes()).into()
}

/// The per-isolate layout cache (single-threaded on wasm32; per thread in
/// native tests).
pub mod cache {
    use super::Layout;
    use core::cell::Cell;

    #[cfg(target_arch = "wasm32")]
    struct Slot(Cell<Option<Layout>>);
    // SAFETY: wasm32 without atomics is single-threaded.
    #[cfg(target_arch = "wasm32")]
    unsafe impl Sync for Slot {}
    #[cfg(target_arch = "wasm32")]
    static SLOT: Slot = Slot(Cell::new(None));

    #[cfg(not(target_arch = "wasm32"))]
    std::thread_local! {
        static SLOT: Cell<Option<Layout>> = const { Cell::new(None) };
    }

    pub fn get() -> Option<Layout> {
        #[cfg(target_arch = "wasm32")]
        {
            SLOT.0.get()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            SLOT.with(Cell::get)
        }
    }

    pub fn set(layout: Option<Layout>) {
        #[cfg(target_arch = "wasm32")]
        {
            SLOT.0.set(layout)
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            SLOT.with(|s| s.set(layout))
        }
    }
}

/// The finalized head one isolate holds for one block time: its hash and
/// number and when it was read (fixed-size `Copy`, no heap, as [`Layout`]).
/// The roster read at it serves every signed request in that block, so the
/// head costs one `chain_getFinalizedHead` and one `chain_getHeader` a block
/// per isolate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub hash: [u8; 32],
    pub number: u64,
    pub read_ms: u64,
}

impl Head {
    /// The nominal block time (12 s, the coordinator's
    /// `chain::BLOCK_TIME_SECS`): a head read this recently is still the
    /// head the next finalized block replaces.
    pub const REUSE_MS: u64 = crate::front::BLOCK_TIME_SECS * 1000;

    /// Whether the head read at `read_ms` still serves at `now_ms` (a clock
    /// that went back serves nothing).
    pub fn fresh(&self, now_ms: u64) -> bool {
        now_ms >= self.read_ms && now_ms - self.read_ms < Self::REUSE_MS
    }

    pub fn hash_hex(&self) -> String {
        format!("0x{}", hex::encode(self.hash))
    }
}

/// A per-isolate slot of a `Copy` value (single-threaded on wasm32; per
/// thread in native tests).
macro_rules! slot {
    ($name:ident, $ty:ty) => {
        pub mod $name {
            use core::cell::Cell;

            #[cfg(target_arch = "wasm32")]
            struct Slot(Cell<Option<$ty>>);
            // SAFETY: wasm32 without atomics is single-threaded.
            #[cfg(target_arch = "wasm32")]
            unsafe impl Sync for Slot {}
            #[cfg(target_arch = "wasm32")]
            static SLOT: Slot = Slot(Cell::new(None));

            #[cfg(not(target_arch = "wasm32"))]
            std::thread_local! {
                static SLOT: Cell<Option<$ty>> = const { Cell::new(None) };
            }

            pub fn get() -> Option<$ty> {
                #[cfg(target_arch = "wasm32")]
                {
                    SLOT.0.get()
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    SLOT.with(Cell::get)
                }
            }

            pub fn set(value: Option<$ty>) {
                #[cfg(target_arch = "wasm32")]
                {
                    SLOT.0.set(value)
                }
                #[cfg(not(target_arch = "wasm32"))]
                {
                    SLOT.with(|s| s.set(value))
                }
            }
        }
    };
}

slot!(load_state, super::LoadState);

/// The roster load's state in an isolate: whether one load is in flight
/// (later requests wait for it), and when the last load failed (the isolate
/// then answers `unavailable` for one block without reading again).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LoadState {
    pub loading: bool,
    pub failed_ms: Option<u64>,
}

impl LoadState {
    /// Whether a load that failed at `failed_ms` still holds at `now_ms`.
    pub fn backing_off(&self, now_ms: u64) -> bool {
        self.failed_ms.is_some_and(|f| now_ms >= f && now_ms - f < Head::REUSE_MS)
    }
}

/// The subnet's registered hotkeys at one finalized head, in a fixed
/// static array (no heap; module docs). Single-threaded on wasm32; per
/// thread in native tests.
pub mod roster {
    use super::Head;

    /// Every UID a `u16` can name.
    pub const CAPACITY: usize = 1 << 16;

    struct State {
        /// The head of a complete roster (`None` while loading or after a
        /// failed load).
        head: Option<Head>,
        n: usize,
    }

    #[cfg(target_arch = "wasm32")]
    mod store {
        use super::{CAPACITY, State};
        use core::cell::UnsafeCell;
        struct Cells {
            state: UnsafeCell<State>,
            keys: UnsafeCell<[[u8; 32]; CAPACITY]>,
        }
        // SAFETY: wasm32 without atomics is single-threaded, and `with`
        // never reenters.
        unsafe impl Sync for Cells {}
        static CELLS: Cells = Cells {
            state: UnsafeCell::new(State { head: None, n: 0 }),
            keys: UnsafeCell::new([[0; 32]; CAPACITY]),
        };
        pub(super) fn with<R>(f: impl FnOnce(&mut State, &mut [[u8; 32]]) -> R) -> R {
            // SAFETY: single-threaded; `f` is synchronous and never calls
            // `with` again, so no second reference exists.
            unsafe { f(&mut *CELLS.state.get(), &mut *CELLS.keys.get()) }
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    mod store {
        use super::{CAPACITY, State};
        use core::cell::RefCell;
        std::thread_local! {
            static CELLS: RefCell<(State, Vec<[u8; 32]>)> = RefCell::new((State { head: None, n: 0 }, vec![[0; 32]; CAPACITY]));
        }
        pub(super) fn with<R>(f: impl FnOnce(&mut State, &mut [[u8; 32]]) -> R) -> R {
            CELLS.with(|c| {
                let mut c = c.borrow_mut();
                let (s, k) = &mut *c;
                f(s, k)
            })
        }
    }

    /// Whether `who` is registered, when a complete roster read within the
    /// last block time serves at `now_ms`.
    pub fn lookup(now_ms: u64, who: &[u8; 32]) -> Option<bool> {
        store::with(|s, keys| {
            let h = s.head?;
            if !h.fresh(now_ms) {
                return None;
            }
            Some(keys.get(..s.n).is_some_and(|k| k.binary_search(who).is_ok()))
        })
    }

    /// The head of the complete roster, fresh or not.
    pub fn head() -> Option<Head> {
        store::with(|s, _| s.head)
    }

    /// The finalized head is still the roster's: it serves another block
    /// time from `read_ms`.
    pub fn renew(read_ms: u64) {
        store::with(|s, _| {
            if let Some(h) = s.head.as_mut() {
                h.read_ms = read_ms;
            }
        })
    }

    /// Starts a load: no roster serves until [`finish`].
    pub fn begin() {
        store::with(|s, _| {
            s.head = None;
            s.n = 0;
        })
    }

    /// Stores uid `at`'s hotkey. `false` past the capacity.
    pub fn put(at: usize, key: [u8; 32]) -> bool {
        store::with(|_, keys| keys.get_mut(at).map(|k| *k = key).is_some())
    }

    /// Completes a load of `n` hotkeys at `head` (sorted in place for the
    /// lookups). `false` past the capacity.
    pub fn finish(head: Head, n: usize) -> bool {
        store::with(|s, keys| {
            let Some(k) = keys.get_mut(..n) else { return false };
            k.sort_unstable();
            s.head = Some(head);
            s.n = n;
            true
        })
    }
}

// ------------------------------------------------------------- JSON-RPC

pub fn request(id: u64, method: &str, params: Json) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

pub fn finalized_head_request(id: u64) -> String {
    request(id, "chain_getFinalizedHead", json!([]))
}

/// `chain_getHeader(hash)`: the head's number.
pub fn header_request(id: u64, hash: &str) -> String {
    request(id, "chain_getHeader", json!([hash]))
}

pub fn storage_request(id: u64, key_hex: &str, at: &str) -> String {
    request(id, "state_getStorage", json!([key_hex, at]))
}

/// `state_queryStorageAt(keys, at)`: every key read at one block.
pub fn query_storage_request(id: u64, keys: &[&str], at: &str) -> String {
    request(id, "state_queryStorageAt", json!([keys, at]))
}

pub fn runtime_version_request(id: u64, at: &str) -> String {
    request(id, "state_getRuntimeVersion", json!([at]))
}

/// `Metadata_metadata_at_version(15)` at `at`.
pub fn metadata_request(id: u64, at: &str) -> String {
    request(id, "state_call", json!(["Metadata_metadata_at_version", "0x0f000000", at]))
}

fn result(id: u64, body: &[u8]) -> Result<Json, ReadError> {
    let v: Json = serde_json::from_slice(body).map_err(|_| ReadError("rpc_json"))?;
    if v.get("id").and_then(Json::as_u64) != Some(id) {
        return Err(ReadError("rpc_id"));
    }
    if v.get("error").is_some_and(|e| !e.is_null()) {
        return Err(ReadError("rpc_error"));
    }
    v.get("result").cloned().ok_or(ReadError("rpc_result"))
}

fn is_hash(t: &str) -> bool {
    t.len() == 66 && t.starts_with("0x") && t.bytes().skip(2).all(|b| b.is_ascii_hexdigit())
}

/// The finalized head's hash.
pub fn parse_finalized_head(id: u64, body: &[u8]) -> Result<String, ReadError> {
    let r = result(id, body)?;
    let h = r.as_str().ok_or(ReadError("rpc_head"))?;
    if !is_hash(h) {
        return Err(ReadError("rpc_head"));
    }
    Ok(h.to_ascii_lowercase())
}

/// A header's block number (`0x` hex, as Substrate encodes it).
pub fn parse_header_number(id: u64, body: &[u8]) -> Result<u64, ReadError> {
    let r = result(id, body)?;
    let n = r.get("number").and_then(Json::as_str).and_then(|t| t.strip_prefix("0x")).ok_or(ReadError("rpc_header"))?;
    if n.is_empty() || n.len() > 16 {
        return Err(ReadError("rpc_header"));
    }
    u64::from_str_radix(n, 16).map_err(|_| ReadError("rpc_header"))
}

/// A head hash as bytes (`parse_finalized_head` checked its shape).
pub fn hash_bytes(hex_hash: &str) -> Result<[u8; 32], ReadError> {
    let mut out = [0u8; 32];
    hex::decode_to_slice(hex_hash.trim_start_matches("0x"), &mut out).map_err(|_| ReadError("rpc_head"))?;
    Ok(out)
}

/// Whether a storage value is present (`null` when absent).
pub fn parse_storage_present(id: u64, body: &[u8]) -> Result<bool, ReadError> {
    match result(id, body)? {
        Json::Null => Ok(false),
        Json::String(s) if s.starts_with("0x") && s.len() > 2 && s.bytes().skip(2).all(|b| b.is_ascii_hexdigit()) => {
            Ok(true)
        }
        _ => Err(ReadError("rpc_storage")),
    }
}

fn hex_value(v: &Json) -> Result<String, ReadError> {
    match v.as_str() {
        Some(t) if t.starts_with("0x") && t.bytes().skip(2).all(|b| b.is_ascii_hexdigit()) => Ok(t.to_ascii_lowercase()),
        _ => Err(ReadError("rpc_storage")),
    }
}

/// The values of `keys` in a `state_queryStorageAt` reply: one change set
/// for exactly block `at`, naming only requested keys (`None` for a key the
/// reply lists as absent or does not list).
pub fn parse_query_storage(id: u64, body: &[u8], keys: &[&str], at: &str) -> Result<Vec<Option<String>>, ReadError> {
    let v = result(id, body)?;
    let sets = v.as_array().ok_or(ReadError("rpc_storage"))?;
    let mut out = vec![None; keys.len()];
    for set in sets {
        if set.get("block").and_then(Json::as_str).map(str::to_ascii_lowercase).as_deref() != Some(at) {
            return Err(ReadError("rpc_storage_block"));
        }
        for change in set.get("changes").and_then(Json::as_array).ok_or(ReadError("rpc_storage"))? {
            let Some([key, value]) = change.as_array().map(Vec::as_slice) else {
                return Err(ReadError("rpc_storage"));
            };
            let key = hex_value(key)?;
            let at_key = keys.iter().position(|k| k.eq_ignore_ascii_case(&key)).ok_or(ReadError("rpc_storage_key"))?;
            let value = match value {
                Json::Null => None,
                other => Some(hex_value(other)?),
            };
            if let Some(slot) = out.get_mut(at_key) {
                *slot = value;
            }
        }
    }
    Ok(out)
}

/// `(specVersion, transactionVersion)`.
pub fn parse_runtime_version(id: u64, body: &[u8]) -> Result<(u32, u32), ReadError> {
    let r = result(id, body)?;
    let n = |k: &str| r.get(k).and_then(Json::as_u64).and_then(|v| u32::try_from(v).ok());
    Ok((n("specVersion").ok_or(ReadError("rpc_version"))?, n("transactionVersion").ok_or(ReadError("rpc_version"))?))
}

/// The prefixed V15 metadata from a `state_call` reply (the SCALE
/// `Option<OpaqueMetadata>` unwrapped).
pub fn parse_metadata(id: u64, body: &[u8]) -> Result<Vec<u8>, ReadError> {
    let r = result(id, body)?;
    let h = r.as_str().and_then(|s| s.strip_prefix("0x")).ok_or(ReadError("rpc_metadata"))?;
    let scale = hex::decode(h).map_err(|_| ReadError("rpc_metadata"))?;
    opaque_metadata(&scale)
}

/// Decodes `Option<OpaqueMetadata>` into the prefixed metadata blob.
pub fn opaque_metadata(scale: &[u8]) -> Result<Vec<u8>, ReadError> {
    use rtao_core::codec::decode::{Cursor, compact_len};
    let mut c = Cursor::new(scale);
    match c.byte().map_err(|_| ReadError("metadata_malformed"))? {
        1 => {}
        0 => return Err(ReadError("metadata_unavailable")),
        _ => return Err(ReadError("metadata_malformed")),
    }
    let len = compact_len(&mut c).map_err(|_| ReadError("metadata_malformed"))?;
    let body = c.take(len).map_err(|_| ReadError("metadata_malformed"))?;
    if c.remaining() != 0 {
        return Err(ReadError("metadata_malformed"));
    }
    Ok(body.to_vec())
}

/// Parses prefixed V15 metadata (rtao composes only from V15).
pub fn parse_runtime(prefixed: &[u8], spec: u32, tx: u32) -> Result<Runtime, ReadError> {
    let rt = Runtime::parse(prefixed, spec, tx, crate::gate::SS58_FORMAT).map_err(|_| ReadError("metadata_unparsable"))?;
    if !rt.is_v15 {
        return Err(ReadError("metadata_degraded"));
    }
    Ok(rt)
}

#[cfg(test)]
mod tests {
    use super::*;

    const METADATA: &[u8] = include_bytes!("../../fixtures/runtime-486/testnet-486.v15.scale");

    fn runtime() -> Runtime {
        parse_runtime(METADATA, 486, 1).unwrap()
    }

    #[test]
    fn the_layout_composes_the_coordinators_roster_keys_by_name() {
        let rt = runtime();
        let layout = Layout::derive(&rt, 601, marker_digest(Some("0x01"))).unwrap();
        assert_eq!(layout.marker, marker_digest(Some("0x01")));
        // The same keys rtao composes from the full parse.
        let n = rt.storage_entry(SUBTENSOR, SUBNETWORK_N).unwrap();
        let full = rt.storage_key(n, &[Value::Int(601)]).unwrap();
        assert_eq!(layout.n_key_hex().unwrap(), format!("0x{}", hex::encode(full)));
        let k = rt.storage_entry(SUBTENSOR, KEYS).unwrap();
        for uid in [0u16, 1, 255, 0x0102, u16::MAX] {
            let full = rt.storage_key(k, &[Value::Int(601), Value::Int(i128::from(uid))]).unwrap();
            assert_eq!(layout.key_hex(uid).unwrap(), format!("0x{}", hex::encode(&full)), "{uid}");
            assert_eq!(layout.key_len().unwrap(), full.len());
        }
        // Another netuid is another key.
        let other = Layout::derive(&rt, 602, [0; 32]).unwrap();
        assert_ne!(other.key_hex(3).unwrap(), layout.key_hex(3).unwrap());
        assert_ne!(other.n_key_hex().unwrap(), layout.n_key_hex().unwrap());
        // Values: a little-endian u16 count (absent: 0) and a raw account.
        assert_eq!(parse_n(Some("0x0001")).unwrap(), 256);
        assert_eq!(parse_n(None).unwrap(), 0);
        assert!(parse_n(Some("0x01")).is_err());
        let acct = format!("0x{}", "07".repeat(32));
        assert_eq!(parse_account(Some(&acct)).unwrap(), [7; 32]);
        assert!(parse_account(None).is_err() && parse_account(Some("0x07")).is_err());
    }

    /// The runtime-487 metadata (subtensor PR #3294's build) derives the
    /// same roster layout as 486, and both parses are admitted from their own
    /// length within the route's bound at the reply limit.
    #[test]
    fn the_487_runtime_derives_the_same_layout() {
        const REAL_487: &[u8] = include_bytes!("../../fixtures/runtime-487/pr3294-f3654266.v15.scale");
        let new = parse_runtime(REAL_487, 487, 1).unwrap();
        let marker = marker_digest(Some("0x01"));
        let (a, b) = (Layout::derive(&runtime(), 601, marker).unwrap(), Layout::derive(&new, 601, marker).unwrap());
        assert_eq!(a, b);
        for uid in [0u16, 7, u16::MAX] {
            assert_eq!(a.key_hex(uid).unwrap(), b.key_hex(uid).unwrap());
        }
        for metadata in [METADATA, REAL_487] {
            let need = crate::routes::metadata_parse_bytes(metadata.len());
            assert!(need >= slop_ninja_arena::measured::RUNTIME_PARSE_MEASUREMENTS.iter().map(|m| m.1).min().unwrap());
            assert!(need <= crate::routes::metadata_parse_bytes(crate::routes::MAX_METADATA_BYTES));
        }
    }

    #[test]
    fn a_keys_call_fits_the_small_reply_bound() {
        let layout = Layout::derive(&runtime(), 601, [0; 32]).unwrap();
        let len = layout.key_len().unwrap();
        let per = keys_per_call(len);
        assert!(per > 0);
        let head = format!("0x{}", "ab".repeat(32));
        let changes: Vec<Json> =
            (0..per).map(|u| json!([layout.key_hex(u as u16).unwrap(), format!("0x{}", "ff".repeat(32))])).collect();
        let reply = json!({"jsonrpc": "2.0", "id": u64::MAX, "result": [{"block": head, "changes": changes}]}).to_string();
        assert!(reply.len() <= MAX_RESPONSE_BYTES, "{}", reply.len());
        let more: Vec<Json> =
            (0..=per).map(|u| json!([layout.key_hex(u as u16).unwrap(), format!("0x{}", "ff".repeat(32))])).collect();
        let over = json!({"jsonrpc": "2.0", "id": u64::MAX, "result": [{"block": head, "changes": more}]}).to_string();
        assert!(over.len() > MAX_RESPONSE_BYTES - 200, "the count is the bound's, not padded");
    }

    #[test]
    fn the_cache_holds_one_copy_layout() {
        let layout = Layout::derive(&runtime(), 601, [0; 32]).unwrap();
        cache::set(None);
        assert_eq!(cache::get(), None);
        cache::set(Some(layout));
        assert_eq!(cache::get(), Some(layout));
        cache::set(None);
    }

    #[test]
    fn the_roster_serves_its_head_for_one_block_time_and_only_when_complete() {
        let h = Head { hash: [0xab; 32], number: 9, read_ms: 1_000 };
        roster::begin();
        assert_eq!(roster::lookup(1_000, &[1; 32]), None, "nothing loaded");
        assert!(roster::put(0, [3; 32]) && roster::put(1, [1; 32]) && roster::put(2, [2; 32]));
        assert_eq!(roster::lookup(1_000, &[1; 32]), None, "loading");
        assert!(roster::finish(h, 3));
        assert_eq!(roster::lookup(1_000, &[1; 32]), Some(true));
        assert_eq!(roster::lookup(1_000, &[3; 32]), Some(true));
        assert_eq!(roster::lookup(1_000, &[4; 32]), Some(false));
        assert_eq!(roster::lookup(1_000 + Head::REUSE_MS, &[1; 32]), None, "a block later");
        roster::renew(1_000 + Head::REUSE_MS);
        assert_eq!(roster::lookup(1_000 + Head::REUSE_MS, &[1; 32]), Some(true), "the same head again");
        assert_eq!(roster::head().map(|h| h.hash), Some([0xab; 32]));
        assert!(roster::put(roster::CAPACITY - 1, [9; 32]) && !roster::put(roster::CAPACITY, [9; 32]));
        assert!(!roster::finish(h, roster::CAPACITY + 1));
        roster::begin();
        assert_eq!(roster::head(), None);
        // A failed load backs off for one block.
        let st = LoadState { loading: false, failed_ms: Some(5_000) };
        assert!(st.backing_off(5_000) && st.backing_off(5_000 + Head::REUSE_MS - 1));
        assert!(!st.backing_off(5_000 + Head::REUSE_MS) && !st.backing_off(4_999));
        assert!(!LoadState::default().backing_off(0));
        load_state::set(Some(st));
        assert_eq!(load_state::get(), Some(st));
        load_state::set(None);
    }

    #[test]
    fn the_head_serves_one_block_time_and_parses_strictly() {
        let h = Head { hash: [0xab; 32], number: 9, read_ms: 1_000 };
        assert!(h.fresh(1_000) && h.fresh(1_000 + Head::REUSE_MS - 1));
        assert!(!h.fresh(1_000 + Head::REUSE_MS) && !h.fresh(999));
        assert_eq!(hash_bytes(&h.hash_hex()).unwrap(), h.hash);
        let ok = json!({"jsonrpc": "2.0", "id": 2, "result": {"number": "0x7f0a1c", "parentHash": "0x00"}}).to_string();
        assert_eq!(parse_header_number(2, ok.as_bytes()).unwrap(), 0x7f0a1c);
        for bad in [json!({"number": 7}), json!({"number": "0x"}), json!({"number": "0x11112222333344445"}), json!(null)] {
            let b = json!({"jsonrpc": "2.0", "id": 2, "result": bad}).to_string();
            assert!(parse_header_number(2, b.as_bytes()).is_err(), "{b}");
        }
        assert!(header_request(2, &h.hash_hex()).contains("chain_getHeader"));
    }

    #[test]
    fn replies_parse_strictly() {
        let head = format!("0x{}", "ab".repeat(32));
        let body = json!({"jsonrpc": "2.0", "id": 3, "result": head}).to_string();
        assert_eq!(parse_finalized_head(3, body.as_bytes()).unwrap(), head);
        assert!(parse_finalized_head(4, body.as_bytes()).is_err(), "another id");
        let err = json!({"jsonrpc": "2.0", "id": 3, "error": {"code": 1}}).to_string();
        assert!(parse_finalized_head(3, err.as_bytes()).is_err());
        let none = json!({"jsonrpc": "2.0", "id": 5, "result": null}).to_string();
        assert!(!parse_storage_present(5, none.as_bytes()).unwrap());
        let some = json!({"jsonrpc": "2.0", "id": 5, "result": "0x0700"}).to_string();
        assert!(parse_storage_present(5, some.as_bytes()).unwrap());
        let bad = json!({"jsonrpc": "2.0", "id": 5, "result": 7}).to_string();
        assert!(parse_storage_present(5, bad.as_bytes()).is_err());
        let v = json!({"jsonrpc": "2.0", "id": 6, "result": {"specVersion": 486, "transactionVersion": 1}}).to_string();
        assert_eq!(parse_runtime_version(6, v.as_bytes()).unwrap(), (486, 1));
        // state_queryStorageAt: one set for exactly the head, requested keys only.
        let keys = ["0xaa", "0xbb"];
        let q = json!({"jsonrpc": "2.0", "id": 8, "result": [{"block": head, "changes": [["0xAA", "0x01"], ["0xbb", null]]}]}).to_string();
        assert_eq!(parse_query_storage(8, q.as_bytes(), &keys, &head).unwrap(), vec![Some("0x01".into()), None]);
        let other_block = json!({"jsonrpc": "2.0", "id": 8, "result": [{"block": format!("0x{}", "cd".repeat(32)), "changes": []}]}).to_string();
        assert!(parse_query_storage(8, other_block.as_bytes(), &keys, &head).is_err());
        let other_key = json!({"jsonrpc": "2.0", "id": 8, "result": [{"block": head, "changes": [["0xcc", "0x01"]]}]}).to_string();
        assert!(parse_query_storage(8, other_key.as_bytes(), &keys, &head).is_err());
        // The marker's key is the plain-value key of System.LastRuntimeUpgrade.
        assert_eq!(marker_key(), "0x26aa394eea5630e07c48ae0c9558cef7f9cce9c888469bb1a0dceaa129672ef8");
        assert_ne!(marker_digest(None), marker_digest(Some("0x00")));
        // Option<OpaqueMetadata>: Some(compact len ++ bytes).
        let mut scale = vec![1u8, 3 << 2];
        scale.extend_from_slice(b"abc");
        assert_eq!(opaque_metadata(&scale).unwrap(), b"abc");
        assert!(opaque_metadata(&[0]).is_err());
        assert!(opaque_metadata(&[1, 3 << 2, b'a']).is_err());
    }
}
