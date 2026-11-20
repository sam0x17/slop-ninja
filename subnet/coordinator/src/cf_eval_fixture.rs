//! Feature-only routes for an isolated synthetic package deployment. Normal
//! webhook, scheduler, queue and evaluation dispatch are disabled in this build.
//! The deployment review must also prove bucket/namespace isolation: binding
//! names alone cannot establish which Cloudflare resources they reference.
use super::*;
use crate::eval::package_fixture_import::{self as import, Table};
use crate::sql::{self, StoreError};
use crate::eval::package_fixture_metadata as metadata;

const FIXTURES: &str = "PACKAGE_FIXTURES";
const STATE: &str = "package_fixture_progress";
const MANIFEST_BYTES: usize = 1024 * 1024;
const CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Clone, Default, Deserialize, Serialize)]
struct Progress {
    digest: String,
    initialized: bool,
    tables: usize,
    objects: usize,
    started: bool,
    completed: bool,
    #[serde(default)]
    outputs: usize,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    measurement: Option<Value>,
    #[serde(default)]
    steps_completed: u64,
    #[serde(default)]
    absent_outputs: usize,
    #[serde(default)]
    next_part: usize,
    #[serde(default)]
    next_offset: u64,
    #[serde(default)]
    after_rowid: i64,
    #[serde(default)]
    step_in_flight: Option<u64>,
    #[serde(default)]
    table_readback: Option<import::TableReadback>,
    #[serde(default)]
    total_package_wall_ms: u64,
}

fn failure(code: &str) -> worker::Error { worker::Error::RustError(code.into()) }
fn require(ok: bool, code: &str) -> Result<()> { if ok { Ok(()) } else { Err(failure(code)) } }
fn tx_ok<T, E: Into<engine::EvalError>>(r: std::result::Result<T, TxError<E>>) -> Result<T> {
    r.map_err(|e| match e {
        TxError::Operation(e) => failure(e.into().code()),
        TxError::Runtime => failure("fixture runtime transaction refused"),
    })
}

fn isolation(env: &Env) -> Result<()> {
    require(env.var("PACKAGE_FIXTURE_ENABLED").ok().map(|v| v.to_string()).as_deref() == Some("synthetic-only"), "fixture mode missing")?;
    for secret in ["COORDINATOR_HOTKEY_SEED", "AUTHORIZATION_SIGNING_KEY", "RTAO_WEBHOOK_SIGNING_SECRET"] {
        require(env.secret(secret).is_err(), "fixture has a production capability")?;
    }
    for binding in ["CHAIN_JOURNAL", "EVAL_LANE", "A_SCORER", "SPEND_LEDGER", "GEN_LIMITER"] {
        require(env.durable_object(binding).is_err(), "fixture has a workload binding")?;
    }
    require(secret32(env, "EVAL_PRIVATE_SEED") == Some([5; 32])
        && secret32(env, "EVAL_PACKAGE_SIGNING_KEY") == Some([0x51; 32])
        && secret32(env, "EVAL_RECEIPT_SIGNING_KEY") == Some([0x52; 32]), "fixture requires public synthetic seeds")?;
    keys(env).map_err(failure)?;
    Ok(())
}

