//! Opt-in volatile hook queue. Only bounded counters reach disk; grants are fetched afresh.
use crate::hook_credentials::{ancestors, owner, private_file};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{OwnedSemaphorePermit, Semaphore, mpsc},
};

const EVENTS: usize = 64;
const BYTES: usize = 1_048_576;
const ENQUEUE: Duration = Duration::from_millis(150);
const REQUEST: Duration = Duration::from_secs(2);
const IDLE: Duration = Duration::from_secs(30);
const DRAIN: Duration = Duration::from_secs(10);
const TOKEN_ENV: &str = "HYPERAG_HOOK_WRITER_TOKEN";
const URL_ENV: &str = "HYPERAG_HOOK_WRITER_URL";
const MARKER_ENV: &str = "HYPERAG_HOOK_WRITER_MARKER";

#[derive(Clone, Copy, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
enum Category {
    LostQueue,
    Http401,
    Http403,
    Backend,
    Transport,
    QueueFull,
    Cancelled,
    DrainDeadline,
    IpcRejected,
    EnqueueTimeout,
    EnqueueIo,
    Startup,
    BackgroundFailure,
    AckFailure,
    SkippedUngranted,
    WriterFailure,
}
#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct State {
    pid: u32,
    updated_ms: u128,
    accepted: u64,
    stored: u64,
    failed: u64,
    cancelled: u64,
    rejected: u64,
    blocked: bool,
    stopped: bool,
    last_category: Option<Category>,
    categories: BTreeMap<Category, u64>,
}
impl State {
    fn record(&mut self, category: Category, count: u64) {
        *self.categories.entry(category).or_default() += count;
        self.last_category = Some(category);
    }
    fn pending(&self) -> u64 {
        self.accepted
            .saturating_sub(self.stored)
            .saturating_sub(self.failed)
    }
}
#[derive(Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
struct EnqueueErrors {
    rejected: u64,
    skipped: u64,
    categories: BTreeMap<Category, u64>,
}
struct Item {
    body: Vec<u8>,
    _event: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}
type Shared = Arc<Mutex<State>>;

fn private_dir(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.uid() == owner() && metadata.mode() & 0o777 == 0o700 && metadata.is_dir(),
        "unsafe async directory owner, mode or type"
    );
    Ok(())
}

fn mkdir(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(error) => return Err(error.into()),
    }
    private_dir(path)?;
    Ok(())
}

fn lock(path: &Path) -> Result<File> {
    if let Err(error) = fs::symlink_metadata(path) {
        ensure!(
            error.kind() == std::io::ErrorKind::NotFound,
            "hook lock unavailable"
        );
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
            Err(error) => return Err(error.into()),
        }
    }
    let before = private_file(path)?;
    let file = OpenOptions::new().read(true).write(true).open(path)?;
    let opened = file.metadata()?;
    let after = private_file(path)?;
    ensure!(
        (before.dev(), before.ino()) == (opened.dev(), opened.ino())
            && (after.dev(), after.ino()) == (opened.dev(), opened.ino()),
        "hook lock changed while opening"
    );
    Ok(file)
}

fn save<T: Serialize>(root: &Path, path: &Path, value: &T) -> Result<()> {
    if fs::symlink_metadata(path).is_ok() {
        private_file(path)?;
    }
    let mut file = tempfile::NamedTempFile::new_in(root)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.as_file().sync_all()?;
    file.persist(path)
        .map_err(|_| anyhow!("hook metadata save failed"))?;
    Ok(())
}

fn key(base: &str, token: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(base.trim_end_matches('/').as_bytes());
    hash.update([0]);
    hash.update(token.as_bytes());
    hex::encode(&hash.finalize()[..16])
}

fn now_ms() -> Result<u128> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis())
}

