ALTER TABLE memory_versions
  DROP CONSTRAINT IF EXISTS memory_versions_text_check,
  ADD CONSTRAINT memory_versions_text_check CHECK (char_length(text) BETWEEN 1 AND 200000);
