-- slop_ninja_detect_store_v1: the detect Worker's D1 store (its only writer).
--
-- No column holds text, an IP address, a value derived from either, a user
-- id or an email address. Timestamps are integer milliseconds UTC. Every
-- *_h column is a SHA-256 or HMAC-SHA-256 under its own domain prefix, and
-- the prefixes are part of the schema version. Foreign keys are enforced (D1
-- enforces them by default). Migrations are applied by the deploy wrapper's
-- D1 step, which refuses a list that skips a version.

CREATE TABLE schema_info (k TEXT PRIMARY KEY, v TEXT NOT NULL);           -- schema id, migration list digest

CREATE TABLE switches (                                                   -- runtime kill switches
  name TEXT PRIMARY KEY, state TEXT NOT NULL, at INTEGER NOT NULL, journal_seq INTEGER NOT NULL);

CREATE TABLE sources (
  source_id INTEGER PRIMARY KEY,
  canon_key BLOB NOT NULL UNIQUE,            -- H("source_v1" ‖ "url:<canonical>" | "bsky:<did>/<rkey>" | "x:status:<id>")
  canon_url TEXT,                            -- NULL only on an X source: a refusal row or a removal tombstone
  host TEXT NOT NULL, adapter TEXT NOT NULL CHECK (adapter IN ('web','bluesky','x')),
  title TEXT, site_name TEXT, author TEXT, published_at INTEGER, lang TEXT,
  latest_det BLOB, fetched_at INTEGER, fresh_until INTEGER, fetch_outcome TEXT,
  etag TEXT, last_modified TEXT,             -- conditional GET for "Check the page again"
  state TEXT NOT NULL DEFAULT 'ok' CHECK (state IN ('ok','gone','opted_out','unlisted')),
  reputation TEXT NOT NULL CHECK (reputation IN ('ranked','unranked','adult')),
  rank_list_v TEXT NOT NULL,
  check_queued_at INTEGER,                   -- X: an x_check message is out for this source
  gone_candidate_at INTEGER,                 -- X: oEmbed's first "gone" answer, awaiting confirmation
  edit_check_at INTEGER,                     -- X: when the post's edit window closes (plus grace)
  x_mismatch_at INTEGER,                     -- X: a lookup contradicted oEmbed for this source
  CHECK (canon_url IS NOT NULL OR adapter = 'x'),
  CHECK (adapter <> 'x' OR (host = 'x.com' AND title IS NULL AND site_name IS NULL AND author IS NULL  -- no X content
         AND published_at IS NULL AND lang IS NULL AND etag IS NULL AND last_modified IS NULL)));
CREATE INDEX sources_host ON sources(host);
CREATE INDEX sources_x_fetched ON sources(fetched_at) WHERE adapter = 'x' AND canon_url IS NOT NULL;   -- the X sweep
CREATE INDEX sources_x_candidate ON sources(gone_candidate_at) WHERE gone_candidate_at IS NOT NULL;
CREATE INDEX sources_x_edit ON sources(edit_check_at) WHERE edit_check_at IS NOT NULL;
CREATE INDEX sources_x_marked ON sources(x_mismatch_at) WHERE x_mismatch_at IS NOT NULL;     -- the count of marked sources

CREATE TABLE source_aliases (alias_key BLOB PRIMARY KEY, source_id INTEGER NOT NULL REFERENCES sources(source_id));

CREATE TABLE runs (                          -- public runs only; private runs live inside their encrypted document
  run_id INTEGER PRIMARY KEY, content_key BLOB UNIQUE,
  package_load TEXT NOT NULL, package_pub TEXT NOT NULL, package_label TEXT NOT NULL,
  policy_v TEXT NOT NULL, canon_v TEXT NOT NULL, extract_v TEXT, display_v TEXT NOT NULL, evidence_id TEXT NOT NULL,
  band TEXT NOT NULL, words INTEGER NOT NULL, total_words INTEGER NOT NULL, cut TEXT NOT NULL, tokens INTEGER NOT NULL,
  scored_at INTEGER NOT NULL, doc_key TEXT NOT NULL, doc_bytes INTEGER NOT NULL);

CREATE TABLE detections (
  id_h BLOB PRIMARY KEY,                     -- public: H("slug_v1" ‖ slug); secret: HMAC(pepper_v, "slug_v1" ‖ slug)
  pepper_v INTEGER,                          -- secret only
  public_slug TEXT UNIQUE,                   -- public only
  kind TEXT NOT NULL CHECK (kind IN ('public_url','private_text','private_paid')),
  platform TEXT NOT NULL CHECK (platform IN ('paste','web','bluesky','x')),                  -- export exclusion
  state TEXT NOT NULL CHECK (state IN ('pending','live','failed','excerpt_withheld','removed','disabled',
                                       'deleted','expired')),
  state_v INTEGER NOT NULL DEFAULT 1,
  refusal TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, expires_at INTEGER,
  source_id INTEGER REFERENCES sources(source_id), run_id INTEGER REFERENCES runs(run_id),
  superseded_by BLOB, package_load TEXT,
  manage_h BLOB,                             -- H("manage_v1" ‖ token)
  owner_h BLOB,                              -- HMAC(K_owner, user_id) when saved with ownership or paid; else NULL
  owner_via TEXT CHECK (owner_via IN ('created','paid','claimed')),                          -- how owner_h was set
  seal_nonce_h BLOB UNIQUE,                  -- one share per scan
  doc_key TEXT, doc_bytes INTEGER,
  sitemap_seq INTEGER UNIQUE, index_force TEXT CHECK (index_force IN ('on','off')),           -- operator override
  copy_of BLOB,                              -- a secret re-run copy's family root, the first original's id_h
  expiry_cap INTEGER,                        -- a copy's latest allowed expires_at, the original's
  CHECK ((owner_h IS NULL) = (owner_via IS NULL)),
  CHECK (platform <> 'x' OR kind = 'public_url'));                                             -- an X post is a public page
