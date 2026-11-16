-- slop_ninja_detect_store_v2: every check makes its page; pasted-text pages are content-addressed; no page expires
-- by time. One exception to 0001's rule that no column holds a value derived from text: texts.text_h, an HMAC under
-- the slug pepper, which a dump cannot test against any text without the pepper. The store keeps a text's address
-- parameters, never its slug.

CREATE TABLE texts (                         -- a pasted text's address: its keyed hash -> its current page
  text_h BLOB PRIMARY KEY,                   -- HMAC(pepper_v, "text_v1" 0 "canon_v1" 0 ‖ text)
  pepper_v INTEGER NOT NULL,
  gen INTEGER NOT NULL CHECK (gen >= 0),     -- the current page's generation (+1 at the first save after a delete)
  slug_len INTEGER NOT NULL CHECK (slug_len BETWEEN 8 AND 19),  -- 8: permalink-policy/v1's floor; 20 is the drawn form
  det_id_h BLOB NOT NULL UNIQUE REFERENCES detections(id_h),    -- the current generation's page, in any state
  hold TEXT CHECK (hold IN ('notice')),      -- set when a disabled page is deleted; cleared by its case or release_text
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);

-- No page expires any more. Pages already past their expiry end now (the backstop removes their documents, as for
-- every ended row), with the live and pending re-run copies of an ended root; every other page loses its end date.
UPDATE detections SET state = 'expired', doc_key = NULL, doc_bytes = NULL, manage_h = NULL, owner_h = NULL,
  owner_via = NULL, state_v = state_v + 1, updated_at = CAST(strftime('%s','now') AS INTEGER) * 1000
WHERE expires_at IS NOT NULL AND expires_at <= CAST(strftime('%s','now') AS INTEGER) * 1000
  AND state IN ('live','pending','failed');
UPDATE detections SET state = 'expired', doc_key = NULL, doc_bytes = NULL, manage_h = NULL, owner_h = NULL,
  owner_via = NULL, state_v = state_v + 1, updated_at = CAST(strftime('%s','now') AS INTEGER) * 1000
WHERE copy_of IN (SELECT id_h FROM detections WHERE state = 'expired') AND state IN ('live','pending');
UPDATE detections SET expires_at = NULL, expiry_cap = NULL WHERE expires_at IS NOT NULL OR expiry_cap IS NOT NULL;
DROP INDEX det_expiry;
UPDATE schema_info SET v = 'slop_ninja_detect_store_v2' WHERE k = 'schema';
