//! JSON-RPC encoding for the Workers RPC transport.
//!
//! The heartbeat uses three read-only methods: `chain_getFinalizedHead`,
//! `chain_getHeader` and `chain_getBlockHash` (its list form). The signing
//! journal (`crate::signing`) adds reads pinned to a block hash
//! (`state_getRuntimeVersion`, the V15 metadata runtime API through
//! `state_call`, `state_queryStorageAt`, `chain_getBlock`) and the single
//! write, `author_submitExtrinsic`. The Worker sends one call per HTTP POST
//! through `fetch` (see `cf.rs`); this module builds the request bodies and
//! validates the responses so both are tested natively.

use crate::wire::{FinalizedHeader, Id, MAX_SAFE_INTEGER};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

mod block_scan;
pub use block_scan::{BlockMatch, BlockScan, scan_block};

/// Public OpenTensor testnet endpoint (testnet-only, like the genesis guard).
pub const DEFAULT_TESTNET_RPC_URL: &str = "https://test.finney.opentensor.ai";
/// The named User-Agent every RPC call sends. Public endpoints behind
/// Cloudflare's Browser Integrity Check (rtao.io answers error 1010 to a
/// missing or library-default agent such as `Python-urllib/3`) need a named
/// product and version.
pub const USER_AGENT: &str = concat!("slop-ninja-coordinator-testnet/", env!("CARGO_PKG_VERSION"));

/// The headers of every JSON-RPC POST (`cf.rs`): JSON both ways, an
/// unencoded reply (whose exact declared length `cf_io` bounds; an encoded
/// one is still read correctly, bounded by the cap), and [`USER_AGENT`].
pub const REQUEST_HEADERS: [(&str, &str); 4] = [
    ("content-type", "application/json"),
    ("accept", "application/json"),
    ("accept-encoding", "identity"),
    ("user-agent", USER_AGENT),
];
/// Responses larger than this are refused before parsing.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024;
/// Upper bound on heights asked for in one `chain_getBlockHash` list call.
pub const MAX_HASHES_PER_CALL: u64 = 32;
/// Per-call timeout the Worker applies with an abort signal.
pub const CALL_TIMEOUT_MS: u64 = 8_000;
/// Cap for the signing journal's larger replies that are held whole: V15
/// metadata (about 0.8 MB of hex at spec 486, 873,782 reply bytes at 488)
/// and storage reads such as event lists (about 9 KB at the 488 upgrade
/// block). Block bodies are not held and have no such cap: they are
/// streamed through [`BlockScan`].
pub const MAX_LARGE_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
/// Timeout for the metadata call, which moves about 0.8 MB.
pub const METADATA_TIMEOUT_MS: u64 = 20_000;
/// Timeout for a streamed `chain_getBlock`. A runtime's `System::BlockLength`
/// bounds a block's extrinsics (5 MiB on subtensor), so a reply is at most
/// about twice that in hex plus its envelope; the largest measured, the
/// 486 -> 488 upgrade block 8380857 (5,740,192 bytes), took 1.15 s from
/// build-host. The metadata call's timeout leaves room for the protocol's
/// largest block at well under that rate.
pub const BLOCK_TIMEOUT_MS: u64 = METADATA_TIMEOUT_MS;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpcError {
    /// HTTP 429: the call is not retried within this invocation.
    RateLimited,
    /// Other non-200 status, timeout or network failure.
    Transport,
    TooLarge,
    /// The response is not the JSON-RPC 2.0 shape expected for the call.
    Malformed,
    /// A JSON-RPC error object.
    Remote,
    /// A header whose number or parent does not fit the request.
    Inconsistent,
    /// A `null` header or block for a requested hash: the node behind the
    /// endpoint does not serve that block yet (a load-balanced node behind
    /// another, or a block a delivery named before RPC has it). Retried at
    /// the next delivery or tick, like a transport failure.
    NotServed,
}

