//! A fresh private generation stays pending until its entire source snapshot is indexed.
use crate::lexical_snapshot::SourceDocument;
use crate::lexical_snapshot::{Batch, Checkpoint, SourceSnapshot};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};
use tantivy::{
    Index, IndexReader, IndexWriter, ReloadPolicy, Searcher, TantivyDocument, Term,
    directory::{Directory, INDEX_WRITER_LOCK, META_LOCK},
    merge_policy::NoMergePolicy,
    schema::{FAST, IndexRecordOption, STORED, STRING, Schema, TextFieldIndexing, TextOptions},
    tokenizer::{PreTokenizedString, Token},
};

pub(super) struct Generation {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    target: Checkpoint,
    source: SourceSnapshot,
    interrupted: bool,
    sealed: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: u32,
    schema_hash: String,
    checkpoint: Checkpoint,
    complete: bool,
}

fn schema() -> Schema {
    let mut schema = Schema::builder();
    schema.add_text_field("id", STRING | FAST | STORED);
    schema.add_text_field("scope", STRING | FAST | STORED);
    for name in ["version", "recorded_at", "valid_from", "valid_to"] {
        schema.add_i64_field(name, FAST | STORED);
    }
    schema.add_text_field(
        "terms",
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("raw")
                .set_index_option(IndexRecordOption::WithFreqs),
        ),
    );
    schema.build()
}

fn fingerprint(schema: &Schema) -> String {
    hex::encode(Sha256::digest(
        format!(
            "postgres-simple-tf-v1\n{}",
            serde_json::to_string(schema).expect("static lexical schema serialization")
        )
        .as_bytes(),
    ))
}

fn document(source: SourceDocument, schema: &Schema) -> anyhow::Result<TantivyDocument> {
    let mut doc = TantivyDocument::default();
    doc.add_text(schema.get_field("id")?, source.id);
    doc.add_text(schema.get_field("scope")?, source.scope);
    for (name, value) in [
        ("version", i64::from(source.version)),
        ("recorded_at", source.recorded_at.timestamp_micros()),
        (
            "valid_from",
            source.valid_from.map_or(i64::MIN, |t| t.timestamp_micros()),
        ),
        (
            "valid_to",
            source.valid_to.map_or(i64::MAX, |t| t.timestamp_micros()),
        ),
    ] {
        doc.add_i64(schema.get_field(name)?, value);
    }
    let mut tokens = Vec::new();
    for (lexeme, tf) in source.terms.0 {
        anyhow::ensure!(tf > 0, "invalid PostgreSQL term frequency");
        for _ in 0..tf {
            tokens.push(Token {
                text: lexeme.clone(),
                position: tokens.len(),
                offset_from: 0,
                offset_to: 0,
                position_length: 1,
            });
        }
    }
    doc.add_pre_tokenized_text(
        schema.get_field("terms")?,
        PreTokenizedString {
            text: String::new(),
            tokens,
        },
    );
    Ok(doc)
}

impl Generation {
    #[cfg(test)]
    pub fn source_for_test(&mut self) -> &mut SourceSnapshot {
        &mut self.source
    }

