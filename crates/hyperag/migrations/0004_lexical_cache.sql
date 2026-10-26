-- Derived current-document statistics. Source rows and scoring defaults are unchanged.
CREATE TABLE IF NOT EXISTS lexical_documents (
  memory_id text PRIMARY KEY REFERENCES memories(id) ON DELETE CASCADE,
  version int NOT NULL, scope text NOT NULL,
  valid_from timestamptz NOT NULL, valid_to timestamptz NOT NULL,
  doc_len int NOT NULL CHECK (doc_len >= 0), tf jsonb NOT NULL
);
CREATE TABLE IF NOT EXISTS lexical_corpus_stats (
  scope text NOT NULL, valid_from timestamptz NOT NULL, valid_to timestamptz NOT NULL,
  n bigint NOT NULL, total_len bigint NOT NULL,
  PRIMARY KEY(scope,valid_from,valid_to)
);
CREATE TABLE IF NOT EXISTS lexical_term_stats (
  scope text NOT NULL, lexeme text NOT NULL,
  valid_from timestamptz NOT NULL, valid_to timestamptz NOT NULL, df bigint NOT NULL,
  PRIMARY KEY(scope,lexeme,valid_from,valid_to)
);
CREATE INDEX IF NOT EXISTS lexical_zero_terms ON lexical_term_stats(scope,valid_from,valid_to) WHERE df=0;
INSERT INTO settings(key,value) VALUES('lexical_cache_ready','false') ON CONFLICT DO NOTHING;

-- Serialize metadata writes before source row locks, including explicit backfill. Reads use MVCC.
CREATE OR REPLACE FUNCTION lexical_write_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_advisory_xact_lock(1213811052,1);
  RETURN NULL;
END $$;

CREATE OR REPLACE FUNCTION lexical_apply_delta() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
  old_doc lexical_documents%ROWTYPE;
  new_doc lexical_documents%ROWTYPE;
BEGIN
  IF TG_OP <> 'INSERT' THEN old_doc := OLD; END IF;
  IF TG_OP <> 'DELETE' THEN new_doc := NEW; END IF;
  WITH delta AS (
    SELECT old_doc.scope AS scope,old_doc.valid_from AS vf,old_doc.valid_to AS vt,
           -1::bigint AS n,-old_doc.doc_len::bigint AS dl WHERE old_doc.memory_id IS NOT NULL
    UNION ALL
    SELECT new_doc.scope,new_doc.valid_from,new_doc.valid_to,1::bigint,new_doc.doc_len::bigint
     WHERE new_doc.memory_id IS NOT NULL
  )
  INSERT INTO lexical_corpus_stats(scope,valid_from,valid_to,n,total_len)
  SELECT scope,vf,vt,sum(n),sum(dl) FROM delta GROUP BY scope,vf,vt
   HAVING sum(n)<>0 OR sum(dl)<>0 ORDER BY scope,vf,vt
  ON CONFLICT(scope,valid_from,valid_to) DO UPDATE
    SET n=lexical_corpus_stats.n+excluded.n,total_len=lexical_corpus_stats.total_len+excluded.total_len;

  WITH delta AS (
    SELECT old_doc.scope AS scope,k.lexeme,old_doc.valid_from AS vf,old_doc.valid_to AS vt,-1::bigint AS df
      FROM jsonb_object_keys(coalesce(old_doc.tf,'{}'::jsonb)) AS k(lexeme)
    UNION ALL
    SELECT new_doc.scope,k.lexeme,new_doc.valid_from,new_doc.valid_to,1::bigint
      FROM jsonb_object_keys(coalesce(new_doc.tf,'{}'::jsonb)) AS k(lexeme)
  )
  INSERT INTO lexical_term_stats(scope,lexeme,valid_from,valid_to,df)
  SELECT scope,lexeme,vf,vt,sum(df) FROM delta GROUP BY scope,lexeme,vf,vt
   HAVING sum(df)<>0 ORDER BY scope,lexeme,vf,vt
  ON CONFLICT(scope,lexeme,valid_from,valid_to) DO UPDATE SET df=lexical_term_stats.df+excluded.df;

  DELETE FROM lexical_corpus_stats WHERE n=0 AND (scope,valid_from,valid_to) IN
    ((old_doc.scope,old_doc.valid_from,old_doc.valid_to),(new_doc.scope,new_doc.valid_from,new_doc.valid_to));
  DELETE FROM lexical_term_stats WHERE df=0 AND (scope,valid_from,valid_to) IN
    ((old_doc.scope,old_doc.valid_from,old_doc.valid_to),(new_doc.scope,new_doc.valid_from,new_doc.valid_to));
  RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS lexical_delta ON lexical_documents;