/// One JSON-RPC 2.0 request body.
pub fn request(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

pub fn finalized_head_request(id: u64) -> String {
    request(id, "chain_getFinalizedHead", json!([]))
}

pub fn header_request(id: u64, hash: &Id) -> String {
    request(id, "chain_getHeader", json!([hash.to_hex()]))
}

/// List form of `chain_getBlockHash` for the inclusive range `from..=to`.
pub fn block_hashes_request(id: u64, from: u64, to: u64) -> Result<String, RpcError> {
    if to < from || to - from >= MAX_HASHES_PER_CALL || to > MAX_SAFE_INTEGER {
        return Err(RpcError::Inconsistent);
    }
    let heights: Vec<u64> = (from..=to).collect();
    Ok(request(id, "chain_getBlockHash", json!([heights])))
}

fn result(id: u64, body: &[u8]) -> Result<Value, RpcError> {
    result_capped(id, body, MAX_RESPONSE_BYTES)
}

fn result_capped(id: u64, body: &[u8], cap: usize) -> Result<Value, RpcError> {
    if body.len() > cap {
        return Err(RpcError::TooLarge);
    }
    let value: Value = serde_json::from_slice(body).map_err(|_| RpcError::Malformed)?;
    let object = value.as_object().ok_or(RpcError::Malformed)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || object.get("id").and_then(Value::as_u64) != Some(id)
    {
        return Err(RpcError::Malformed);
    }
    if object.contains_key("error") {
        return Err(RpcError::Remote);
    }
    object.get("result").cloned().ok_or(RpcError::Malformed)
}

fn hash(value: &Value) -> Result<Id, RpcError> {
    value
        .as_str()
        .and_then(|s| Id::parse(s).ok())
        .filter(|id| !id.is_zero())
        .ok_or(RpcError::Malformed)
}

/// `0x`-prefixed hexadecimal block number as Substrate encodes it.
fn hex_number(value: &Value) -> Result<u64, RpcError> {
    hex_number_str(value.as_str().ok_or(RpcError::Malformed)?)
}

fn hex_number_str(s: &str) -> Result<u64, RpcError> {
    let raw = s.strip_prefix("0x").ok_or(RpcError::Malformed)?;
    if raw.is_empty() || raw.len() > 16 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RpcError::Malformed);
    }
    let number = u64::from_str_radix(raw, 16).map_err(|_| RpcError::Malformed)?;
    if number > MAX_SAFE_INTEGER {
        return Err(RpcError::Malformed);
    }
    Ok(number)
}

pub fn parse_finalized_head(id: u64, body: &[u8]) -> Result<Id, RpcError> {
    hash(&result(id, body)?)
}

/// Parses the header fetched by `hash`. The header JSON has no hash of its own,
/// so the requested hash is carried in.
pub fn parse_header(id: u64, body: &[u8], requested: Id) -> Result<FinalizedHeader, RpcError> {
    let value = result(id, body)?;
    if value.is_null() {
        return Err(RpcError::NotServed);
    }
    let number = hex_number(value.get("number").ok_or(RpcError::Malformed)?)?;
    let parent = value.get("parentHash").ok_or(RpcError::Malformed)?;
    let parent_hash = if number == 0 { Id::ZERO } else { hash(parent)? };
    if parent_hash == requested {
        return Err(RpcError::Inconsistent);
    }
    Ok(FinalizedHeader {
        number,
        hash: requested,
        parent_hash,
    })
}

/// Parses a list `chain_getBlockHash` reply. Heights the node does not have
/// yet come back as `null`; the caller stops at the first one.
pub fn parse_block_hashes(id: u64, body: &[u8], count: u64) -> Result<Vec<Option<Id>>, RpcError> {
    let value = result(id, body)?;
    let items = value.as_array().ok_or(RpcError::Malformed)?;
    if items.len() as u64 != count {
        return Err(RpcError::Inconsistent);
    }
    items
        .iter()
        .map(|item| match item {
            Value::Null => Ok(None),
            other => hash(other).map(Some),
        })
        .collect()
}

// ------------------------------------------------------------ signing

/// `state_getRuntimeVersion` at `at`.
pub fn runtime_version_request(id: u64, at: &Id) -> String {
    request(id, "state_getRuntimeVersion", json!([at.to_hex()]))
}

/// The live `(specVersion, transactionVersion)`. Observed, never compared
/// with a pinned value: they fill the signing payload's implied fields.
pub fn parse_runtime_version(id: u64, body: &[u8]) -> Result<(u32, u32), RpcError> {
    let v = result(id, body)?;
    let get = |name| {
        v.get(name)
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .ok_or(RpcError::Malformed)
    };
    Ok((get("specVersion")?, get("transactionVersion")?))
}