CREATE INDEX det_expiry ON detections(expires_at) WHERE expires_at IS NOT NULL;
CREATE INDEX det_copy ON detections(copy_of) WHERE copy_of IS NOT NULL;                      -- copy families
CREATE INDEX det_source ON detections(source_id);
CREATE INDEX det_owner ON detections(owner_h) WHERE owner_h IS NOT NULL;                     -- owner purge
CREATE INDEX det_sitemap ON detections(sitemap_seq) WHERE sitemap_seq IS NOT NULL;

CREATE TABLE claims (claim_key BLOB PRIMARY KEY, det_id_h BLOB NOT NULL, lease_until INTEGER NOT NULL, stage TEXT NOT NULL);

CREATE TABLE jobs (                          -- paid whole-document checks
  det_id_h BLOB PRIMARY KEY REFERENCES detections(id_h),
  owner_h BLOB NOT NULL, hold_ref TEXT NOT NULL UNIQUE,  -- the account ledger's hold id (opaque here)
  package_load TEXT NOT NULL, policy_v TEXT NOT NULL,
  sections_total INTEGER NOT NULL, sections_done INTEGER NOT NULL, tokens_billed INTEGER NOT NULL,
  lease_token BLOB, lease_until INTEGER,     -- the fencing token of the driver holding the work lease
  state TEXT NOT NULL CHECK (state IN ('running','scored','committed','failed','cancelled')),
  failure TEXT, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);

CREATE TABLE cases (
  case_id TEXT PRIMARY KEY, status_h BLOB NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('copyright','author','personal_data','wrong_result','illegal','counter_notice',
                                     'court_order','other_legal')),
  state TEXT NOT NULL CHECK (state IN ('received','incomplete','reviewing','accepted','countered','restored',
                                       'rejected','closed')),
  submitter_h BLOB,                          -- HMAC(K_owner, user_id) when submitted while signed in
  received_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, due_at INTEGER, notice_key TEXT NOT NULL);
CREATE TABLE case_targets (case_id TEXT NOT NULL, det_id_h BLOB NOT NULL, PRIMARY KEY (case_id, det_id_h));
CREATE TABLE strikes (owner_h BLOB NOT NULL, case_id TEXT NOT NULL, at INTEGER NOT NULL,     -- strikes per owner, for repeat notices
  owner_via TEXT NOT NULL CHECK (owner_via IN ('created','paid','claimed')),                 -- copied from the page
  review TEXT CHECK (review IN ('counts','not_counted')),    -- claimed strikes: the operator's review; NULL until then
  PRIMARY KEY (owner_h, case_id));

CREATE TABLE op_journal (seq INTEGER PRIMARY KEY, at INTEGER NOT NULL, actor TEXT NOT NULL, op TEXT NOT NULL,
  signature BLOB, nonce_h BLOB UNIQUE);
CREATE TABLE removals (seq INTEGER PRIMARY KEY, at INTEGER NOT NULL, public_slug TEXT, scope TEXT NOT NULL,
  basis TEXT NOT NULL, jurisdiction TEXT NOT NULL);
CREATE TABLE optouts (host TEXT PRIMARY KEY, at INTEGER NOT NULL, journal_seq INTEGER NOT NULL);
CREATE TABLE sitemap_shards (shard INTEGER PRIMARY KEY, dirty INTEGER NOT NULL, urls INTEGER NOT NULL, built_at INTEGER);
CREATE TABLE examples (slot INTEGER PRIMARY KEY CHECK (slot BETWEEN 1 AND 4),
  source TEXT NOT NULL CHECK (source IN ('synthetic','public')),
  public_slug TEXT,                          -- source = public
  doc_key TEXT,                              -- source = synthetic: R2 x/examples/<v>/<slot>
  journal_seq INTEGER NOT NULL,
  CHECK ((source = 'public') = (public_slug IS NOT NULL)), CHECK ((source = 'synthetic') = (doc_key IS NOT NULL)));

CREATE TABLE hour_counters (                 -- scans per hour, content-free
  hour TEXT NOT NULL, channel TEXT NOT NULL, package_load TEXT NOT NULL, outcome TEXT NOT NULL, n INTEGER NOT NULL,
  PRIMARY KEY (hour, channel, package_load, outcome));
  -- one writer: the DetectGate alarm, by absolute-value upsert. Paid documents are counted in paid_receipts only.
CREATE TABLE paid_receipts (                 -- one per counted paid document
  receipt_id TEXT PRIMARY KEY, hour TEXT NOT NULL, channel TEXT NOT NULL, package_load TEXT NOT NULL,
  hold_ref TEXT NOT NULL UNIQUE, tokens INTEGER NOT NULL, cash INTEGER NOT NULL, at INTEGER NOT NULL);
CREATE TABLE edge_coverage (                 -- per model-running route and UTC hour
  route TEXT NOT NULL, hour TEXT NOT NULL, covered INTEGER NOT NULL, ruleset_v TEXT, probe TEXT,
  journal_seq INTEGER NOT NULL, PRIMARY KEY (route, hour));

INSERT INTO schema_info (k, v) VALUES ('schema', 'slop_ninja_detect_store_v1');
