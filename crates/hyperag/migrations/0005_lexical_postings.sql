-- Stable numeric posting keys keep term indexes narrow; old rows are filled only in bounded batches.
CREATE SEQUENCE IF NOT EXISTS lexical_document_posting_seq;
ALTER TABLE lexical_documents ADD COLUMN IF NOT EXISTS posting_id bigint;
ALTER TABLE lexical_documents ALTER COLUMN posting_id SET DEFAULT nextval('lexical_document_posting_seq');
ALTER TABLE lexical_documents ADD COLUMN IF NOT EXISTS current_version int;
ALTER TABLE lexical_documents ADD COLUMN IF NOT EXISTS status text NOT NULL DEFAULT 'active';
ALTER TABLE lexical_documents ADD COLUMN IF NOT EXISTS recorded_at timestamptz;
CREATE UNIQUE INDEX IF NOT EXISTS lexical_documents_posting_id ON lexical_documents(posting_id);
CREATE TABLE IF NOT EXISTS lexical_postings (
  posting_id bigint NOT NULL REFERENCES lexical_documents(posting_id) ON DELETE CASCADE,
  lexeme text NOT NULL, scope text NOT NULL, tf int NOT NULL,
  PRIMARY KEY(lexeme,scope,posting_id) INCLUDE(tf)
);
CREATE INDEX IF NOT EXISTS lexical_postings_document ON lexical_postings(posting_id);
INSERT INTO settings(key,value) VALUES('lexical_postings_ready','false') ON CONFLICT DO NOTHING;

CREATE OR REPLACE FUNCTION lexical_sync_postings() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='UPDATE' AND OLD.posting_id=NEW.posting_id
     AND OLD.tf IS NOT DISTINCT FROM NEW.tf AND OLD.scope=NEW.scope THEN RETURN NULL; END IF;
  DELETE FROM lexical_postings WHERE posting_id=NEW.posting_id;
  INSERT INTO lexical_postings(posting_id,lexeme,scope,tf)
    SELECT NEW.posting_id,k.key,NEW.scope,k.value::int FROM jsonb_each_text(NEW.tf) k;
  RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS lexical_postings_sync ON lexical_documents;
CREATE TRIGGER lexical_postings_sync AFTER INSERT OR UPDATE ON lexical_documents
  FOR EACH ROW EXECUTE FUNCTION lexical_sync_postings();

CREATE OR REPLACE FUNCTION lexical_refresh_document(wanted_id text) RETURNS void LANGUAGE plpgsql AS $$
DECLARE wanted record;
BEGIN
  PERFORM pg_advisory_xact_lock(1213811052,1);
  SELECT m.id AS memory_id,v.version,m.scope,coalesce(v.valid_from,'-infinity'::timestamptz) AS valid_from,
         coalesce(v.valid_to,'infinity'::timestamptz) AS valid_to,length(v.text_search) AS doc_len,
         coalesce((SELECT jsonb_object_agg(u.lexeme,coalesce(array_length(u.positions,1),1))
                     FROM unnest(v.text_search) u),'{}'::jsonb) AS tf,
         m.current_version,v.status,v.recorded_at
    INTO wanted FROM memories m JOIN memory_versions v ON v.memory_id=m.id AND v.version=m.current_version
   WHERE m.id=wanted_id AND m.deleted_at IS NULL AND v.status='active';
  IF NOT FOUND THEN
    DELETE FROM lexical_documents WHERE memory_id=wanted_id;
  ELSE
    INSERT INTO lexical_documents(memory_id,version,scope,valid_from,valid_to,doc_len,tf,current_version,status,recorded_at)
    VALUES(wanted.memory_id,wanted.version,wanted.scope,wanted.valid_from,wanted.valid_to,
           wanted.doc_len,wanted.tf,wanted.current_version,wanted.status,wanted.recorded_at)
    ON CONFLICT(memory_id) DO UPDATE SET version=excluded.version,scope=excluded.scope,
      valid_from=excluded.valid_from,valid_to=excluded.valid_to,doc_len=excluded.doc_len,tf=excluded.tf,
      current_version=excluded.current_version,status=excluded.status,recorded_at=excluded.recorded_at,
      posting_id=coalesce(lexical_documents.posting_id,excluded.posting_id)
    WHERE lexical_documents.posting_id IS NULL OR
      (lexical_documents.version,lexical_documents.scope,lexical_documents.valid_from,lexical_documents.valid_to,
       lexical_documents.doc_len,lexical_documents.tf,lexical_documents.current_version,
       lexical_documents.status,lexical_documents.recorded_at)
      IS DISTINCT FROM (excluded.version,excluded.scope,excluded.valid_from,excluded.valid_to,
       excluded.doc_len,excluded.tf,excluded.current_version,excluded.status,excluded.recorded_at);
  END IF;
END $$;

CREATE OR REPLACE FUNCTION lexical_postings_backfill(batch_size int DEFAULT 64) RETURNS int LANGUAGE plpgsql AS $$
DECLARE ids text[]; wanted_id text;
BEGIN
  IF batch_size IS NULL OR batch_size<1 OR batch_size>256 THEN RAISE EXCEPTION 'postings backfill batch must be from 1 to 256'; END IF;
  PERFORM pg_advisory_xact_lock(1213811052,1);
  IF (SELECT value FROM settings WHERE key='lexical_cache_ready') IS DISTINCT FROM 'true' THEN
    RAISE EXCEPTION 'complete lexical_cache_backfill first';
  END IF;
  SELECT array_agg(memory_id ORDER BY memory_id) INTO ids FROM (
    SELECT memory_id FROM lexical_documents WHERE posting_id IS NULL OR recorded_at IS NULL
     ORDER BY memory_id LIMIT batch_size
  ) missing;
  IF ids IS NULL THEN
    UPDATE settings SET value='true' WHERE key='lexical_postings_ready'; RETURN 0;
  END IF;
  FOREACH wanted_id IN ARRAY ids LOOP PERFORM lexical_refresh_document(wanted_id); END LOOP;
  RETURN array_length(ids,1);
END $$;
UPDATE settings SET value='true' WHERE key='lexical_postings_ready' AND value='false'
  AND NOT EXISTS(SELECT 1 FROM lexical_documents);