/// `state_call("Metadata_metadata_at_version", u32 15)` at `at`.
pub fn metadata_request(id: u64, at: &Id) -> String {
    request(
        id,
        "state_call",
        json!(["Metadata_metadata_at_version", "0x0f000000", at.to_hex()]),
    )
}

fn hex_string(value: &Value) -> Result<String, RpcError> {
    let s = value.as_str().ok_or(RpcError::Malformed)?;
    let raw = s.strip_prefix("0x").ok_or(RpcError::Malformed)?;
    if raw.len() % 2 != 0 || !raw.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RpcError::Malformed);
    }
    Ok(s.to_ascii_lowercase())
}

/// A `0x` hex result of up to [`MAX_LARGE_RESPONSE_BYTES`].
pub fn parse_large_hex(id: u64, body: &[u8]) -> Result<String, RpcError> {
    hex_string(&result_capped(id, body, MAX_LARGE_RESPONSE_BYTES)?)
}

/// `state_queryStorageAt(keys, at)`: every key read at one block.
pub fn query_storage_request(id: u64, keys: &[String], at: &Id) -> String {
    request(id, "state_queryStorageAt", json!([keys, at.to_hex()]))
}

/// The `(key, value)` pairs of a `state_queryStorageAt` reply, which must be
/// one change set for exactly the requested block and only requested keys.
pub fn parse_query_storage(
    id: u64,
    body: &[u8],
    keys: &[String],
    at: &Id,
) -> Result<Vec<(String, Option<String>)>, RpcError> {
    let v = result_capped(id, body, MAX_LARGE_RESPONSE_BYTES)?;
    let sets = v.as_array().ok_or(RpcError::Malformed)?;
    let mut out = Vec::new();
    for set in sets {
        if set.get("block").map(hash) != Some(Ok(*at)) {
            return Err(RpcError::Inconsistent);
        }
        for change in set
            .get("changes")
            .and_then(Value::as_array)
            .ok_or(RpcError::Malformed)?
        {
            let Some([key, value]) = change.as_array().map(Vec::as_slice) else {
                return Err(RpcError::Malformed);
            };
            let key = hex_string(key)?;
            if !keys.iter().any(|k| k.eq_ignore_ascii_case(&key)) {
                return Err(RpcError::Inconsistent);
            }
            let value = match value {
                Value::Null => None,
                other => Some(hex_string(other)?),
            };
            out.push((key, value));
        }
    }
    Ok(out)
}

/// `chain_getBlock` at `at`; its reply is read through [`BlockScan`].
pub fn block_request(id: u64, at: &Id) -> String {
    request(id, "chain_getBlock", json!([at.to_hex()]))
}

/// `author_submitExtrinsic`: the only chain write.
pub fn submit_request(id: u64, extrinsic_hex: &str) -> String {
    request(id, "author_submitExtrinsic", json!([extrinsic_hex]))
}