CREATE TRIGGER lexical_delta AFTER INSERT OR UPDATE OR DELETE ON lexical_documents
  FOR EACH ROW EXECUTE FUNCTION lexical_apply_delta();

CREATE OR REPLACE FUNCTION lexical_refresh_document(wanted_id text) RETURNS void LANGUAGE plpgsql AS $$
DECLARE wanted lexical_documents%ROWTYPE;
BEGIN
  PERFORM pg_advisory_xact_lock(1213811052,1);
  SELECT m.id,v.version,m.scope,coalesce(v.valid_from,'-infinity'::timestamptz),
         coalesce(v.valid_to,'infinity'::timestamptz),length(v.text_search),
         coalesce((SELECT jsonb_object_agg(u.lexeme,coalesce(array_length(u.positions,1),1))
                     FROM unnest(v.text_search) u),'{}'::jsonb)
    INTO wanted FROM memories m JOIN memory_versions v ON v.memory_id=m.id AND v.version=m.current_version
   WHERE m.id=wanted_id AND m.deleted_at IS NULL AND v.status='active';
  IF NOT FOUND THEN
    DELETE FROM lexical_documents WHERE memory_id=wanted_id;
  ELSE
    INSERT INTO lexical_documents VALUES(wanted.memory_id,wanted.version,wanted.scope,
      wanted.valid_from,wanted.valid_to,wanted.doc_len,wanted.tf)
    ON CONFLICT(memory_id) DO UPDATE SET version=excluded.version,scope=excluded.scope,
      valid_from=excluded.valid_from,valid_to=excluded.valid_to,doc_len=excluded.doc_len,tf=excluded.tf
    WHERE (lexical_documents.version,lexical_documents.scope,lexical_documents.valid_from,
           lexical_documents.valid_to,lexical_documents.doc_len,lexical_documents.tf)
      IS DISTINCT FROM (excluded.version,excluded.scope,excluded.valid_from,excluded.valid_to,excluded.doc_len,excluded.tf);
  END IF;
END $$;

CREATE OR REPLACE FUNCTION lexical_memory_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='INSERT' THEN PERFORM lexical_refresh_document(NEW.id);
  ELSIF TG_OP='DELETE' THEN PERFORM lexical_refresh_document(OLD.id);
  ELSE
    PERFORM lexical_refresh_document(OLD.id);
    IF NEW.id IS DISTINCT FROM OLD.id THEN PERFORM lexical_refresh_document(NEW.id); END IF;
  END IF;
  RETURN NULL;
END $$;
CREATE OR REPLACE FUNCTION lexical_version_changed() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP='INSERT' THEN PERFORM lexical_refresh_document(NEW.memory_id);
  ELSIF TG_OP='DELETE' THEN PERFORM lexical_refresh_document(OLD.memory_id);
  ELSE
    PERFORM lexical_refresh_document(OLD.memory_id);
    IF NEW.memory_id IS DISTINCT FROM OLD.memory_id THEN PERFORM lexical_refresh_document(NEW.memory_id); END IF;
  END IF;
  RETURN NULL;
