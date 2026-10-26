-- Source fence only: no index worker, rebuild, or retrieval mode is enabled here.
CREATE TABLE IF NOT EXISTS lexical_index_state (
  singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
  database_id uuid NOT NULL DEFAULT gen_random_uuid(),
  epoch bigint NOT NULL DEFAULT 0 CHECK (epoch >= 0),
  retained_after bigint NOT NULL DEFAULT 0 CHECK (retained_after BETWEEN 0 AND epoch),
  fence_token uuid NOT NULL DEFAULT gen_random_uuid(),
  previous_token uuid, last_writer_xid xid8
);
INSERT INTO lexical_index_state(singleton) VALUES(true) ON CONFLICT DO NOTHING;
CREATE TABLE IF NOT EXISTS lexical_index_changes (
  epoch bigint NOT NULL, memory_id text, kind text NOT NULL,
  previous_token uuid NOT NULL, fence_token uuid NOT NULL,
  CHECK ((kind='memory' AND memory_id IS NOT NULL)
      OR (kind IN ('authorization','reset') AND memory_id IS NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS lexical_index_event_identity
  ON lexical_index_changes(epoch,kind,coalesce(memory_id,''));

-- Same guard as all Store writers, before their first row/table lock. Direct
-- administrative FOR UPDATE/TRUNCATE transactions must acquire it explicitly first.
CREATE OR REPLACE FUNCTION lexical_write_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM pg_advisory_xact_lock(1213811052,1);
  RETURN NULL;
END $$;
CREATE OR REPLACE FUNCTION lexical_index_emit(wanted_id text, event_kind text)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE previous uuid; next_token uuid; next_epoch bigint; writer xid8;
BEGIN
  PERFORM pg_advisory_xact_lock(1213811052,1);
  SELECT previous_token,fence_token,epoch,last_writer_xid INTO previous,next_token,next_epoch,writer
    FROM lexical_index_state;
  IF NOT FOUND THEN RAISE EXCEPTION 'missing lexical source fence'; END IF;
  -- One committed source state per transaction; coalesce repeated IDs without
  -- repeatedly updating a global counter during a bulk statement.
  IF writer IS DISTINCT FROM pg_current_xact_id() THEN
    UPDATE lexical_index_state SET epoch=epoch+1,previous_token=fence_token,
      fence_token=gen_random_uuid(),last_writer_xid=pg_current_xact_id()
      RETURNING epoch,fence_token,previous_token INTO next_epoch,next_token,previous;
  END IF;
  INSERT INTO lexical_index_changes VALUES(next_epoch,wanted_id,event_kind,previous,next_token)
    ON CONFLICT DO NOTHING;
END $$;
CREATE OR REPLACE FUNCTION lexical_index_source_changed() RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE old_id text; new_id text;
BEGIN
  IF TG_OP='TRUNCATE' THEN PERFORM lexical_index_emit(NULL,'reset');
  ELSIF TG_TABLE_NAME='grants' THEN PERFORM lexical_index_emit(NULL,'authorization');
  ELSE
    IF TG_OP<>'INSERT' THEN
      IF TG_TABLE_NAME='memories' THEN old_id:=OLD.id; ELSE old_id:=OLD.memory_id; END IF;
    END IF;
    IF TG_OP<>'DELETE' THEN
      IF TG_TABLE_NAME='memories' THEN new_id:=NEW.id; ELSE new_id:=NEW.memory_id; END IF;
    END IF;
    PERFORM lexical_index_emit(coalesce(new_id,old_id),'memory');
    IF old_id IS NOT NULL AND new_id IS NOT NULL AND old_id<>new_id THEN
      PERFORM lexical_index_emit(old_id,'memory');
    END IF;
  END IF;
  RETURN NULL;
END $$;
DO $$
DECLARE relation text; guard text;
BEGIN
  FOR relation,guard IN SELECT * FROM (VALUES
    ('memories','lexical_memory_guard'),('memory_versions','lexical_version_guard'),
    ('grants','lexical_grant_guard')) AS guards(relation,guard)
  LOOP
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I',guard,relation);
    EXECUTE format('CREATE TRIGGER %I BEFORE INSERT OR UPDATE OR DELETE OR TRUNCATE ON %I
      FOR EACH STATEMENT EXECUTE FUNCTION lexical_write_guard()',guard,relation);
    EXECUTE format('DROP TRIGGER IF EXISTS lexical_index_change ON %I',relation);
    EXECUTE format('CREATE TRIGGER lexical_index_change AFTER INSERT OR UPDATE OR DELETE ON %I
      FOR EACH ROW EXECUTE FUNCTION lexical_index_source_changed()',relation);
    EXECUTE format('DROP TRIGGER IF EXISTS lexical_index_reset ON %I',relation);
    EXECUTE format('CREATE TRIGGER lexical_index_reset AFTER TRUNCATE ON %I
      FOR EACH STATEMENT EXECUTE FUNCTION lexical_index_source_changed()',relation);
  END LOOP;
END $$;

-- Retention is explicit. Readers below this durable floor must rebuild/fall back.
CREATE OR REPLACE FUNCTION lexical_index_prune(through_epoch bigint)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE current_epoch bigint;
BEGIN
  PERFORM pg_advisory_xact_lock(1213811052,1);
  IF EXISTS (SELECT 1 FROM lexical_index_state WHERE last_writer_xid=pg_current_xact_id()) THEN
    RAISE EXCEPTION 'retention requires a separate transaction from source writes';
  END IF;
  SELECT epoch INTO current_epoch FROM lexical_index_state;
  IF current_epoch IS NULL OR through_epoch IS NULL OR through_epoch<0 OR through_epoch>current_epoch THEN
    RAISE EXCEPTION 'invalid lexical outbox retention boundary';
  END IF;
  DELETE FROM lexical_index_changes WHERE epoch<=through_epoch;
  UPDATE lexical_index_state SET retained_after=greatest(retained_after,through_epoch);
END $$;