/// How a submission reply is read. Only a JSON-RPC error that proves the
/// node kept the transaction out of its pool is a rejection; an error that
/// can mean it is (or was) in a pool, a malformed reply and anything else
/// stay uncertain, to be resolved from finalized state.
pub fn parse_submit(id: u64, body: &[u8]) -> crate::signing::SubmitOutcome {
    use crate::signing::SubmitOutcome;
    let value: Value = match serde_json::from_slice(body) {
        Ok(v) => v,
        Err(_) => {
            return SubmitOutcome::Uncertain {
                reason: "malformed_reply".into(),
            };
        }
    };
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id").and_then(Value::as_u64) != Some(id)
    {
        return SubmitOutcome::Uncertain {
            reason: "malformed_reply".into(),
        };
    }
    if let Some(result) = value.get("result") {
        return match hex_string(result) {
            Ok(hash) if hash.len() == 66 => SubmitOutcome::Accepted { hash },
            _ => SubmitOutcome::Uncertain {
                reason: "malformed_hash".into(),
            },
        };
    }
    let Some(error) = value.get("error") else {
        return SubmitOutcome::Uncertain {
            reason: "malformed_reply".into(),
        };
    };
    let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
    let message = [error.get("message"), error.get("data")]
        .iter()
        .filter_map(|v| v.and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join(": ");
    let message: String = message
        .chars()
        .filter(|c| !c.is_control())
        .take(160)
        .collect();
    let lower = message.to_ascii_lowercase();
    // 1012 temporarily banned (recently in a pool), 1013 already imported,
    // 1014 a same-nonce replacement is pooled, and an outdated (stale) nonce
    // all leave the fate open.
    let open = matches!(code, 1012..=1014) || lower.contains("outdated") || lower.contains("stale");
    if !open && matches!(code, 1010 | 1011 | 1016 | -32602) {
        SubmitOutcome::Rejected { code, message }
    } else {
        SubmitOutcome::Uncertain {
            reason: format!("rpc_error {code}: {message}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::id;

    #[test]
    fn requests_are_read_only_json_rpc() {
        let head: Value = serde_json::from_str(&finalized_head_request(1)).unwrap();
        assert_eq!(head["method"], "chain_getFinalizedHead");
        assert_eq!(head["params"], json!([]));
        let header: Value = serde_json::from_str(&header_request(2, &id(0xab))).unwrap();
        assert_eq!(header["params"][0], id(0xab).to_hex());
        let list: Value = serde_json::from_str(&block_hashes_request(3, 10, 12).unwrap()).unwrap();
        assert_eq!(list["params"], json!([[10, 11, 12]]));
        assert_eq!(
            block_hashes_request(4, 10, 10 + MAX_HASHES_PER_CALL),
            Err(RpcError::Inconsistent)
        );
        assert!(block_hashes_request(5, 10, 10 + MAX_HASHES_PER_CALL - 1).is_ok());
        assert_eq!(block_hashes_request(6, 10, 9), Err(RpcError::Inconsistent));
    }

    #[test]
    fn parses_the_public_endpoint_shapes() {
        // Shapes recorded from test.finney.opentensor.ai on 2026-10-29.
        let head = br#"{"jsonrpc":"2.0","result":"0x2481be3367a6a44f54b309f69c06f256b84712ce7272634a2932330cdc0998ad","id":1}"#;
        let h = parse_finalized_head(1, head).unwrap();
        let header = br#"{"jsonrpc":"2.0","result":{"digest":{"logs":[]},"extrinsicsRoot":"0x986af3b76d89eef369ea4f1e3b2bcacc4041a6664f5e4552c7bedc01197a188c","number":"0x7f248e","parentHash":"0xe94bb624aa09cb71da505910faccaa3bd029834e561d719459c21146ca2aa289","stateRoot":"0x58c83d41a45a4af69b5de0015f816b780997bb8909da39d9d4cb555d79a26915"},"id":2}"#;
        let parsed = parse_header(2, header, h).unwrap();
        assert_eq!(parsed.number, 0x7f248e);
        assert_eq!(
            parsed.parent_hash.to_hex(),
            "0xe94bb624aa09cb71da505910faccaa3bd029834e561d719459c21146ca2aa289"
        );
        let list = br#"{"jsonrpc":"2.0","result":["0xe93b03a90aba65e4b1622cec7f45b2880b45aa487fc530c531f3c1bcef7c4b03","0xe94bb624aa09cb71da505910faccaa3bd029834e561d719459c21146ca2aa289",null],"id":3}"#;
        let hashes = parse_block_hashes(3, list, 3).unwrap();
        assert_eq!(hashes[1], Some(parsed.parent_hash));
        assert_eq!(hashes[2], None);
        assert_eq!(parse_block_hashes(3, list, 4), Err(RpcError::Inconsistent));
    }

    #[test]
    fn a_block_the_node_does_not_serve_yet_is_not_served_never_malformed() {
        // A contract-2 delivery can name a block before the endpoint (or the
        // node behind it that answers) serves it: its header and body come
        // back `null`, a retryable answer, not a malformed one.
        let null = |id: u64| format!(r#"{{"jsonrpc":"2.0","result":null,"id":{id}}}"#);
        assert_eq!(parse_header(2, null(2).as_bytes(), id(0xab)), Err(RpcError::NotServed));
        assert_eq!(scan_block(4, null(4).as_bytes(), 10, "0x0400"), Err(RpcError::NotServed));
        // A list stops at the first height not served yet (`journal::apply_hashes`).
        let list = format!(r#"{{"jsonrpc":"2.0","result":["{}",null,null],"id":3}}"#, id(0x11).to_hex());
        assert_eq!(parse_block_hashes(3, list.as_bytes(), 3).unwrap(), vec![Some(id(0x11)), None, None]);
        // An unknown-block error stays a remote error (retried behind backoff).
        let unknown = br#"{"jsonrpc":"2.0","error":{"code":4003,"message":"Client error: UnknownBlock: Header was not found in the database"},"id":2}"#;
        assert_eq!(parse_header(2, unknown, id(0xab)), Err(RpcError::Remote));
        // It is one serialized value of the Worker-to-journal round (`RpcRound`).
        assert_eq!(serde_json::to_string(&RpcError::NotServed).unwrap(), r#""not_served""#);
    }

    #[test]
    fn every_rpc_post_names_its_user_agent() {
        // Cloudflare's Browser Integrity Check (rtao.io) refuses a missing or
        // library-default agent (error 1010); every call names this product.
        let (_, ua) = REQUEST_HEADERS.iter().find(|(k, _)| *k == "user-agent").copied().unwrap();
        assert_eq!(ua, USER_AGENT);
        let (product, version) = ua.split_once('/').unwrap();
        assert_eq!(product, "slop-ninja-coordinator-testnet");
        assert_eq!(version, env!("CARGO_PKG_VERSION"));
        assert!(!version.is_empty() && version.bytes().all(|b| b.is_ascii_graphic()));
        let lower = ua.to_ascii_lowercase();
        for library in ["python", "urllib", "curl", "wget", "go-http", "reqwest", "node"] {
            assert!(!lower.contains(library), "{ua} looks like a library default");
        }
        let names: Vec<&str> = REQUEST_HEADERS.iter().map(|(k, _)| *k).collect();
        assert_eq!(names, ["content-type", "accept", "accept-encoding", "user-agent"]);
    }

    #[test]
    fn signing_reads_are_pinned_to_a_block_and_validated() {
        let at = id(0x42);
        let v: Value = serde_json::from_str(&runtime_version_request(1, &at)).unwrap();
        assert_eq!(v["method"], "state_getRuntimeVersion");
        assert_eq!(v["params"][0], at.to_hex());
        let reply = br#"{"jsonrpc":"2.0","result":{"specName":"node-subtensor","specVersion":486,"transactionVersion":1},"id":1}"#;
        assert_eq!(parse_runtime_version(1, reply), Ok((486, 1)));
        let m: Value = serde_json::from_str(&metadata_request(2, &at)).unwrap();
        assert_eq!(
            m["params"],
            json!(["Metadata_metadata_at_version", "0x0f000000", at.to_hex()])
        );
        let keys = vec!["0xaa".to_owned(), "0xbb".to_owned()];
        let q: Value = serde_json::from_str(&query_storage_request(3, &keys, &at)).unwrap();
        assert_eq!(q["params"], json!([["0xaa", "0xbb"], at.to_hex()]));
        let ok = format!(
            r#"{{"jsonrpc":"2.0","result":[{{"block":"{}","changes":[["0xaa","0x0100"],["0xbb",null]]}}],"id":3}}"#,
            at.to_hex()
        );
        assert_eq!(
            parse_query_storage(3, ok.as_bytes(), &keys, &at).unwrap(),
            vec![
                ("0xaa".into(), Some("0x0100".into())),
                ("0xbb".into(), None)
            ]
        );
        let other = format!(
            r#"{{"jsonrpc":"2.0","result":[{{"block":"{}","changes":[]}}],"id":3}}"#,
            id(0x43).to_hex()
        );
        assert_eq!(
            parse_query_storage(3, other.as_bytes(), &keys, &at),
            Err(RpcError::Inconsistent)
        );
        let foreign = format!(
            r#"{{"jsonrpc":"2.0","result":[{{"block":"{}","changes":[["0xcc","0x01"]]}}],"id":3}}"#,
            at.to_hex()
        );
        assert_eq!(
            parse_query_storage(3, foreign.as_bytes(), &keys, &at),
            Err(RpcError::Inconsistent)
        );
        let block = br#"{"jsonrpc":"2.0","result":{"block":{"header":{"number":"0x10"},"extrinsics":["0x0400","0xABCD"]},"justifications":null},"id":4}"#;
        assert_eq!(
            scan_block(4, block, 16, "0xabcd").unwrap(),
            BlockMatch { count: 2, index: Some(1) }
        );
        assert_eq!(scan_block(4, block, 16, "0xab").unwrap(), BlockMatch { count: 2, index: None });
        assert_eq!(scan_block(4, block, 17, "0xabcd"), Err(RpcError::Inconsistent));
    }

    #[test]
    fn submission_replies_are_rejections_only_when_provably_unpooled() {
        use crate::signing::SubmitOutcome;
        let s: Value = serde_json::from_str(&submit_request(5, "0x1234")).unwrap();
        assert_eq!(
            (s["method"].as_str(), &s["params"]),
            (Some("author_submitExtrinsic"), &json!(["0x1234"]))
        );
        let hash = format!(
            r#"{{"jsonrpc":"2.0","result":"0x{}","id":5}}"#,
            "ab".repeat(32)
        );
        assert!(matches!(
            parse_submit(5, hash.as_bytes()),
            SubmitOutcome::Accepted { .. }
        ));
        let bad = br#"{"jsonrpc":"2.0","error":{"code":1010,"message":"Invalid Transaction","data":"Custom error: 6"},"id":5}"#;
        assert_eq!(
            parse_submit(5, bad),
            SubmitOutcome::Rejected {
                code: 1010,
                message: "Invalid Transaction: Custom error: 6".into()
            }
        );
        for open in [
            br#"{"jsonrpc":"2.0","error":{"code":1010,"message":"Invalid Transaction","data":"Transaction is outdated"},"id":5}"#.as_slice(),
            br#"{"jsonrpc":"2.0","error":{"code":1013,"message":"Transaction Already Imported"},"id":5}"#,
            br#"{"jsonrpc":"2.0","error":{"code":1014,"message":"Priority is too low"},"id":5}"#,
            br#"{"jsonrpc":"2.0","error":{"code":-32000,"message":"overloaded"},"id":5}"#,
            br#"<html>502</html>"#,
            br#"{"jsonrpc":"2.0","result":"0x12","id":5}"#,
            br#"{"jsonrpc":"2.0","result":"0x12","id":6}"#,
        ] {
            assert!(matches!(parse_submit(5, open), SubmitOutcome::Uncertain { .. }), "{}", String::from_utf8_lossy(open));
        }
    }

    #[test]
    fn refuses_malformed_or_mismatched_replies() {
        let ok = br#"{"jsonrpc":"2.0","result":"0x2481be3367a6a44f54b309f69c06f256b84712ce7272634a2932330cdc0998ad","id":1}"#;
        assert_eq!(parse_finalized_head(9, ok), Err(RpcError::Malformed));
        let error = br#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"x"},"id":1}"#;
        assert_eq!(parse_finalized_head(1, error), Err(RpcError::Remote));
        let upper = br#"{"jsonrpc":"2.0","result":"0x2481BE3367A6A44F54B309F69C06F256B84712CE7272634A2932330CDC0998AD","id":1}"#;
        assert_eq!(parse_finalized_head(1, upper), Err(RpcError::Malformed));
        let zero = format!(
            r#"{{"jsonrpc":"2.0","result":"0x{}","id":1}}"#,
            "00".repeat(32)
        );
        assert_eq!(
            parse_finalized_head(1, zero.as_bytes()),
            Err(RpcError::Malformed)
        );
        let big = vec![b' '; MAX_RESPONSE_BYTES + 1];
        assert_eq!(parse_finalized_head(1, &big), Err(RpcError::TooLarge));
        let float = br#"{"jsonrpc":"2.0","result":{"number":12,"parentHash":"0xe94bb624aa09cb71da505910faccaa3bd029834e561d719459c21146ca2aa289"},"id":2}"#;
        assert_eq!(parse_header(2, float, id(1)), Err(RpcError::Malformed));
        let unsafe_number = br#"{"jsonrpc":"2.0","result":{"number":"0x20000000000000","parentHash":"0xe94bb624aa09cb71da505910faccaa3bd029834e561d719459c21146ca2aa289"},"id":2}"#;
        assert_eq!(
            parse_header(2, unsafe_number, id(1)),
            Err(RpcError::Malformed)
        );
    }
}