pub(in crate::cf) async fn route(req: Request, env: Env) -> Result<Response> {
    isolation(&env)?;
    match super::super::bearer_ok(&req, &env, crate::access::OPERATOR_TOKEN) {
        Ok(true) => (),
        Ok(false) => return super::super::unauthorized(),
        Err(()) => return error("token_not_configured", 503),
    }
    require(req.path().starts_with("/qualification/"), "fixture route only")?;
    // An explicit repetition name creates a separate database, never the live
    // main/load identity. The deployment has its own EvalCohort namespace.
    let repetition = req.headers().get("x-fixture-repetition")?.ok_or_else(|| failure("fixture repetition missing"))?;
    require(!repetition.is_empty() && repetition.len() <= 64 && repetition.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'), "fixture repetition")?;
    require(env.var("PACKAGE_FIXTURE_REPETITION")?.to_string() == repetition, "fixture repetition not deployed")?;
    let stub = env.durable_object("EVAL_COHORT")?.get_by_name(&format!("synthetic-package/{repetition}"))?;
    stub.fetch_with_request(req).await
}

async fn bytes(env: &Env, bucket: &str, key: &str, bound: usize, digest: &str) -> Result<Vec<u8>> {
    let value = get_bounded(env, bucket, key, bound).await.map_err(|_| failure("fixture object read"))?.ok_or_else(|| failure("fixture object missing"))?;
    require(sha_hex(&value) == digest, "fixture object digest")?;
    Ok(value)
}

/// Validate fixture inputs and expected outputs using one arena page of
/// scratch space. Full model-output objects can exceed the control scope.
async fn verify_object(env: &Env, bucket: &str, key: &str, size: usize, digest: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    let object = env.bucket(bucket)?.get(key).execute().await?.ok_or_else(|| failure("fixture object missing"))?;
    require(object.size() == size as u64, "fixture object length")?;
    let body = object.body().ok_or_else(|| failure("fixture object body missing"))?.response_body()?;
    let mut response = Response::from_body(body)?;
    response.headers_mut().set("content-length", &size.to_string())?;
    let mut hash = Sha256::new();
    super::super::cf_io::response_scan(&mut response, &mut |part| {
        hash.update(part);
        true
    }).await.map_err(|_| failure("fixture object scan"))?;
    require(hex::encode(hash.finalize()) == digest, "fixture object digest")
}

fn tables(manifest: &Value, name: &str) -> Result<Vec<Table>> {
    serde_json::from_value(manifest[name].clone()).map_err(|_| failure("fixture table descriptors"))
}

async fn paged_item(env: &Env, list: &metadata::List, index: usize) -> Result<Value> {
    let mut lookup = list.lookup(index).map_err(failure)?;
    loop {
        let page = get_bounded(env, FIXTURES, &metadata::key(&lookup.digest), metadata::PAGE_BYTES)
            .await.map_err(|_| failure("fixture descriptor read"))?.ok_or_else(|| failure("fixture descriptor missing"))?;
        match lookup.read(&page).map_err(failure)? {
            metadata::Read::Next(next) => lookup = next,
            metadata::Read::Item(item) => return Ok(item),
        }
    }
}

enum Descriptors<'a> {
    Inline(&'a [Value]),
    Paged(metadata::List),
}

impl<'a> Descriptors<'a> {
    fn parse(value: &'a Value, paged: bool) -> Result<Self> {
        if paged {
            let list: metadata::List = serde_json::from_value(value.clone()).map_err(|_| failure("fixture descriptor list"))?;
            require(list.valid(), "fixture descriptor root")?;
            Ok(Self::Paged(list))
        } else {
            Ok(Self::Inline(value.as_array().ok_or_else(|| failure("fixture descriptor array"))?))
        }
    }

    fn len(&self) -> usize {
        match self { Self::Inline(items) => items.len(), Self::Paged(list) => list.count }
    }

    async fn item(&self, env: &Env, index: usize) -> Result<Value> {
        match self {
            Self::Inline(items) => items.get(index).cloned().ok_or_else(|| failure("fixture descriptor index")),
            Self::Paged(list) => paged_item(env, list, index).await,
        }
    }
}

async fn check_absent(env: &Env, object: &Value) -> Result<()> {
    let bucket = match object["bucket"].as_str() { Some("expected-eval") => EVAL_BUCKET, Some("expected-receipts") => RECEIPTS_BUCKET, _ => return Err(failure("fixture output bucket")) };
    let key = object["key"].as_str().ok_or_else(|| failure("fixture output key"))?;
    let existing = get_bounded(env, bucket, key, 0).await.map_err(|_| failure("fixture output absent check refused"))?;
    require(existing.is_none(), "fixture output already exists")
}

impl EvalCohort {
    pub(in crate::cf) async fn fixture_fetch(&self, req: Request, scope: &ScopeRef) -> Result<Response> {
        // R2 awaits permit interleaving. Reject concurrent setup/run requests;
        // durable progress handles later requests and object eviction.
        require(!self.fixture_busy.replace(true), "fixture request in progress")?;
        struct Busy<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Busy<'_> { fn drop(&mut self) { self.0.set(false); } }
        let _busy = Busy(&self.fixture_busy);
        self.fixture_fetch_inner(req, scope).await
    }

    async fn fixture_fetch_inner(&self, req: Request, scope: &ScopeRef) -> Result<Response> {
        isolation(&self.env)?;
        require(self.schema.is_ok(), "fixture schema unavailable")?;
        let digest = self.env.var("PACKAGE_FIXTURE_SHA256")?.to_string();
        let key = self.env.var("PACKAGE_FIXTURE_KEY")?.to_string();
        let repetition = self.env.var("PACKAGE_FIXTURE_REPETITION")?.to_string();
        let manifest: Value = serde_json::from_slice(&bytes(&self.env, FIXTURES, &key, MANIFEST_BYTES, &digest).await?).map_err(|_| failure("fixture manifest JSON"))?;
        let paged = manifest["schema"] == "slop_ninja_synthetic_package_fixture_v7";
        let chunked = if paged {
            require(matches!(manifest["receipt_format"].as_str(), Some("legacy-v1" | "chunked-v2")), "fixture receipt format")?;
            manifest["receipt_format"] == "chunked-v2"
        } else {
            let chunked = manifest["schema"] == "slop_ninja_synthetic_package_fixture_v6";
            require(manifest["schema"] == "slop_ninja_synthetic_package_fixture_v5" || chunked, "fixture schema")?;
            require(chunked == (manifest["plan"]["receipt_index_format"] == "chunked-v2"), "fixture receipt format differs")?;
            chunked
        };
        require(manifest["synthetic_only"] == true, "fixture synthetic flag")?;
        // Each target bucket must contain an operator-installed marker bound
        // to this fixture before setup or package writes are possible.
        for bucket in [EVAL_BUCKET, RECEIPTS_BUCKET] {
            let marker = get_bounded(&self.env, bucket, "qualification-bucket.json", 1024).await.map_err(|_| failure("fixture bucket marker"))?.ok_or_else(|| failure("fixture bucket marker absent"))?;
            let marker: Value = serde_json::from_slice(&marker).map_err(|_| failure("fixture bucket marker JSON"))?;
            require(marker == j!({"synthetic_only":true,"fixture_sha256":digest,"binding":bucket,"repetition":repetition}), "fixture bucket binding")?;
        }
        let storage = self.state.storage();
        let saved = storage.get::<Progress>(STATE).await?;
        let mut progress = saved.clone().unwrap_or_default();
        require(progress.digest.is_empty() || progress.digest == digest, "fixture identity changed")?;
        let incoming = tables(&manifest, "tables")?;
        require(incoming.iter().all(|t| t.chunk_index.is_some() == paged), "fixture table metadata format")?;
        let objects = Descriptors::parse(&manifest["objects"], paged)?;
        let outputs = Descriptors::parse(&manifest["expected"]["objects"], paged)?;
        let path = req.path();
        if req.method() == Method::Get && path == "/qualification/status" {
            return json(&progress);
        }
        if req.method() == Method::Get && let Some(step) = path.strip_prefix("/qualification/measurement/") {
            let step: u64 = step.parse().map_err(|_| failure("fixture measurement step"))?;
            require(step < progress.steps_completed, "fixture measurement incomplete")?;
            let measurement = storage.get::<Value>(&format!("package_fixture_step_{step}")).await?
                .ok_or_else(|| failure("fixture measurement missing"))?;
            return json(&measurement);
        }
        require(req.method() == Method::Post, "fixture method")?;
        if path == "/qualification/init" {
            if progress.initialized { return json(&progress); }
            progress.digest = digest;
            storage.put(STATE, &progress).await?;
            let baseline = tables(&manifest, "baseline_tables")?;
            let recovering = saved.is_some();
            tx_ok(transaction(&storage, move |db| {
                import::check_roster(db, &incoming)?;
                let mut empty = true;
                for table in &incoming {
                    if table.table == "schema_meta" { import::verify_table(db, table)?; continue; }
                    let rows = db.exec(&format!("SELECT COUNT(*) FROM \"{}\"", table.table), &[])?;
                    empty &= sql::uint(rows.first().ok_or(StoreError("fixture count"))?, 0)? == 0;
                }
                if recovering && empty { Ok(()) } else { import::clear_pristine(db, &baseline, &incoming) }
            }))?;
            progress.initialized = true;
            storage.put(STATE, &progress).await?;
            return json(&progress);
        }
        require(progress.initialized, "fixture not initialized")?;
        if let Some(index) = path.strip_prefix("/qualification/check-output/") {
            require(paged && !progress.started, "fixture output preflight unavailable")?;
            let index: usize = index.parse().map_err(|_| failure("fixture output index"))?;
            require(index <= progress.absent_outputs, "fixture output preflight order")?;
            check_absent(&self.env, &outputs.item(&self.env, index).await?).await?;
            if index == progress.absent_outputs {
                progress.absent_outputs += 1;
                storage.put(STATE, &progress).await?;
            }
            return json(&progress);
        }
        if let Some(index) = path.strip_prefix("/qualification/verify-output/") {
            require(progress.started, "fixture package not attempted")?;
            let index: usize = index.parse().map_err(|_| failure("fixture output index"))?;
            require(index <= progress.outputs, "fixture output verification order")?;
            let object = outputs.item(&self.env, index).await?;
            let bucket = match object["bucket"].as_str() { Some("expected-eval") => EVAL_BUCKET, Some("expected-receipts") => RECEIPTS_BUCKET, _ => return Err(failure("fixture output bucket")) };
            let key = object["key"].as_str().ok_or_else(|| failure("fixture output key"))?;
            let digest = object["sha256"].as_str().ok_or_else(|| failure("fixture output hash"))?;
            let size = object["bytes"].as_u64().and_then(|v| usize::try_from(v).ok()).ok_or_else(|| failure("fixture output size"))?;
            require(size <= 16 * 1024 * 1024, "fixture output readback bound")?;
            verify_object(&self.env, bucket, key, size, digest).await?;
            if index == progress.outputs {
                progress.outputs += 1;
                storage.put(STATE, &progress).await?;
            }
            return json(&progress);
        }
        if path == "/qualification/reconcile" {
            require(progress.started && outputs.len() > 0 && progress.outputs == outputs.len(), "fixture output verification incomplete")?;
            let seq = manifest["seq"].as_i64().ok_or_else(|| failure("fixture seq"))?;
            let expected = manifest["expected"].clone();
            tx_ok(transaction(&storage, move |db| import::verify_package_commit(db, seq, &expected)))?;
            // Recovery never re-enters package_v3. A committed result can be
            // verified even if its response or measurement was lost. Missing
            // telemetry stays missing and is not a performance pass.
            progress.completed = true;
            progress.verified = true;
            storage.put(STATE, &progress).await?;
            return json(&progress);
        }
        if let Some(rest) = path.strip_prefix("/qualification/chunk/") {
            require(!progress.started, "fixture package already started")?;
            let (table, part) = rest.split_once('/').ok_or_else(|| failure("fixture chunk path"))?;
            let table: usize = table.parse().map_err(|_| failure("fixture table index"))?;
            let part: usize = part.parse().map_err(|_| failure("fixture chunk index"))?;
            require(table == progress.tables, "fixture table order")?;
            let table = incoming.get(table).cloned().ok_or_else(|| failure("fixture table absent"))?;
            let chunk: import::Chunk = if let Some(list) = &table.chunk_index {
                require(part <= progress.next_part, "fixture chunk order")?;
                serde_json::from_value(paged_item(&self.env, list, part).await?)
                    .map_err(|_| failure("fixture chunk descriptor"))?
            } else {
                table.chunks.get(part).cloned().ok_or_else(|| failure("fixture chunk absent"))?
            };
            let advancing = paged && part == progress.next_part;
            if advancing {
                require(chunk.offset == progress.next_offset && chunk.after_rowid == progress.after_rowid, "fixture chunk continuity")?;
            }
            let end = chunk.offset.checked_add(chunk.rows).ok_or_else(|| failure("fixture chunk row overflow"))?;
            let last = chunk.last_rowid;
            let body = bytes(&self.env, FIXTURES, &chunk.file, CHUNK_BYTES, &chunk.sha256).await?;
            tx_ok(transaction(&storage, move |db| import::import_descriptor(db, &table, &chunk, &body)))?;
            if advancing {
                progress.next_part += 1;
                progress.next_offset = end;
                progress.after_rowid = last;
                storage.put(STATE, &progress).await?;
            }
            return json(&progress);
        }
        if let Some(index) = path.strip_prefix("/qualification/verify-table/") {
            require(!progress.started, "fixture package already started")?;
            let index: usize = index.parse().map_err(|_| failure("fixture table index"))?;
            if index < progress.tables { return json(&progress); }
            require(index == progress.tables, "fixture table verification order")?;
            let table = incoming.get(progress.tables).cloned().ok_or_else(|| failure("fixture tables already verified"))?;
            if let Some(list) = &table.chunk_index {
                require(progress.next_part == list.count && progress.next_offset == table.rows, "fixture chunks incomplete")?;
            }
            if paged {
                let cursor = progress.table_readback.take();
                progress.table_readback = tx_ok(transaction(&storage, move |db| import::verify_table_page(db, &table, cursor)))?;
                if progress.table_readback.is_some() {
                    storage.put(STATE, &progress).await?;
                    return json(&progress);
                }
            } else {
                tx_ok(transaction(&storage, move |db| import::verify_table(db, &table)))?;
            }
            progress.next_part = 0;
            progress.next_offset = 0;
            progress.after_rowid = 0;
            progress.tables += 1;
            storage.put(STATE, &progress).await?;
            return json(&progress);
        }
        if let Some(index) = path.strip_prefix("/qualification/verify-object/") {
            require(!progress.started, "fixture package already started")?;
            let index: usize = index.parse().map_err(|_| failure("fixture object index"))?;
            if index < progress.objects { return json(&progress); }
            require(index == progress.objects, "fixture object verification order")?;
            let object = objects.item(&self.env, progress.objects).await?;
            let bucket = match object["bucket"].as_str() { Some("eval") => EVAL_BUCKET, Some("receipts") => RECEIPTS_BUCKET, _ => return Err(failure("fixture object bucket")) };
            let key = object["key"].as_str().ok_or_else(|| failure("fixture object key"))?;
            let digest = object["sha256"].as_str().ok_or_else(|| failure("fixture object hash"))?;
            let size = object["bytes"].as_u64().and_then(|v| usize::try_from(v).ok()).ok_or_else(|| failure("fixture object size"))?;
            // Setup can verify large R2 objects independently of package timing.
            require(size <= 16 * 1024 * 1024, "fixture setup object bound")?;
            verify_object(&self.env, bucket, key, size, digest).await?;
            progress.objects += 1;
            storage.put(STATE, &progress).await?;
            return json(&progress);
        }
        if path == "/qualification/run" || path.starts_with("/qualification/step/") {
            let step = if path == "/qualification/run" { 0 } else {
                require(chunked, "legacy fixture has one package invocation")?;
                path.strip_prefix("/qualification/step/").ok_or_else(|| failure("fixture step path"))?
                    .parse::<u64>().map_err(|_| failure("fixture step number"))?
            };
            if step < progress.steps_completed { return json(&progress); }
            require(progress.tables == incoming.len() && progress.objects == objects.len()
                && !progress.completed && progress.step_in_flight.is_none() && step == progress.steps_completed,
                "fixture setup incomplete or step unresolved")?;
            require((step == 0) != progress.started, "fixture start state")?;
            let expected_steps = if chunked {
                manifest["receipt_count"].as_u64().ok_or_else(|| failure("fixture receipt count"))?
                    .div_ceil(slop_ninja_protocol::receipt_chunks::CHUNK_ENTRIES as u64) + 1
            } else { 1 };
            require(step < expected_steps, "fixture extra package step")?;
            let seq = manifest["seq"].as_i64().ok_or_else(|| failure("fixture seq"))?;
            let cfg = eval_config(&self.env, engine::INSTANCE_MAIN).map_err(failure)?;
            let actual = j!({"private_seed":hex::encode(cfg.private_seed),"instance":cfg.instance,
                "chain":{"genesis":cfg.chain.genesis.to_string(),"netuid":cfg.chain.netuid,"mechanism":cfg.chain.mechanism},
                "authority":cfg.authority.name(),"rules":cfg.authority.rules().name(),"max_score_lanes":cfg.max_score_lanes,
                "roster_cap":cfg.ceilings.roster_cap,"accept_reward_v0":cfg.ceilings.accept_reward_v0,"exposed_pools":cfg.exposed_pools,"scorer_binary":cfg.scorer_binary});
            require(actual == manifest["configuration"], "fixture configuration differs")?;
            let cohort = tx_ok(transaction(&storage, move |db| engine::cohort(db, seq)))?;
            let plan = serde_json::to_value(&cohort.plan)?;
            let plan_matches = if paged {
                sha_hex(&serde_json::to_vec(&plan)?) == manifest["plan_sha256"].as_str().unwrap_or("")
                    && chunked == (plan["receipt_index_format"] == "chunked-v2")
            } else { plan == manifest["plan"] };
            require(cohort.phase == "packaging" && plan_matches, "fixture package state")?;
            // Every deployed repetition has one allowed DO identity. Refuse
            // stale output keys, including outputs from another repetition in
            // accidentally reused buckets. A zero bound reads no object body;
            // a nonempty existing object or any transport error also refuses.
            require(outputs.len() > 0, "fixture expected outputs empty")?;
            if step == 0 {
                match &outputs {
                    Descriptors::Paged(list) => require(progress.absent_outputs == list.count, "fixture output preflight incomplete")?,
                    Descriptors::Inline(items) => {
                        for object in *items { check_absent(&self.env, object).await?; }
                    }
                }
            }
            progress.started = true;
            progress.step_in_flight = Some(step);
            storage.put(STATE, &progress).await?;
            let arena_before = super::super::arena_exports::arena_status();
            let (jobs, docs, span, feedback) = package_shape(&cohort.plan);
            let child = self.step_scope_physical(scope, crate::arena_routes::R_COHORT_PACKAGE, seq,
                crate::arena_routes::package_step_bytes(jobs, docs, span, feedback)).map_err(failure)?;
            let started = now_ms();
            // Keep this exact production method unchanged. Do not invoke the
            // native oracle, load-v0 branch, scheduler or a reduced operation.
            let result = child.scoped(self.package_v3(&cfg, seq, &cohort.plan)).map_err(|_| failure("fixture scope"))?.await;
            let wall_ms = now_ms().saturating_sub(started);
            let _ = child.close();
            let measurement = j!({"step":step,"package_wall_ms":wall_ms,"arena_before":arena_before,
                "arena_after":super::super::arena_exports::arena_status(),"cpu_ms":null,"whole_isolate_peak_bytes":null,
                "limit":"This is one invocation. Qualification requires every step's CPU and whole-isolate peak; sum CPU across all steps and retain the maximum peak. Arena counters alone do not qualify either."});
            storage.put(&format!("package_fixture_step_{step}"), &measurement).await?;
            progress.measurement = Some(measurement);
            progress.total_package_wall_ms = progress.total_package_wall_ms.saturating_add(wall_ms);
            storage.put(STATE, &progress).await?;
            result.map_err(failure)?;
            let cohort = tx_ok(transaction(&storage, move |db| engine::cohort(db, seq)))?;
            require(cohort.phase == if step + 1 == expected_steps { "transition" } else { "packaging" }, "fixture package phase differs")?;
            progress.steps_completed = step + 1;
            progress.step_in_flight = None;
            progress.completed = cohort.phase == "transition";
            storage.put(STATE, &progress).await?;
            return json(&progress);
        }
        error("fixture_route_unknown", 404)
    }
}