fn counter_file<T: for<'a> Deserialize<'a>>(path: &Path, file: File) -> Result<T> {
    private_file(path)?;
    let opened = file.metadata()?;
    ensure!(
        opened.is_file() && opened.uid() == owner() && opened.mode() & 0o777 == 0o600,
        "unsafe async counter snapshot"
    );
    // Counters are owned atomic snapshots, unlike credentials: an old valid FD remains
    // coherent when a concurrent writer replaces its trusted pathname with a new snapshot.
    let mut bytes = Vec::new();
    file.take(4_097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4_096, "async counter metadata too large");
    serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid async counter metadata"))
}
fn counter_snapshot<T: for<'a> Deserialize<'a>>(path: &Path) -> Result<Option<T>> {
    match private_file(path) {
        Ok(_) => (),
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    }
    counter_file(path, File::open(path)?).map(Some)
}
fn state(root: &Path) -> Result<State> {
    Ok(counter_snapshot(&root.join("status.json"))?.unwrap_or_default())
}
fn change(root: &Path, shared: &Shared, update: impl FnOnce(&mut State)) -> Result<()> {
    let mut state = shared
        .lock()
        .map_err(|_| anyhow!("async state unavailable"))?;
    let mut next = state.clone();
    update(&mut next);
    next.updated_ms = now_ms()?;
    save(root, &root.join("status.json"), &next)?;
    *state = next;
    Ok(())
}
fn root(base: &str, token: &str) -> Result<PathBuf> {
    let home = crate::hook_setup::home()?.canonicalize()?;
    ancestors(&home)?;
    ensure!(
        fs::metadata(&home)?.uid() == owner(),
        "unsafe hook home owner"
    );
    let parent = home.join(".hyperag-hooks");
    mkdir(&parent)?;
    let root = parent.join(key(base, token));
    mkdir(&root)?;
    ensure!(
        root.join("q").as_os_str().as_bytes().len() <= 100,
        "async socket path too long"
    );
    Ok(root)
}

fn socket_remove(root: &Path) -> Result<()> {
    let path = root.join("q");
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
        Ok(metadata) => ensure!(
            metadata.file_type().is_socket() && metadata.uid() == owner(),
            "unsafe async socket type or owner"
        ),
    }
    fs::remove_file(path)?;
    Ok(())
}
fn peer(stream: &UnixStream) -> Result<()> {
    ensure!(stream.peer_cred()?.uid() == owner(), "async peer rejected");
    Ok(())
}
pub fn inspect(base: &str, token: &str) -> Result<Value> {
    let root = root(base, token)?;
    let startup = lock(&root.join("startup.lock"))?;
    if startup.try_lock().is_ok() {
        let worker = lock(&root.join("worker.lock"))?;
        if worker.try_lock().is_ok() {
            let mut previous = state(&root)?;
            let pending = previous.pending();
            if pending > 0 {
                previous.failed += pending;
                previous.blocked = true;
                previous.updated_ms = now_ms()?;
                previous.record(Category::LostQueue, pending);
                save(&root, &root.join("status.json"), &previous)?;
            }
        }
    }
    let mut value = serde_json::to_value(state(&root)?)?;
    let errors: EnqueueErrors =
        counter_snapshot(&root.join("enqueue-errors.json"))?.unwrap_or_default();
    value["enqueue_errors"] = serde_json::to_value(errors)?;
    Ok(value)
}