END $$;
CREATE OR REPLACE FUNCTION lexical_truncated() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  INSERT INTO settings(key,value) VALUES('lexical_cache_ready','reset')
    ON CONFLICT(key) DO UPDATE SET value=excluded.value;
  RETURN NULL;
END $$;
DROP TRIGGER IF EXISTS lexical_memory_guard ON memories;
CREATE TRIGGER lexical_memory_guard BEFORE INSERT OR UPDATE OR DELETE OR TRUNCATE ON memories
  FOR EACH STATEMENT EXECUTE FUNCTION lexical_write_guard();
DROP TRIGGER IF EXISTS lexical_version_guard ON memory_versions;
CREATE TRIGGER lexical_version_guard BEFORE INSERT OR UPDATE OR DELETE OR TRUNCATE ON memory_versions
  FOR EACH STATEMENT EXECUTE FUNCTION lexical_write_guard();
DROP TRIGGER IF EXISTS lexical_memory_sync ON memories;
CREATE TRIGGER lexical_memory_sync AFTER INSERT OR UPDATE OR DELETE ON memories
  FOR EACH ROW EXECUTE FUNCTION lexical_memory_changed();
DROP TRIGGER IF EXISTS lexical_version_sync ON memory_versions;
CREATE TRIGGER lexical_version_sync AFTER INSERT OR UPDATE OR DELETE ON memory_versions
  FOR EACH ROW EXECUTE FUNCTION lexical_version_changed();
DROP TRIGGER IF EXISTS lexical_memory_truncate ON memories;
CREATE TRIGGER lexical_memory_truncate AFTER TRUNCATE ON memories FOR EACH STATEMENT EXECUTE FUNCTION lexical_truncated();
DROP TRIGGER IF EXISTS lexical_version_truncate ON memory_versions;
CREATE TRIGGER lexical_version_truncate AFTER TRUNCATE ON memory_versions FOR EACH STATEMENT EXECUTE FUNCTION lexical_truncated();

-- Explicit bounded backfill, rather than an unbounded startup scan. Zero means complete/ready.
CREATE OR REPLACE FUNCTION lexical_cache_backfill(batch_size int DEFAULT 64) RETURNS int LANGUAGE plpgsql AS $$
DECLARE ids text[]; wanted_id text;
BEGIN
  IF batch_size IS NULL OR batch_size<1 OR batch_size>256 THEN RAISE EXCEPTION 'lexical backfill batch must be from 1 to 256'; END IF;
  PERFORM pg_advisory_xact_lock(1213811052,1);
  IF (SELECT value FROM settings WHERE key='lexical_cache_ready')='reset' THEN
    TRUNCATE lexical_documents,lexical_corpus_stats,lexical_term_stats;
    UPDATE settings SET value='false' WHERE key='lexical_cache_ready';
  END IF;
  SELECT array_agg(id ORDER BY id) INTO ids FROM (
    SELECT m.id FROM memories m JOIN memory_versions v ON v.memory_id=m.id AND v.version=m.current_version
      LEFT JOIN lexical_documents d ON d.memory_id=m.id
     WHERE m.deleted_at IS NULL AND v.status='active' AND (d.memory_id IS NULL OR d.version<>v.version)
     ORDER BY m.id LIMIT batch_size
  ) missing;
  IF ids IS NULL THEN
    UPDATE settings SET value='true' WHERE key='lexical_cache_ready';
    RETURN 0;
  END IF;
  FOREACH wanted_id IN ARRAY ids LOOP PERFORM lexical_refresh_document(wanted_id); END LOOP;
  RETURN array_length(ids,1);
END $$;
UPDATE settings SET value='true' WHERE key='lexical_cache_ready' AND value='false' AND NOT EXISTS (
  SELECT 1 FROM memories m JOIN memory_versions v ON v.memory_id=m.id AND v.version=m.current_version
   WHERE m.deleted_at IS NULL AND v.status='active'
);