    pub fn replay(path: &Path, base: &Path, source: SourceSnapshot) -> anyhow::Result<Self> {
        let expected = source.fresh_replay_base().ok_or_else(|| {
            anyhow::anyhow!("replay requires an unconsumed source stream and its base checkpoint")
        })?;
        let index = Index::open_in_dir(base)?;
        let _writer = index.directory().acquire_lock(&INDEX_WRITER_LOCK)?;
        let _files = index.directory().acquire_lock(&META_LOCK)?;
        let manifest = complete_manifest(&index)?
            .ok_or_else(|| anyhow::anyhow!("incomplete or incompatible replay base"))?;
        anyhow::ensure!(
            manifest.checkpoint == *expected,
            "replay base checkpoint mismatch"
        );
        private_directory(path)?;
        // shortcut: copy base files once; use reflinks if measured copy cost warrants it.
        for entry in fs::read_dir(base)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == INDEX_WRITER_LOCK.filepath.as_os_str()
                || name == META_LOCK.filepath.as_os_str()
            {
                continue;
            }
            anyhow::ensure!(entry.file_type()?.is_file(), "nonregular replay base entry");
            fs::copy(entry.path(), path.join(name))?;
        }
        Self::initialize(Index::open_in_dir(path)?, source)
    }
    pub fn create(path: &Path, source: SourceSnapshot) -> anyhow::Result<Self> {
        anyhow::ensure!(
            source.is_fresh_rebuild(),
            "a new generation requires an unconsumed full rebuild snapshot"
        );
        private_directory(path)?;
        Self::initialize(Index::create_in_dir(path, schema())?, source)
    }

    fn initialize(index: Index, source: SourceSnapshot) -> anyhow::Result<Self> {
        let writer = index.writer_with_num_threads(1, 32 * 1024 * 1024)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let mut generation = Self {
            index,
            writer,
            reader,
            target: source.target_checkpoint().clone(),
            source,
            interrupted: false,
            sealed: false,
        };
        generation.commit_state(false)?;
        Ok(generation)
    }
    /// Owning the reader prevents skipping batches; an interrupted step requires rebuild.
    pub async fn advance(&mut self) -> anyhow::Result<bool> {
        anyhow::ensure!(
            !self.sealed && !self.interrupted,
            "generation must be rebuilt after interruption"
        );
        self.interrupted = true;
        let Some(batch) = self.source.next_batch().await? else {
            if self
                .index
                .searchable_segment_metas()?
                .iter()
                .any(|segment| segment.has_deletes())
            {
                // Native merges approximate token totals; rebuild at the same MVCC fence.
                self.writer.delete_all_documents()?;
                self.source.rewind_for_rebuild().await?;
                self.commit_state(false)?;
                self.interrupted = false;
                return Ok(true);
            }
            self.interrupted = false;
            return Ok(false);
        };
        self.apply(batch)?;
        self.interrupted = false;
        Ok(true)
    }

    fn apply(&mut self, batch: Batch) -> anyhow::Result<()> {
        anyhow::ensure!(!self.sealed, "generation is immutable after completion");
        let schema = self.index.schema();
        let documents = batch
            .documents
            .into_iter()
            .map(|source| document(source, &schema))
            .collect::<anyhow::Result<Vec<_>>>()?;
        for id in batch.ids {
            self.writer
                .delete_term(Term::from_field_text(schema.get_field("id")?, &id));
        }
        for doc in documents {
            self.writer.add_document(doc)?;
        }
        self.commit_state(false)
    }
    pub fn publish(&mut self) -> anyhow::Result<Searcher> {
        anyhow::ensure!(
            !self.sealed
                && !self.interrupted
                && self.source.completed_checkpoint() == Some(&self.target),
            "publication requires exhaustion of the matching source snapshot"
        );
        self.commit_state(true)?;
        self.sealed = true;
        self.reader.reload()?;
        Ok(self.reader.searcher())
    }
    fn commit_state(&mut self, complete: bool) -> anyhow::Result<()> {
        let payload = serde_json::to_string(&Manifest {
            format: 1,
            schema_hash: fingerprint(&self.index.schema()),
            checkpoint: self.target.clone(),
            complete,
        })?;
        let mut commit = self.writer.prepare_commit()?;
        commit.set_payload(&payload);
        commit.commit()?;
        Ok(())
    }
}

pub(super) fn recover(path: &Path, source: &Checkpoint) -> anyhow::Result<Option<Searcher>> {
    let index = Index::open_in_dir(path)?;
    // Fence inspection and reader reload must not race another local writer's commit.
    let _owner = index.directory().acquire_lock(&INDEX_WRITER_LOCK)?;
    let Some(manifest) = complete_manifest(&index)? else {
        return Ok(None);
    };
    if manifest.checkpoint != *source {
        return Ok(None);
    }
    let reader: IndexReader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    reader.reload()?;
    Ok(Some(reader.searcher()))
}

fn private_directory(path: &Path) -> anyhow::Result<()> {
    let mut directory = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directory.mode(0o700);
    }
    directory.create(path)?; // Existing generations are never overwritten.
    Ok(())
}

fn complete_manifest(index: &Index) -> anyhow::Result<Option<Manifest>> {
    let meta = index.load_metas()?;
    let Some(payload) = meta.payload else {
        return Ok(None);
    };
    let manifest: Manifest = serde_json::from_str(&payload)?;
    let expected = schema();
    if !manifest.complete
        || manifest.format != 1
        || manifest.schema_hash != fingerprint(&expected)
        || meta.schema != expected
    {
        return Ok(None);
    }
    Ok(Some(manifest))
}

#[cfg(test)]
#[path = "lexical_generation_tests.rs"]
mod tests;