async fn connection(
    root: &Path,
    base: &str,
    token: &str,
    marker: Option<&Path>,
) -> Result<UnixStream> {
    if state(root)?.blocked {
        // A watchdog or another enqueue must classify a previously blocked worker's death too.
        inspect(base, token)?;
        anyhow::bail!("async writer background failure");
    }
    if let Ok(stream) = UnixStream::connect(root.join("q")).await {
        peer(&stream)?;
        return Ok(stream);
    }
    let startup = lock(&root.join("startup.lock"))?;
    let mut owns_startup = false;
    let mut spawned = false;
    loop {
        if let Ok(stream) = UnixStream::connect(root.join("q")).await {
            peer(&stream)?;
            return Ok(stream);
        }
        if !owns_startup {
            owns_startup = startup.try_lock().is_ok();
        }
        if owns_startup && !spawned {
            // A clean idle exit can release this lock after our first failed connect.
            let probe = lock(&root.join("worker.lock"))?;
            if probe.try_lock().is_ok() {
                let mut previous = state(root)?;
                if previous.pending() > 0 {
                    let pending = previous.pending();
                    previous.failed += pending;
                    previous.blocked = true;
                    previous.updated_ms = now_ms()?;
                    previous.record(Category::LostQueue, pending);
                    save(root, &root.join("status.json"), &previous)?;
                    anyhow::bail!("async lost_queue");
                }
                ensure!(!previous.blocked, "async writer background failure");
                socket_remove(root)?;
                drop(probe);
                let mut command = Command::new(std::env::current_exe()?);
                command
                    .args([std::ffi::OsStr::new("hook-writer"), root.as_os_str()])
                    .env_clear()
                    .env(TOKEN_ENV, token)
                    .env(URL_ENV, base)
                    .env("TOKIO_WORKER_THREADS", "1")
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .process_group(0);
                if let Some(marker) = marker {
                    command.env(MARKER_ENV, marker);
                }
                command
                    .spawn()
                    .map_err(|_| anyhow!("async worker startup failed"))?;
                spawned = true;
            }
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}
async fn enqueue_error(root: &Path, category: Category) -> Result<()> {
    let file = lock(&root.join("errors.lock"))?;
    tokio::time::timeout(Duration::from_millis(25), async {
        while file.try_lock().is_err() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let mut errors: EnqueueErrors =
            counter_snapshot(&root.join("enqueue-errors.json"))?.unwrap_or_default();
        if category == Category::SkippedUngranted {
            errors.skipped += 1;
        } else {
            errors.rejected += 1;
        }
        *errors.categories.entry(category).or_default() += 1;
        save(root, &root.join("enqueue-errors.json"), &errors)
    })
    .await
    .map_err(|_| anyhow!("async error accounting unavailable"))?
}

pub async fn skipped_scope(base: &str, token: &str) -> Result<()> {
    enqueue_error(&root(base, token)?, Category::SkippedUngranted).await
}

pub async fn enqueue(base: &str, token: &str, body: Value, marker: Option<&Path>) -> Result<()> {
    let root = root(base, token)?;
    if marker.is_some_and(|path| !path.exists()) {
        enqueue_error(&root, Category::Cancelled).await?;
        return Ok(());
    }
    let bytes = serde_json::to_vec(&body)?;
    if bytes.is_empty() || bytes.len() > BYTES {
        enqueue_error(&root, Category::IpcRejected).await?;
        anyhow::bail!("async queue item too large");
    }
    let mut category = Category::Startup;
    let result = tokio::time::timeout(ENQUEUE, async {
        let mut stream = connection(&root, base, token, marker).await?;
        category = Category::EnqueueIo;
        stream.write_u32(bytes.len() as u32).await?;
        match stream.read_u8().await? {
            0 => (),
            1 => {
                category = Category::QueueFull;
                anyhow::bail!("async queue_full");
            }
            _ => {
                category = Category::BackgroundFailure;
                anyhow::bail!("async writer rejected enqueue");
            }
        }
        stream.write_all(&bytes).await?;
        ensure!(
            stream.read_u8().await? == 0,
            "async enqueue acknowledgement invalid"
        );
        Ok(())
    })
    .await;
    let error = match result {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(error)) => error,
        Err(_) => {
            category = Category::EnqueueTimeout;
            anyhow!("async enqueue timed out")
        }
    };
    if error.to_string() == "async lost_queue" {
        category = Category::LostQueue;
    }
    if error.to_string() == "async writer background failure" {
        category = Category::BackgroundFailure;
    }
    enqueue_error(&root, category).await?;
    Err(error)
}

async fn accept(
    stream: &mut UnixStream,
    sender: &mpsc::Sender<Item>,
    events: &Arc<Semaphore>,
    bytes: &Arc<Semaphore>,
    root: &Path,
    shared: &Shared,
) -> Result<()> {
    peer(stream)?;
    let size = stream.read_u32().await? as usize;
    ensure!(size > 0 && size <= BYTES, "async queue frame invalid");
    let blocked = shared
        .lock()
        .map_err(|_| anyhow!("async state unavailable"))?
        .blocked;
    if blocked {
        stream.write_u8(2).await?;
        return Ok(());
    }
    // Reserve both budgets before allocating or reading a client-controlled body.
    let event = events.clone().try_acquire_owned();
    let credit = bytes.clone().try_acquire_many_owned(size as u32);
    if event.is_err() || credit.is_err() {
        change(root, shared, |state| {
            state.rejected += 1;
            state.record(Category::QueueFull, 1);
        })?;
        stream.write_u8(1).await?;
        return Ok(());
    }
    stream.write_u8(0).await?; // Credit only: the final ACK below confirms enqueue.
    let mut body = vec![0; size];
    stream.read_exact(&mut body).await?;
    let value: Value =
        serde_json::from_slice(&body).map_err(|_| anyhow!("async queue frame invalid"))?;
    ensure!(
        value["scope"]
            .as_str()
            .is_some_and(|scope| scope.len() <= 1_024),
        "async queue scope invalid"
    );
    admit(
        root,
        shared,
        sender,
        Item {
            body,
            _event: event?,
            _bytes: credit?,
        },
    )?;
    if stream.write_u8(0).await.is_err() {
        // The item is accepted; an ACK failure must not mark the write itself failed.
        change(root, shared, |state| state.record(Category::AckFailure, 1))?;
    }
    Ok(())
}
fn admit(root: &Path, shared: &Shared, sender: &mpsc::Sender<Item>, item: Item) -> Result<()> {
    let slot = sender
        .try_reserve()
        .map_err(|_| anyhow!("async writer unavailable"))?;
    // Publish durable accepted accounting before the writer can observe this item.
    change(root, shared, |state| state.accepted += 1)?;
    slot.send(item);
    Ok(())
}

async fn writes(
    mut receiver: mpsc::Receiver<Item>,
    root: PathBuf,
    shared: Shared,
    base: String,
    token: String,
    marker: Option<PathBuf>,
) -> Result<()> {
    let http = reqwest::Client::builder().timeout(REQUEST).build()?;
    while let Some(item) = receiver.recv().await {
        let cancelled = marker.as_ref().is_some_and(|path| !path.exists());
        let blocked = shared
            .lock()
            .map_err(|_| anyhow!("async state unavailable"))?
            .blocked;
        if cancelled || blocked {
            change(&root, &shared, |state| {
                state.failed += 1;
                state.cancelled += 1;
                state.record(Category::Cancelled, 1);
            })?;
            continue;
        }
        let result = http
            .post(format!("{}/v1/memories", base.trim_end_matches('/')))
            .bearer_auth(&token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(item.body)
            .send()
            .await;
        let failure = match result {
            Ok(response) if response.status().is_success() => None,
            Ok(response) if response.status().as_u16() == 401 => Some(Category::Http401),
            Ok(response) if response.status().as_u16() == 403 => Some(Category::Http403),
            Ok(_) => Some(Category::Backend),
            Err(_) => Some(Category::Transport),
        };
        change(&root, &shared, |state| {
            if let Some(category) = failure {
                state.failed += 1;
                state.blocked = true;
                state.record(category, 1);
            } else {
                state.stored += 1;
            }
        })?;
    }
    Ok(())
}

pub async fn run(root: &Path) -> Result<()> {
    ancestors(root)?;
    private_dir(root)?;
    let token = std::env::var(TOKEN_ENV).map_err(|_| anyhow!("missing async token"))?;
    let base = std::env::var(URL_ENV).map_err(|_| anyhow!("missing async endpoint"))?;
    ensure!(
        root.file_name()
            .is_some_and(|name| name == key(&base, &token).as_str()),
        "async queue identity mismatch"
    );
    let marker = std::env::var_os(MARKER_ENV).map(PathBuf::from);
    let worker = lock(&root.join("worker.lock"))?;
    if worker.try_lock().is_err() {
        return Ok(());
    }
    let mut initial = state(root)?;
    ensure!(
        !initial.blocked && initial.pending() == 0,
        "async queue requires owner attention"
    );
    initial.pid = std::process::id();
    initial.updated_ms = now_ms()?;
    initial.stopped = false;
    save(root, &root.join("status.json"), &initial)?;
    let shared = Arc::new(Mutex::new(initial));
    socket_remove(root)?;
    let listener = UnixListener::bind(root.join("q"))?;
    fs::set_permissions(root.join("q"), fs::Permissions::from_mode(0o600))?;
    let events = Arc::new(Semaphore::new(EVENTS));
    let bytes = Arc::new(Semaphore::new(BYTES));
    let (sender, receiver) = mpsc::channel(EVENTS);
    let mut writer = tokio::spawn(writes(
        receiver,
        root.to_path_buf(),
        shared.clone(),
        base,
        token,
        marker.clone(),
    ));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut idle = tokio::time::Instant::now() + IDLE;
    loop {
        tokio::select! {
            _ = terminate.recv() => break,
            _ = tokio::time::sleep_until(idle), if events.available_permits() == EVENTS => break,
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                if writer.is_finished() || marker.as_ref().is_some_and(|path| !path.exists()) || shared.lock().map_err(|_| anyhow!("async state unavailable"))?.blocked { break; }
            }
            accepted = listener.accept() => {
                let (mut stream, _) = accepted?;
                idle = tokio::time::Instant::now() + IDLE;
                // One bounded reader avoids an unbounded task per connected local process.
                let rejection = match tokio::time::timeout(ENQUEUE, accept(&mut stream, &sender, &events, &bytes, root, &shared)).await {
                    Ok(Ok(())) => None, Ok(Err(_)) => Some(Category::IpcRejected), Err(_) => Some(Category::EnqueueTimeout),
                };
                if let Some(category) = rejection {
                    change(root, &shared, |state| { state.rejected += 1; state.record(category, 1); })?;
                }
            }
        }
    }
    drop(listener);
    socket_remove(root)?;
    drop(sender);
    let drain = tokio::time::timeout(DRAIN, &mut writer).await;
    let expired = drain.is_err();
    if !matches!(drain, Ok(Ok(Ok(())))) {
        if expired {
            writer.abort();
            let _ = writer.await;
        }
        change(root, &shared, |state| {
            let pending = state.pending();
            state.failed += pending;
            state.blocked = true;
            state.record(
                if expired {
                    Category::DrainDeadline
                } else {
                    Category::WriterFailure
                },
                pending,
            );
        })?;
    }
    change(root, &shared, |state| state.stopped = true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counter_snapshot_keeps_a_coherent_old_fd_across_atomic_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("status.json");
        save(
            root.path(),
            &path,
            &State {
                accepted: 1,
                stored: 1,
                ..State::default()
            },
        )
        .unwrap();
        private_file(&path).unwrap();
        let old = File::open(&path).unwrap();
        save(
            root.path(),
            &path,
            &State {
                accepted: 2,
                stored: 2,
                ..State::default()
            },
        )
        .unwrap();
        let previous: State = counter_file(&path, old).unwrap();
        assert_eq!((previous.accepted, previous.stored), (1, 1));
        let current: State = counter_snapshot(&path).unwrap().unwrap();
        assert_eq!((current.accepted, current.stored), (2, 2));
    }
    #[test]
    fn counter_snapshot_rejects_unsafe_open_fd_even_after_safe_path_replacement() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("status.json");
        save(root.path(), &path, &State::default()).unwrap();
        private_file(&path).unwrap();
        let old = File::open(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let replacement = tempfile::NamedTempFile::new_in(root.path()).unwrap();
        replacement.persist(&path).unwrap();
        private_file(&path).unwrap();
        assert!(counter_file::<State>(&path, old).is_err());
    }
    #[test]
    fn failed_accepted_save_never_publishes_item_or_consumes_budget() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("status.json")).unwrap();
        let shared = Arc::new(Mutex::new(State::default()));
        let (sender, mut receiver) = mpsc::channel(EVENTS);
        let events = Arc::new(Semaphore::new(EVENTS));
        let bytes = Arc::new(Semaphore::new(BYTES));
        let item = Item {
            body: b"{}".to_vec(),
            _event: events.clone().try_acquire_owned().unwrap(),
            _bytes: bytes.clone().try_acquire_many_owned(2).unwrap(),
        };
        assert!(admit(root.path(), &shared, &sender, item).is_err());
        assert!(matches!(
            receiver.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(shared.lock().unwrap().accepted, 0);
        assert_eq!(events.available_permits(), EVENTS);
        assert_eq!(bytes.available_permits(), BYTES);
    }
}
