//! Clipboard bridge between the OS pasteboard and the CLIPRDR virtual channel.
//!
//! The headline requirement is that this bridge must never wedge: Microsoft's own client
//! is known to have the clipboard "stop working randomly after a bit" on long sessions, and
//! that is exactly the failure mode this module is built to rule out.
//!
//! # Why two structs
//!
//! [`ironrdp_cliprdr::backend::CliprdrBackend`] callbacks take `&mut self` and return `()` —
//! they have no access to the network transport, so they cannot themselves cause a PDU to be
//! sent. [`ClipboardBackend`] therefore does almost nothing: every callback just forwards the
//! raw event as a [`ClipboardAction`] over an `mpsc` channel and returns immediately. It must
//! never block, never do I/O that can hang, and never panic.
//!
//! [`ClipboardBridge`] is the session-side half that owns the channel's receiver and the entire
//! protocol state machine (which formats the remote last offered, whether we're waiting on a
//! paste response, and whether our last advertise was accepted). OS clipboard calls and the
//! potentially large encode/decode operations run on one serialized worker instead. The
//! session loop only submits bounded work and drains bounded results, so a slow pasteboard can
//! never hold the graphics/input thread.
//!
//! The worker owns the OS handle and its echo-suppression fingerprint. It never sees a live
//! `Cliprdr`, socket, or session reference. Every result carries an epoch and operation id; the
//! bridge drops results from an older lifecycle or superseded request before they can mutate
//! protocol state. The inline executor used by [`clipboard_channel_with_clock`] calls the same
//! worker operation function synchronously, keeping the deterministic unit tests fast and
//! faithful to production behaviour.
//!
//! Text is preferred when present. Otherwise the bridge uses standard uncompressed `CF_DIB`
//! bitmap data, with strict size checks around every decode and encode operation.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::wake::Doorbell;
use ironrdp_cliprdr::backend::CliprdrBackend;
use ironrdp_cliprdr::pdu::{
    ClipboardFileAttributes, ClipboardFormat, ClipboardFormatId, ClipboardFormatName,
    ClipboardGeneralCapabilityFlags, FORMAT_NAME_FILE_LIST, FileContentsFlags, FileContentsRequest,
    FileContentsResponse, FileDescriptor, FormatDataRequest, FormatDataResponse, LockDataId,
    OwnedFileContentsResponse, OwnedFormatDataResponse,
};
use ironrdp_cliprdr::{Cliprdr, CliprdrSvcMessages, Role};
use ironrdp_svc::pdu::IntoOwned as _;
use ironrdp_svc::pdu::ironrdp_core::AsAny;
use sha2::{Digest, Sha256};
use tracing::{debug, trace, warn};

/// How long we'll wait for the remote to answer a paste request before giving up and
/// returning to `Idle`. A stuck "pending" flag with no way back is the classic wedge.
pub const DEFAULT_PASTE_TIMEOUT_MS: u64 = 5_000;

/// Session policy for clipboard sharing, from Settings ▸ Clipboard.
///
/// The direction gates suppress whole flows rather than filtering content: with
/// `to_remote` off the local clipboard is never announced, and with `from_remote`
/// off remote announcements are never followed up. `max_image_bytes` tightens (never
/// widens) the built-in safety ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClipboardPolicy {
    pub to_remote: bool,
    pub from_remote: bool,
    pub max_image_bytes: u64,
    pub paste_timeout_ms: u64,
}

impl Default for ClipboardPolicy {
    fn default() -> Self {
        ClipboardPolicy {
            to_remote: true,
            from_remote: true,
            max_image_bytes: MAX_IMAGE_BYTES as u64,
            paste_timeout_ms: DEFAULT_PASTE_TIMEOUT_MS,
        }
    }
}

/// Keep malformed remote clipboard data from forcing a large allocation or expensive decode.
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;
const MAX_IMAGE_BYTES: usize = 256 * 1024 * 1024;

/// Bounded retries for a rejected format-list advertise (MS-RDPECLIP allows the remote to
/// reject transiently, e.g. its window wasn't focused). Small and bounded on purpose: see
/// the hazard documented at `CliprdrBackend::on_format_list_response`.
const MAX_ADVERTISE_ATTEMPTS: u8 = 3;

/// Cap on how many bytes of clipboard content [`content_fingerprint`] will hash.
///
/// Text at or under this size is fingerprinted exactly as before (every byte hashed) — no
/// behaviour change for realistic clipboard content: a URL, a paragraph, a code snippet,
/// even a sizeable spreadsheet selection. Above the cap, only the first
/// `HASH_PREFIX_CAP_BYTES` bytes are hashed and the *full* byte length is folded into the
/// same digest, so the cost of every poll is bounded by this constant no matter how large
/// the payload on the clipboard gets.
///
/// At SHA-256's ~1-2 GB/s (the figure that made the original bug real: a 100 MB payload
/// cost 50-100 ms per poll), 256 KiB costs roughly 0.1-0.3 ms — negligible next to the
/// 250 ms poll cadence and the session pump's 5 ms read slice (`session::READ_SLICE`).
const HASH_PREFIX_CAP_BYTES: usize = 256 * 1024;

/// Maximum number of clipboard actions and worker results handled by one session turn.
///
/// Protocol callbacks remain queued in FIFO order, but one noisy peer cannot keep the session
/// from returning to graphics/input processing. The worker's command queue is smaller still:
/// one in-flight or queued operation is enough because all OS access is serialized.
pub const MAX_CLIPBOARD_ACTIONS_PER_PUMP: usize = 32;
const MAX_CLIPBOARD_ACTIONS_BEFORE_RESULT: usize = 8;
const WORKER_COMMAND_QUEUE_DEPTH: usize = 1;
// One result can itself contain the full image ceiling. Count backpressure therefore stays
// one-deep too; a 32-entry result queue could otherwise retain gigabytes of image responses.
const WORKER_RESULT_QUEUE_DEPTH: usize = 1;
const WORKER_SHUTDOWN_WAIT: Duration = Duration::from_millis(100);

/// File transfer limits are deliberately conservative. The protocol carries attacker
/// controlled counts, names, sizes, offsets, and stream IDs, so every one is checked before
/// it can allocate, create, or write anything.
const MAX_FILE_COUNT: usize = 256;
const MAX_FILE_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
const MAX_FILE_CHUNK_BYTES: u32 = 1024 * 1024;
// IronRDP bounds unanswered file requests at 1000. Keep every canceled stream ID until its
// response or timeout arrives, so a late response can never collide with a newer transfer.
const MAX_STALE_FILE_STREAM_IDS: usize = 1000;
const FILE_TRANSFER_TIMEOUT_MS: u64 = 60_000;
const STAGING_MIN_AGE: Duration = Duration::from_secs(60 * 60);
const STAGING_LOCK_ATTEMPTS: usize = 32;
const STAGING_PARENT_NAME: &str = "mdrdp-clipboard";
const STAGING_ROOT_PREFIX: &str = "session-";

/// The session presents this class of failures to the user. Other malformed input stays in
/// diagnostics because it does not tell the user what action to take and must not become a
/// toast flood from an untrusted peer.
fn is_file_limit_error(error: &str) -> bool {
    error.contains("file count exceeds")
        || error.contains("files exceed the total size limit")
        || error.contains("file size exceeds the safety limit")
        || error.contains("file sizes overflow")
}

static NEXT_STAGING_ID: AtomicU64 = AtomicU64::new(1);

/// Raw clipboard events forwarded from [`ClipboardBackend`] to [`ClipboardBridge`].
///
/// Each variant mirrors a [`CliprdrBackend`] callback (or, for [`Self::AdvertiseRequested`],
/// a local-clipboard-change detected by [`ClipboardBridge::poll_local_change`]). The bridge
/// interprets these with full access to the clipboard state machine and the live
/// [`Cliprdr`] instance; the backend that produces them does no interpretation at all.
pub enum ClipboardAction {
    /// We should (re-)advertise our current clipboard formats to the remote.
    AdvertiseRequested,
    /// IronRDP received Monitor Ready and now requires the initialization format list.
    ProtocolFormatListRequested,
    /// The remote acknowledged or rejected our last format-list advertise.
    FormatListAcked(bool),
    /// The remote's clipboard changed; these are the formats it now offers.
    RemoteCopy(Vec<ClipboardFormat>),
    /// The remote wants our clipboard content in this format.
    LocalDataRequested(ClipboardFormatId),
    /// The remote sent us clipboard content (or an explicit error) for a paste we requested.
    RemoteDataReceived(OwnedFormatDataResponse),
    /// The remote sent us a parsed FileGroupDescriptorW list for an eager download.
    RemoteFileList {
        files: Vec<FileDescriptor>,
        clip_data_id: Option<u32>,
    },
    /// The remote wants bytes from one of our advertised local files.
    LocalFileContentsRequested(FileContentsRequest),
    /// The remote sent bytes for one of our eager file downloads.
    RemoteFileContentsReceived(OwnedFileContentsResponse),
    /// The server and client completed capability negotiation.
    NegotiatedCapabilities(ClipboardGeneralCapabilityFlags),
    /// Incoming lock callbacks protect a local file-list snapshot.
    Lock(LockDataId),
    Unlock(LockDataId),
    /// IronRDP's outgoing lock lifecycle protects an eager remote download.
    OutgoingLocksExpired(Vec<LockDataId>),
    OutgoingLocksCleared(Vec<LockDataId>),
}

impl fmt::Debug for ClipboardAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AdvertiseRequested => formatter.write_str("AdvertiseRequested"),
            Self::ProtocolFormatListRequested => formatter.write_str("ProtocolFormatListRequested"),
            Self::FormatListAcked(ok) => {
                formatter.debug_tuple("FormatListAcked").field(ok).finish()
            }
            Self::RemoteCopy(formats) => formatter
                .debug_struct("RemoteCopy")
                .field("format_count", &formats.len())
                .finish(),
            Self::LocalDataRequested(format) => formatter
                .debug_tuple("LocalDataRequested")
                .field(format)
                .finish(),
            Self::RemoteDataReceived(response) => formatter
                .debug_struct("RemoteDataReceived")
                .field("is_error", &response.is_error())
                .field("data_len", &response.data().len())
                .finish(),
            Self::RemoteFileList {
                files,
                clip_data_id,
            } => formatter
                .debug_struct("RemoteFileList")
                .field("file_count", &files.len())
                .field("clip_data_id", clip_data_id)
                .finish(),
            Self::LocalFileContentsRequested(request) => formatter
                .debug_struct("LocalFileContentsRequested")
                .field("stream_id", &request.stream_id)
                .field("index", &request.index)
                .field("flags", &request.flags)
                .field("position", &request.position)
                .field("requested_size", &request.requested_size)
                .finish(),
            Self::RemoteFileContentsReceived(response) => formatter
                .debug_struct("RemoteFileContentsReceived")
                .field("stream_id", &response.stream_id())
                .field("is_error", &response.is_error())
                .field("data_len", &response.data().len())
                .finish(),
            Self::NegotiatedCapabilities(capabilities) => formatter
                .debug_tuple("NegotiatedCapabilities")
                .field(capabilities)
                .finish(),
            Self::Lock(id) => formatter.debug_tuple("Lock").field(id).finish(),
            Self::Unlock(id) => formatter.debug_tuple("Unlock").field(id).finish(),
            Self::OutgoingLocksExpired(ids) => formatter
                .debug_struct("OutgoingLocksExpired")
                .field("count", &ids.len())
                .finish(),
            Self::OutgoingLocksCleared(ids) => formatter
                .debug_struct("OutgoingLocksCleared")
                .field("count", &ids.len())
                .finish(),
        }
    }
}

/// Content visible through the OS clipboard. The image bytes are RGBA, row-major, top-down.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClipboardContent {
    Text(String),
    Image {
        width: usize,
        height: usize,
        rgba: Vec<u8>,
    },
    Files(Vec<PathBuf>),
}

/// Stable metadata captured while a local clipboard generation is current. The path is opened
/// only when the peer asks for a bounded SIZE/RANGE response, and the identity is checked again
/// immediately before bytes leave the process.
#[derive(Clone, PartialEq, Eq)]
struct FileIdentity {
    len: u64,
    modified: Option<SystemTime>,
    is_dir: bool,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(windows)]
    creation_time: u64,
    #[cfg(windows)]
    last_write_time: u64,
}

#[derive(Clone)]
struct LocalFileEntry {
    path: PathBuf,
    descriptor: FileDescriptor,
    identity: FileIdentity,
    is_dir: bool,
}

#[derive(Clone)]
struct LocalFileSnapshot {
    entries: Vec<LocalFileEntry>,
}

struct LocalClipboardSnapshot {
    formats: Vec<ClipboardFormat>,
    files: Option<LocalFileSnapshot>,
}

struct RemotePreparedEntry {
    index: usize,
    is_dir: bool,
    declared_size: Option<u64>,
}

struct RemoteStageEntry {
    path: PathBuf,
    is_dir: bool,
    declared_size: Option<u64>,
    size: Option<u64>,
    received: u64,
}

struct RemoteStage {
    root: PathBuf,
    entries: Vec<RemoteStageEntry>,
}

/// Per-process private staging layout. Each process gets its own root; cleanup takes an
/// atomic lock in the shared parent and checks the current OS clipboard while holding it, so
/// one live session cannot remove another session's still-published paths.
#[derive(Clone)]
struct StagingLayout {
    parent: PathBuf,
    root: PathBuf,
    temporary_directory: String,
}

struct StagingLock {
    file: File,
}

impl Drop for StagingLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[cfg(windows)]
fn path_references_stage(reference: &Path, stage: &Path) -> bool {
    let mut reference_components = reference.components();
    stage.components().all(|stage_component| {
        reference_components
            .next()
            .is_some_and(|reference_component| {
                reference_component
                    .as_os_str()
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&stage_component.as_os_str().to_string_lossy())
            })
    })
}

#[cfg(not(windows))]
fn path_references_stage(reference: &Path, stage: &Path) -> bool {
    reference.starts_with(stage)
}

impl StagingLayout {
    fn new() -> Self {
        Self::new_in(std::env::temp_dir().join(STAGING_PARENT_NAME))
    }

    fn new_in(parent: PathBuf) -> Self {
        fs::create_dir_all(&parent).expect("create clipboard staging parent");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&parent, fs::Permissions::from_mode(0o700));
        }
        let id = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
        let mut id = id;
        let root = loop {
            let candidate =
                parent.join(format!("{STAGING_ROOT_PREFIX}{}-{id}", std::process::id()));
            match fs::create_dir(&candidate) {
                Ok(()) => break candidate,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    id = NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed);
                }
                Err(error) => panic!("create clipboard staging root: {error}"),
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&root, fs::Permissions::from_mode(0o700));
        }
        let temporary_directory = root.to_string_lossy().into_owned();
        Self {
            parent,
            root,
            temporary_directory,
        }
    }

    fn temporary_directory(&self) -> &str {
        &self.temporary_directory
    }

    fn acquire_lock(&self) -> Option<StagingLock> {
        let lock_path = self.parent.join(".lock");
        for _ in 0..STAGING_LOCK_ATTEMPTS {
            let file = match OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(&lock_path)
            {
                Ok(file) => file,
                Err(_) => return None,
            };
            match file.try_lock() {
                Ok(()) => return Some(StagingLock { file }),
                Err(std::fs::TryLockError::WouldBlock) => {
                    thread::yield_now();
                }
                Err(_) => return None,
            }
        }
        None
    }

    fn create_remote_stage(
        &self,
        transfer_id: u64,
        files: &[FileDescriptor],
    ) -> Result<RemoteStage, String> {
        let root = self.root.join(format!("transfer-{transfer_id}"));
        if root.exists() {
            return Err("remote clipboard staging transfer already exists".to_string());
        }
        fs::create_dir(&root)
            .map_err(|_| "remote clipboard staging directory could not be created".to_string())?;
        let mut entries = Vec::new();
        let mut seen = HashMap::<String, bool>::new();
        let mut total_known = 0u64;

        if files.is_empty() || files.len() > MAX_FILE_COUNT {
            let _ = fs::remove_dir_all(&root);
            return Err("remote clipboard file count exceeds the safety limit".to_string());
        }

        for (index, descriptor) in files.iter().enumerate() {
            let components = match remote_path_components(descriptor) {
                Ok(components) => components,
                Err(error) => {
                    let _ = fs::remove_dir_all(&root);
                    return Err(error);
                }
            };
            let key = components
                .iter()
                .map(|component| component.to_lowercase())
                .collect::<Vec<_>>()
                .join("/");
            let is_dir = descriptor
                .attributes
                .is_some_and(|attributes| attributes.contains(ClipboardFileAttributes::DIRECTORY));
            if seen.insert(key.clone(), is_dir).is_some() {
                let _ = fs::remove_dir_all(&root);
                return Err("remote clipboard file paths collide".to_string());
            }
            let mut prefix = Vec::new();
            for component in components.iter().take(components.len().saturating_sub(1)) {
                prefix.push(component.to_lowercase());
                if seen.get(&prefix.join("/")).is_some_and(|is_dir| !*is_dir) {
                    let _ = fs::remove_dir_all(&root);
                    return Err("remote clipboard file paths contain a file parent".to_string());
                }
            }

            let declared_size = descriptor.file_size;
            if is_dir {
                if declared_size.is_some_and(|size| size != 0) {
                    let _ = fs::remove_dir_all(&root);
                    return Err("remote clipboard directory has a non-zero size".to_string());
                }
            } else if let Some(size) = declared_size {
                total_known = match total_known.checked_add(size) {
                    Some(total) => total,
                    None => {
                        let _ = fs::remove_dir_all(&root);
                        return Err("remote clipboard file sizes overflow".to_string());
                    }
                };
                if MAX_FILE_TOTAL_BYTES < total_known {
                    let _ = fs::remove_dir_all(&root);
                    return Err("remote clipboard files exceed the total size limit".to_string());
                }
            }
            let path = components
                .iter()
                .fold(root.clone(), |path, component| path.join(component));
            if !path.starts_with(&root) {
                let _ = fs::remove_dir_all(&root);
                return Err("remote clipboard path escaped staging".to_string());
            }
            entries.push(RemoteStageEntry {
                path,
                is_dir,
                declared_size,
                size: if is_dir { Some(0) } else { declared_size },
                received: 0,
            });
            debug_assert_eq!(index, entries.len() - 1);
        }

        // Create every directory first, including implicit parents, then create empty files.
        // No file is exposed to the OS clipboard until all ranges have completed.
        for entry in entries.iter().filter(|entry| entry.is_dir) {
            if fs::create_dir_all(&entry.path).is_err() {
                let _ = fs::remove_dir_all(&root);
                return Err("remote clipboard directory staging failed".to_string());
            }
        }
        for entry in entries.iter().filter(|entry| !entry.is_dir) {
            if let Some(parent) = entry.path.parent()
                && fs::create_dir_all(parent).is_err()
            {
                let _ = fs::remove_dir_all(&root);
                return Err("remote clipboard file parent staging failed".to_string());
            }
            if OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&entry.path)
                .is_err()
            {
                let _ = fs::remove_dir_all(&root);
                return Err("remote clipboard file staging failed".to_string());
            }
        }

        Ok(RemoteStage { root, entries })
    }

    fn cleanup_abandoned(&self, os: &mut dyn OsClipboard) {
        self.cleanup_abandoned_at(os, SystemTime::now());
    }

    fn cleanup_abandoned_at(&self, os: &mut dyn OsClipboard, now: SystemTime) {
        let Some(lock) = self.acquire_lock() else {
            return;
        };
        let referenced = match os.get_file_list() {
            Ok(referenced) => referenced,
            Err(_) => {
                drop(lock);
                return;
            }
        };
        if let Ok(entries) = fs::read_dir(&self.parent) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path == self.root
                    || !path
                        .file_name()
                        .is_some_and(|name| name.to_string_lossy().starts_with(STAGING_ROOT_PREFIX))
                {
                    continue;
                }
                let Ok(metadata) = fs::metadata(&path) else {
                    continue;
                };
                let age = metadata
                    .modified()
                    .ok()
                    .and_then(|modified| now.duration_since(modified).ok());
                if age.is_none_or(|age| age < STAGING_MIN_AGE) {
                    continue;
                }
                if referenced
                    .iter()
                    .any(|reference| path_references_stage(reference, &path))
                {
                    continue;
                }
                let _ = fs::remove_dir_all(&path);
            }
        }
        drop(lock);
    }
}

fn remote_path_components(descriptor: &FileDescriptor) -> Result<Vec<String>, String> {
    let mut components = Vec::new();
    if let Some(relative_path) = &descriptor.relative_path {
        if relative_path.starts_with('/')
            || relative_path.starts_with('\\')
            || relative_path.contains(':')
            || relative_path.contains('\0')
        {
            return Err("remote clipboard path is absolute or malformed".to_string());
        }
        for component in relative_path.split(['/', '\\']) {
            if component.is_empty() {
                return Err("remote clipboard path is absolute or malformed".to_string());
            }
            validate_file_component(component)?;
            components.push(component.to_string());
        }
    }
    validate_file_component(&descriptor.name)?;
    components.push(descriptor.name.clone());
    let wire_len = components
        .iter()
        .map(|component| wire_component_len(component))
        .sum::<usize>()
        .saturating_add(components.len().saturating_sub(1));
    if wire_len > 259 {
        return Err("remote clipboard file path is too long".to_string());
    }
    Ok(components)
}

fn wire_component_len(component: &str) -> usize {
    // FileDescriptor names are UTF-16 on the wire. `str::chars().count()` undercounts
    // supplementary-plane characters, allowing a path to exceed the 260-code-unit field.
    component.encode_utf16().count()
}

fn validate_file_component(component: &str) -> Result<(), String> {
    if component.is_empty()
        || component == "."
        || component == ".."
        || component.ends_with([' ', '.'])
        || component.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '/' | '\\' | ':' | '"' | '<' | '>' | '|' | '?' | '*'
                )
        })
        || wire_component_len(component) > 255
    {
        return Err("remote clipboard file name is invalid".to_string());
    }
    let stem = component
        .trim_end_matches([' ', '.'])
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0')
    {
        return Err("remote clipboard file name is reserved".to_string());
    }
    Ok(())
}

/// One operation sent to the serialized clipboard worker.
///
/// The command carries all policy values needed for that operation instead of sharing mutable
/// policy state with the worker. `Stop` is the only command without a result; its completion is
/// reported through the worker's private done channel so shutdown cannot be blocked by a full
/// result queue.
enum ClipboardWork {
    PollLocal {
        id: u64,
        epoch: u64,
        observation_generation: u64,
        allow_to_remote: bool,
    },
    ReadFormats {
        id: u64,
        epoch: u64,
    },
    ReadLocalData {
        id: u64,
        epoch: u64,
        format: ClipboardFormatId,
        max_image_bytes: usize,
    },
    ReadLocalFile {
        id: u64,
        epoch: u64,
        request: FileContentsRequest,
        entry: LocalFileEntry,
    },
    PrepareRemoteFiles {
        id: u64,
        epoch: u64,
        transfer_id: u64,
        files: Vec<FileDescriptor>,
    },
    StoreRemoteChunk {
        id: u64,
        epoch: u64,
        transfer_id: u64,
        index: usize,
        offset: u64,
        expected_size: u64,
        data: Vec<u8>,
    },
    PublishRemoteFiles {
        id: u64,
        epoch: u64,
        transfer_id: u64,
        sizes: Vec<(usize, u64)>,
    },
    AbortRemoteFiles {
        transfer_id: u64,
    },
    ApplyRemote {
        id: u64,
        epoch: u64,
        observation_generation: u64,
        format: ClipboardFormatId,
        data: Vec<u8>,
        max_image_bytes: usize,
    },
    Stop {
        id: u64,
        epoch: u64,
    },
}

/// A result from [`ClipboardWork`]. IDs and epochs are checked by the bridge before any result
/// changes protocol state. Local poll results carry only the change bit: the bridge requests a
/// fresh format read when it is ready to advertise, so a stale poll cannot advertise stale data.
enum ClipboardResult {
    LocalPoll {
        id: u64,
        epoch: u64,
        changed: bool,
    },
    Formats {
        id: u64,
        epoch: u64,
        result: Result<LocalClipboardSnapshot, String>,
    },
    LocalData {
        id: u64,
        epoch: u64,
        response: OwnedFormatDataResponse,
    },
    LocalFileData {
        id: u64,
        epoch: u64,
        response: OwnedFileContentsResponse,
    },
    RemoteFilesPrepared {
        id: u64,
        epoch: u64,
        transfer_id: u64,
        result: Result<Vec<RemotePreparedEntry>, String>,
    },
    RemoteChunkStored {
        id: u64,
        epoch: u64,
        transfer_id: u64,
        index: usize,
        offset: u64,
        len: usize,
        result: Result<(), String>,
    },
    RemoteFilesPublished {
        id: u64,
        epoch: u64,
        transfer_id: u64,
        result: Result<(), String>,
    },
    RemoteFilesAborted,
    RemoteApplied {
        id: u64,
        epoch: u64,
        result: Result<(), String>,
    },
}

/// Mutable state that belongs exclusively to the worker, or to the inline test executor.
/// Keeping this operation function shared is important: the deterministic tests exercise the
/// same decode, encode, fingerprint, and echo-suppression paths production uses.
struct ClipboardWorkerState {
    os: Box<dyn OsClipboard>,
    last_seen_fingerprint: Option<[u8; 32]>,
    observation_generation: u64,
    active_epoch: Arc<AtomicU64>,
    active_transfer: Arc<AtomicU64>,
    staging: StagingLayout,
    remote_stages: HashMap<u64, RemoteStage>,
}

impl ClipboardWorkerState {
    fn new(
        mut os: Box<dyn OsClipboard>,
        active_epoch: Arc<AtomicU64>,
        active_transfer: Arc<AtomicU64>,
        staging: StagingLayout,
    ) -> Self {
        staging.cleanup_abandoned(&mut *os);
        Self {
            os,
            last_seen_fingerprint: None,
            observation_generation: 1,
            active_epoch,
            active_transfer,
            staging,
            remote_stages: HashMap::new(),
        }
    }

    fn execute(&mut self, work: ClipboardWork) -> ClipboardResult {
        match work {
            ClipboardWork::PollLocal {
                id,
                epoch,
                observation_generation,
                allow_to_remote,
            } => ClipboardResult::LocalPoll {
                id,
                epoch,
                changed: self.poll_local(observation_generation, allow_to_remote),
            },
            ClipboardWork::ReadFormats { id, epoch } => ClipboardResult::Formats {
                id,
                epoch,
                result: read_local_snapshot(&mut *self.os),
            },
            ClipboardWork::ReadLocalData {
                id,
                epoch,
                format,
                max_image_bytes,
            } => ClipboardResult::LocalData {
                id,
                epoch,
                response: read_local_data(&mut *self.os, format, max_image_bytes),
            },
            ClipboardWork::ReadLocalFile {
                id,
                epoch,
                request,
                entry,
            } => ClipboardResult::LocalFileData {
                id,
                epoch,
                response: read_local_file(&request, &entry),
            },
            ClipboardWork::PrepareRemoteFiles {
                id,
                epoch,
                transfer_id,
                files,
            } => {
                let result = self.prepare_remote_files(transfer_id, files);
                let prepared = match result {
                    Ok(stage) => Ok(stage
                        .entries
                        .iter()
                        .enumerate()
                        .map(|(index, entry)| RemotePreparedEntry {
                            index,
                            is_dir: entry.is_dir,
                            declared_size: entry.declared_size,
                        })
                        .collect()),
                    Err(error) => Err(error),
                };
                ClipboardResult::RemoteFilesPrepared {
                    id,
                    epoch,
                    transfer_id,
                    result: prepared,
                }
            }
            ClipboardWork::StoreRemoteChunk {
                id,
                epoch,
                transfer_id,
                index,
                offset,
                expected_size,
                data,
            } => {
                let len = data.len();
                let result =
                    self.store_remote_chunk(transfer_id, index, offset, expected_size, &data);
                ClipboardResult::RemoteChunkStored {
                    id,
                    epoch,
                    transfer_id,
                    index,
                    offset,
                    len,
                    result,
                }
            }
            ClipboardWork::PublishRemoteFiles {
                id,
                epoch,
                transfer_id,
                sizes,
            } => ClipboardResult::RemoteFilesPublished {
                id,
                epoch,
                transfer_id,
                result: self.publish_remote_files(transfer_id, &sizes),
            },
            ClipboardWork::AbortRemoteFiles { transfer_id } => {
                let _ = self.abort_remote_files(transfer_id);
                ClipboardResult::RemoteFilesAborted
            }
            ClipboardWork::ApplyRemote {
                id,
                epoch,
                observation_generation,
                format,
                data,
                max_image_bytes,
            } => {
                if observation_generation != self.observation_generation {
                    self.observation_generation = observation_generation;
                    self.last_seen_fingerprint = None;
                }
                ClipboardResult::RemoteApplied {
                    id,
                    epoch,
                    result: apply_remote_data(
                        &mut *self.os,
                        &mut self.last_seen_fingerprint,
                        &self.active_epoch,
                        epoch,
                        format,
                        &data,
                        max_image_bytes,
                    ),
                }
            }
            ClipboardWork::Stop { .. } => {
                unreachable!("stop is consumed by the worker loop")
            }
        }
    }

    fn prepare_remote_files(
        &mut self,
        transfer_id: u64,
        files: Vec<FileDescriptor>,
    ) -> Result<&RemoteStage, String> {
        if self.active_transfer.load(Ordering::Acquire) != transfer_id {
            return Err("remote clipboard file transfer was cancelled".to_string());
        }
        // The bridge permits only one active remote transfer. A cancellation can race with a
        // queued prepare, so remove any older private stages before retaining the new one.
        let stale_ids: Vec<u64> = self
            .remote_stages
            .keys()
            .copied()
            .filter(|id| *id != transfer_id)
            .collect();
        for stale_id in stale_ids {
            if let Some(stage) = self.remote_stages.remove(&stale_id) {
                self.remove_stage(&stage);
            }
        }
        let stage = self.staging.create_remote_stage(transfer_id, &files)?;
        if self.active_transfer.load(Ordering::Acquire) != transfer_id {
            self.remove_stage(&stage);
            return Err("remote clipboard file transfer was cancelled".to_string());
        }
        self.remote_stages.insert(transfer_id, stage);
        self.remote_stages
            .get(&transfer_id)
            .ok_or_else(|| "remote clipboard staging was not retained".to_string())
    }

    fn store_remote_chunk(
        &mut self,
        transfer_id: u64,
        index: usize,
        offset: u64,
        expected_size: u64,
        data: &[u8],
    ) -> Result<(), String> {
        if self.active_transfer.load(Ordering::Acquire) != transfer_id {
            return Err("remote clipboard file transfer was cancelled".to_string());
        }
        if data.is_empty() || data.len() > MAX_FILE_CHUNK_BYTES as usize {
            return Err("remote clipboard file chunk has an invalid size".to_string());
        }
        if expected_size > MAX_FILE_TOTAL_BYTES {
            return Err("remote clipboard file size exceeds the safety limit".to_string());
        }
        let stage = self
            .remote_stages
            .get_mut(&transfer_id)
            .ok_or_else(|| "remote clipboard file transfer is no longer active".to_string())?;
        let entry = stage
            .entries
            .get_mut(index)
            .ok_or_else(|| "remote clipboard file index is invalid".to_string())?;
        if entry.is_dir || entry.received != offset {
            return Err("remote clipboard file chunk correlation failed".to_string());
        }
        if entry.size.is_none() {
            entry.size = Some(expected_size);
        }
        if entry.size != Some(expected_size) {
            return Err("remote clipboard file chunk correlation failed".to_string());
        }
        let end = offset
            .checked_add(
                u64::try_from(data.len()).map_err(|_| "file chunk is too large".to_string())?,
            )
            .ok_or_else(|| "remote clipboard file range overflowed".to_string())?;
        if expected_size < end {
            return Err("remote clipboard file chunk exceeds the advertised size".to_string());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .open(&entry.path)
            .map_err(|_| "remote clipboard staged file could not be opened".to_string())?;
        file.seek(SeekFrom::Start(offset))
            .map_err(|_| "remote clipboard staged file seek failed".to_string())?;
        file.write_all(data)
            .map_err(|_| "remote clipboard staged file write failed".to_string())?;
        entry.received = end;
        Ok(())
    }

    fn publish_remote_files(
        &mut self,
        transfer_id: u64,
        sizes: &[(usize, u64)],
    ) -> Result<(), String> {
        let stage = self
            .remote_stages
            .remove(&transfer_id)
            .ok_or_else(|| "remote clipboard file transfer is no longer active".to_string())?;
        let mut stage = stage;
        if self.active_transfer.load(Ordering::Acquire) != transfer_id {
            self.remove_stage(&stage);
            return Err("remote clipboard file transfer was cancelled".to_string());
        }
        let mut seen_sizes = HashSet::new();
        let mut total_size = 0u64;
        for &(index, size) in sizes {
            if !seen_sizes.insert(index) {
                self.remove_stage(&stage);
                return Err("remote clipboard file size was duplicated".to_string());
            }
            if size > MAX_FILE_TOTAL_BYTES {
                self.remove_stage(&stage);
                return Err("remote clipboard file size exceeds the safety limit".to_string());
            }
            let Some(entry) = stage.entries.get_mut(index) else {
                self.remove_stage(&stage);
                return Err("remote clipboard file size index is invalid".to_string());
            };
            if entry.is_dir || entry.declared_size.is_some_and(|declared| declared != size) {
                self.remove_stage(&stage);
                return Err("remote clipboard file size correlation failed".to_string());
            }
            total_size = match total_size.checked_add(size) {
                Some(total) if total <= MAX_FILE_TOTAL_BYTES => total,
                _ => {
                    self.remove_stage(&stage);
                    return Err("remote clipboard files exceed the total size limit".to_string());
                }
            };
            entry.size = Some(size);
        }
        if stage
            .entries
            .iter()
            .any(|entry| !entry.is_dir && entry.size != Some(entry.received))
        {
            self.remove_stage(&stage);
            return Err("remote clipboard file transfer is incomplete".to_string());
        }
        let mut top_level = Vec::new();
        let mut seen_top_level = HashSet::new();
        for entry in &stage.entries {
            let Ok(relative) = entry.path.strip_prefix(&stage.root) else {
                self.remove_stage(&stage);
                return Err("remote clipboard staging path escaped its root".to_string());
            };
            let Some(first) = relative.components().next() else {
                self.remove_stage(&stage);
                return Err("remote clipboard staging path is empty".to_string());
            };
            let top = stage.root.join(first.as_os_str());
            if seen_top_level.insert(top.clone()) {
                top_level.push(top);
            }
        }
        if top_level.is_empty() {
            self.remove_stage(&stage);
            return Err("remote clipboard file transfer has no top-level paths".to_string());
        }
        let Some(lock) = self.staging.acquire_lock() else {
            self.remove_stage(&stage);
            return Err("clipboard staging is busy".to_string());
        };
        if self.active_transfer.load(Ordering::Acquire) != transfer_id {
            self.remove_stage(&stage);
            drop(lock);
            return Err("remote clipboard file transfer was cancelled".to_string());
        }
        if let Err(error) = self.os.set_file_list(&top_level) {
            self.remove_stage(&stage);
            drop(lock);
            return Err(error);
        }
        self.last_seen_fingerprint = Some(content_fingerprint(&ClipboardContent::Files(top_level)));
        drop(lock);
        Ok(())
    }

    fn abort_remote_files(&mut self, transfer_id: u64) -> Result<(), String> {
        if let Some(stage) = self.remote_stages.remove(&transfer_id) {
            self.remove_stage(&stage);
        }
        Ok(())
    }

    fn remove_stage(&self, stage: &RemoteStage) {
        // `stage.root` is generated by StagingLayout and never comes from the peer. Keep the
        // check here as a second guard before recursive cleanup of a transfer directory.
        if stage.root.starts_with(&self.staging.root)
            && stage.root != self.staging.root
            && stage
                .root
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("transfer-"))
        {
            let _ = fs::remove_dir_all(&stage.root);
        }
    }

    fn poll_local(&mut self, observation_generation: u64, allow_to_remote: bool) -> bool {
        if !allow_to_remote {
            return false;
        }
        if observation_generation != self.observation_generation {
            self.observation_generation = observation_generation;
            self.last_seen_fingerprint = None;
        }
        let content = match self.os.get_content() {
            Ok(content) => content,
            Err(error) => {
                trace!(%error, "poll_local_change: OS clipboard read failed");
                return false;
            }
        };

        let fingerprint = content_fingerprint(&content);
        if self.last_seen_fingerprint == Some(fingerprint) {
            return false;
        }
        self.last_seen_fingerprint = Some(fingerprint);
        true
    }
}

/// Handle for the production worker. The command queue is deliberately one deep: local polls
/// are advisory and coalesce, while a saturated reliable data request is answered with an error
/// by the bridge rather than waiting on this queue.
struct ClipboardWorkerHandle {
    command_tx: Option<SyncSender<ClipboardWork>>,
    result_rx: Receiver<ClipboardResult>,
    done_rx: Receiver<()>,
    join: Option<JoinHandle<()>>,
}

enum WorkerSubmitOutcome {
    Queued,
    Full(Box<ClipboardWork>),
    Disconnected,
}

impl ClipboardWorkerHandle {
    fn spawn(
        os: Box<dyn OsClipboard>,
        bell: Option<Doorbell>,
        active_epoch: Arc<AtomicU64>,
        active_transfer: Arc<AtomicU64>,
        staging: StagingLayout,
    ) -> Self {
        let (command_tx, command_rx) = mpsc::sync_channel(WORKER_COMMAND_QUEUE_DEPTH);
        let (result_tx, result_rx) = mpsc::sync_channel(WORKER_RESULT_QUEUE_DEPTH);
        let (done_tx, done_rx) = mpsc::sync_channel(1);

        let join = thread::Builder::new()
            .name("mdrdp-clipboard".to_string())
            .spawn(move || {
                let mut state =
                    ClipboardWorkerState::new(os, active_epoch, active_transfer, staging);
                while let Ok(work) = command_rx.recv() {
                    if let ClipboardWork::Stop { id, epoch } = work {
                        let _ = (id, epoch);
                        break;
                    }
                    let result = state.execute(work);
                    if result_tx.send(result).is_err() {
                        break;
                    }
                    if let Some(bell) = &bell {
                        bell.ring();
                    }
                }
                let _ = done_tx.send(());
                if let Some(bell) = &bell {
                    bell.ring();
                }
            })
            .expect("spawn clipboard worker");

        Self {
            command_tx: Some(command_tx),
            result_rx,
            done_rx,
            join: Some(join),
        }
    }

    fn try_submit(&self, work: ClipboardWork) -> WorkerSubmitOutcome {
        let Some(command_tx) = &self.command_tx else {
            return WorkerSubmitOutcome::Disconnected;
        };
        match command_tx.try_send(work) {
            Ok(()) => WorkerSubmitOutcome::Queued,
            Err(TrySendError::Full(work)) => WorkerSubmitOutcome::Full(Box::new(work)),
            Err(TrySendError::Disconnected(_)) => WorkerSubmitOutcome::Disconnected,
        }
    }

    fn try_result(&self) -> Option<ClipboardResult> {
        self.result_rx.try_recv().ok()
    }

    fn shutdown(&mut self, id: u64, epoch: u64) {
        if self.join.is_none() {
            return;
        }

        let deadline = std::time::Instant::now() + WORKER_SHUTDOWN_WAIT;
        let mut stop = ClipboardWork::Stop { id, epoch };
        loop {
            match self.try_submit(stop) {
                WorkerSubmitOutcome::Queued | WorkerSubmitOutcome::Disconnected => break,
                WorkerSubmitOutcome::Full(work) => {
                    if std::time::Instant::now() >= deadline {
                        break;
                    }
                    stop = *work;
                    thread::yield_now();
                }
            }
        }

        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if self.done_rx.recv_timeout(remaining).is_ok() {
            let join = self.join.take().expect("worker join handle present");
            let _ = join.join();
            self.command_tx = None;
        } else {
            // Dropping a JoinHandle detaches. The worker owns only the OS handle and channels;
            // it cannot call back into a session or socket after this bridge is gone.
            let _ = self.join.take();
            self.command_tx = None;
            warn!("clipboard worker did not stop within 100ms; detached");
        }
    }
}

impl Drop for ClipboardWorkerHandle {
    fn drop(&mut self) {
        self.shutdown(u64::MAX, u64::MAX);
    }
}

enum ClipboardExecutor {
    Inline(ClipboardWorkerState),
    Worker(ClipboardWorkerHandle),
}

enum SubmitOutcome {
    Completed(ClipboardResult),
    Queued,
    Full(ClipboardWork),
    Disconnected,
}

impl ClipboardExecutor {
    fn submit(&mut self, work: ClipboardWork) -> SubmitOutcome {
        match self {
            ClipboardExecutor::Inline(state) => SubmitOutcome::Completed(state.execute(work)),
            ClipboardExecutor::Worker(worker) => match worker.try_submit(work) {
                WorkerSubmitOutcome::Queued => SubmitOutcome::Queued,
                WorkerSubmitOutcome::Full(work) => SubmitOutcome::Full(*work),
                WorkerSubmitOutcome::Disconnected => SubmitOutcome::Disconnected,
            },
        }
    }

    fn try_result(&self) -> Option<ClipboardResult> {
        match self {
            ClipboardExecutor::Inline(_) => None,
            ClipboardExecutor::Worker(worker) => worker.try_result(),
        }
    }

    fn shutdown(&mut self, id: u64, epoch: u64) {
        if let ClipboardExecutor::Worker(worker) = self {
            worker.shutdown(id, epoch);
        }
    }
}

/// Abstracts OS clipboard access so the state machine can be tested without touching the
/// real pasteboard.
pub trait OsClipboard: Send {
    /// Reads current clipboard content, preferring text when the platform exposes both.
    fn get_content(&mut self) -> Result<ClipboardContent, String>;
    /// Replaces the clipboard with text, RGBA image, or file-list content.
    fn set_content(&mut self, content: ClipboardContent) -> Result<(), String>;
    /// Reads the native file-list representation when the platform provides one.
    fn get_file_list(&mut self) -> Result<Vec<PathBuf>, String> {
        Err("clipboard file lists are unavailable".to_string())
    }
    /// Replaces the clipboard with a native file-list representation.
    fn set_file_list(&mut self, _paths: &[PathBuf]) -> Result<(), String> {
        Err("clipboard file lists are unavailable".to_string())
    }
}

/// Real [`OsClipboard`] backed by `arboard`.
///
/// The `arboard::Clipboard` handle is created lazily and dropped (to be recreated on next
/// use) whenever an operation errors, so a poisoned handle can never disable the clipboard
/// for the rest of the session.
pub struct ArboardClipboard {
    inner: Option<arboard::Clipboard>,
}

impl ArboardClipboard {
    pub fn new() -> Self {
        Self { inner: None }
    }

    fn ensure(&mut self) -> Result<&mut arboard::Clipboard, String> {
        if self.inner.is_none() {
            let clipboard = arboard::Clipboard::new().map_err(|error| error.to_string())?;
            self.inner = Some(clipboard);
        }
        Ok(self.inner.as_mut().expect("just inserted above"))
    }
}

impl Default for ArboardClipboard {
    fn default() -> Self {
        Self::new()
    }
}

impl OsClipboard for ArboardClipboard {
    fn get_content(&mut self) -> Result<ClipboardContent, String> {
        let clipboard = self.ensure()?;
        match clipboard.get_text() {
            Ok(text) => Ok(ClipboardContent::Text(text)),
            Err(error) => {
                // A non-text clipboard is expected when the user copied an image. Keep the
                // handle alive and try arboard's native image conversion before giving up.
                match clipboard.get_image() {
                    Ok(image) => {
                        let width = image.width;
                        let height = image.height;
                        let rgba = image.bytes.into_owned();
                        if width == 0
                            || height == 0
                            || width > MAX_IMAGE_DIMENSION as usize
                            || height > MAX_IMAGE_DIMENSION as usize
                            || u64::try_from(width)
                                .ok()
                                .and_then(|w| {
                                    u64::try_from(height).ok().and_then(|h| w.checked_mul(h))
                                })
                                .is_none_or(|pixels| pixels > MAX_IMAGE_PIXELS)
                            || rgba.len()
                                != width
                                    .checked_mul(height)
                                    .and_then(|pixels| pixels.checked_mul(4))
                                    .unwrap_or(usize::MAX)
                        {
                            self.inner = None;
                            return Err("clipboard image exceeds safe size limits".to_string());
                        }
                        Ok(ClipboardContent::Image {
                            width,
                            height,
                            rgba,
                        })
                    }
                    Err(image_error) => match clipboard.get().file_list() {
                        Ok(paths) if !paths.is_empty() => Ok(ClipboardContent::Files(paths)),
                        Ok(_) => {
                            self.inner = None;
                            Err(format!("{error}; image read failed: {image_error}"))
                        }
                        Err(file_error) => {
                            self.inner = None;
                            Err(format!(
                                "{error}; image read failed: {image_error}; file list read failed: {file_error}"
                            ))
                        }
                    },
                }
            }
        }
    }

    fn set_content(&mut self, content: ClipboardContent) -> Result<(), String> {
        if let ClipboardContent::Files(paths) = content {
            return self.set_file_list(&paths);
        }
        let clipboard = self.ensure()?;
        let result = match content {
            ClipboardContent::Text(text) => clipboard.set_text(text),
            ClipboardContent::Image {
                width,
                height,
                rgba,
            } => clipboard.set_image(arboard::ImageData {
                width,
                height,
                bytes: Cow::Owned(rgba),
            }),
            ClipboardContent::Files(_) => unreachable!("file lists returned above"),
        };
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.inner = None;
                Err(error.to_string())
            }
        }
    }

    fn get_file_list(&mut self) -> Result<Vec<PathBuf>, String> {
        let clipboard = self.ensure()?;
        match clipboard.get().file_list() {
            Ok(paths) => Ok(paths),
            Err(arboard::Error::ContentNotAvailable) => Ok(Vec::new()),
            Err(error) => {
                self.inner = None;
                Err(error.to_string())
            }
        }
    }

    fn set_file_list(&mut self, paths: &[PathBuf]) -> Result<(), String> {
        let clipboard = self.ensure()?;
        let result = clipboard.set().file_list(paths);
        match result {
            Ok(()) => Ok(()),
            Err(error) => {
                self.inner = None;
                Err(error.to_string())
            }
        }
    }
}

/// Abstracts the monotonic clock used for paste-request timeouts, so tests can advance time
/// deterministically instead of sleeping.
pub trait ClipboardClock: Send + Sync {
    fn now_ms(&self) -> u64;
}

/// Real [`ClipboardClock`] backed by a process-local `Instant` epoch.
#[derive(Default)]
pub struct SystemClock;

impl ClipboardClock for SystemClock {
    fn now_ms(&self) -> u64 {
        use std::sync::OnceLock;
        use std::time::Instant;

        static EPOCH: OnceLock<Instant> = OnceLock::new();
        let epoch = EPOCH.get_or_init(Instant::now);
        u64::try_from(epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
}

/// State of an outstanding paste request (us asking the remote for its clipboard content).
///
/// Every request we send must be able to time out: [`ClipboardBridge::check_timeouts`]
/// returns any stuck `Requested` state to `Idle` so the next remote copy still works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteState {
    Idle,
    Requested {
        format: ClipboardFormatId,
        requested_at_ms: u64,
    },
}

/// State of our outbound format-list advertise (us telling the remote what we have).
///
/// `on_format_list_response(false)` retries a bounded number of times from `Pending`, and a
/// stray/late rejection can never downgrade `Confirmed` — seeing an `Ok` always means we
/// stop re-advertising the same content, per the hazard documented at
/// `CliprdrBackend::on_format_list_response`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AdvertiseState {
    Idle,
    Pending { attempt: u8, resend_requested: bool },
    Confirmed,
}

struct RemoteTransferEntry {
    index: usize,
    is_dir: bool,
    declared_size: Option<u64>,
    size: Option<u64>,
    received: u64,
}

#[derive(Clone, Copy)]
enum RemoteRequestKind {
    Size,
    Range,
}

#[derive(Clone, Copy)]
struct RemotePendingRequest {
    stream_id: u32,
    index: usize,
    offset: u64,
    requested_size: u32,
    kind: RemoteRequestKind,
    operation_id: Option<u64>,
}

struct StoredRemoteChunk {
    id: u64,
    epoch: u64,
    transfer_id: u64,
    index: usize,
    offset: u64,
    len: usize,
    result: Result<(), String>,
}

struct RemoteTransfer {
    id: u64,
    epoch: u64,
    clip_data_id: Option<u32>,
    entries: Vec<RemoteTransferEntry>,
    pending: Option<RemotePendingRequest>,
    started_at_ms: u64,
}

/// Implements [`CliprdrBackend`]. Deliberately thin: every callback forwards the raw event
/// over the channel and returns immediately, doing no I/O and holding no state of its own.
pub struct ClipboardBackend {
    tx: mpsc::Sender<ClipboardAction>,
    temporary_directory: String,
}

impl fmt::Debug for ClipboardBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ClipboardBackend")
    }
}

impl AsAny for ClipboardBackend {
    fn as_any(&self) -> &dyn core::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
        self
    }
}

impl ClipboardBackend {
    fn send(&self, action: ClipboardAction) {
        if self.tx.send(action).is_err() {
            warn!("clipboard bridge is gone; dropping clipboard event");
        }
    }
}

impl CliprdrBackend for ClipboardBackend {
    fn temporary_directory(&self) -> &str {
        &self.temporary_directory
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED
            | ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA
            | ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED
    }

    fn on_ready(&mut self) {
        debug!("cliprdr channel ready");
    }

    fn on_request_format_list(&mut self) {
        self.send(ClipboardAction::ProtocolFormatListRequested);
    }

    fn on_format_list_response(&mut self, ok: bool) {
        self.send(ClipboardAction::FormatListAcked(ok));
    }

    fn on_process_negotiated_capabilities(
        &mut self,
        capabilities: ClipboardGeneralCapabilityFlags,
    ) {
        self.send(ClipboardAction::NegotiatedCapabilities(capabilities));
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        self.send(ClipboardAction::RemoteCopy(available_formats.to_vec()));
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        self.send(ClipboardAction::LocalDataRequested(request.format));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        self.send(ClipboardAction::RemoteDataReceived(response.into_owned()));
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        self.send(ClipboardAction::LocalFileContentsRequested(request));
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        self.send(ClipboardAction::RemoteFileContentsReceived(
            response.into_owned(),
        ));
    }

    fn on_lock(&mut self, data_id: LockDataId) {
        self.send(ClipboardAction::Lock(data_id));
    }

    fn on_unlock(&mut self, data_id: LockDataId) {
        self.send(ClipboardAction::Unlock(data_id));
    }

    fn on_remote_file_list(&mut self, files: &[FileDescriptor], clip_data_id: Option<u32>) {
        let files = if files.len() <= MAX_FILE_COUNT {
            files.to_vec()
        } else {
            // IronRDP has already decoded the PDU, but avoid copying an oversized descriptor
            // list into the bridge. The empty marker is rejected by stage creation and tears
            // down the pending transfer without allocating another attacker-sized vector.
            warn!(
                file_count = files.len(),
                "remote clipboard file list exceeds the safety limit"
            );
            Vec::new()
        };
        self.send(ClipboardAction::RemoteFileList {
            files,
            clip_data_id,
        });
    }

    fn on_outgoing_locks_expired(&mut self, ids: &[LockDataId]) {
        self.send(ClipboardAction::OutgoingLocksExpired(ids.to_vec()));
    }

    fn on_outgoing_locks_cleared(&mut self, ids: &[LockDataId]) {
        self.send(ClipboardAction::OutgoingLocksCleared(ids.to_vec()));
    }
}

/// The session-side half of the clipboard bridge: owns protocol state and the channel receiver.
/// OS access belongs to [`ClipboardExecutor::Worker`] in production, or to the inline executor
/// used by [`clipboard_channel_with_clock`] in deterministic tests.
pub struct ClipboardBridge {
    rx: mpsc::Receiver<ClipboardAction>,
    /// Actions generated locally by [`Self::poll_local_change`], which has no access to a
    /// live [`Cliprdr`] to act on them immediately. Drained by [`Self::pump`] alongside `rx`.
    local_pending: VecDeque<ClipboardAction>,
    executor: ClipboardExecutor,
    ready_results: VecDeque<ClipboardResult>,
    clock: Arc<dyn ClipboardClock>,
    remote_formats: Vec<ClipboardFormat>,
    negotiated_capabilities: ClipboardGeneralCapabilityFlags,
    capabilities_negotiated: bool,
    local_file_snapshot: Option<LocalFileSnapshot>,
    locked_local_files: HashMap<u32, LocalFileSnapshot>,
    pending_file_copy: Option<LocalFileSnapshot>,
    remote_transfer: Option<RemoteTransfer>,
    pending_remote_prepare: Option<(u64, u64)>,
    pending_remote_publish: Option<(u64, u64)>,
    deferred_remote_file_work: Option<ClipboardWork>,
    stale_file_stream_ids: HashSet<u32>,
    next_transfer_id: u64,
    next_file_stream_id: u32,
    active_transfer: Arc<AtomicU64>,
    paste_state: PasteState,
    advertise_state: AdvertiseState,
    /// Local polling starts before CLIPRDR's Monitor Ready handshake can complete. Sending
    /// before this request makes IronRDP emit initialization PDUs out of protocol order.
    initial_format_list_requested: bool,
    paste_timeout_ms: u64,
    allow_to_remote: bool,
    allow_from_remote: bool,
    /// Effective image ceiling: `min(MAX_IMAGE_BYTES, policy)`.
    max_image_bytes: usize,
    /// Monotonic lifecycle and operation bookkeeping. These values never cross into protocol
    /// state without first being checked against the current epoch/request slot.
    epoch: u64,
    active_epoch: Arc<AtomicU64>,
    observation_generation: u64,
    next_operation_id: u64,
    pending_poll_id: Option<u64>,
    pending_format_read: Option<u64>,
    pending_local_data: VecDeque<u64>,
    pending_local_file_data: VecDeque<u64>,
    pending_paste_id: Option<u64>,
    deferred_format_read: bool,
    deferred_remote_apply: Option<ClipboardWork>,
    worker_active: bool,
    /// Coalesced notification consumed by the session thread and shown through the window's
    /// existing toast path. Keep only a bit: one oversized transfer must not build a queue while
    /// the peer retries it.
    file_limit_notification_pending: bool,
}

/// Creates a matched [`ClipboardBackend`]/[`ClipboardBridge`] pair using a serialized worker and
/// the real system clock. Worker results are picked up on the next session turn; use
/// [`clipboard_channel_with_waker`] when the session has a doorbell to wake immediately.
pub fn clipboard_channel(os: Box<dyn OsClipboard>) -> (ClipboardBackend, ClipboardBridge) {
    clipboard_channel_worker(os, Arc::new(SystemClock), None)
}

/// Production worker constructor that rings `bell` after publishing a result.
pub fn clipboard_channel_with_waker(
    os: Box<dyn OsClipboard>,
    bell: Doorbell,
) -> (ClipboardBackend, ClipboardBridge) {
    clipboard_channel_worker(os, Arc::new(SystemClock), Some(bell))
}

/// Same as [`clipboard_channel`], but with an injectable clock (for deterministic timeout
/// tests).
pub fn clipboard_channel_with_clock(
    os: Box<dyn OsClipboard>,
    clock: Arc<dyn ClipboardClock>,
) -> (ClipboardBackend, ClipboardBridge) {
    clipboard_channel_inline(os, clock)
}

fn clipboard_channel_worker(
    os: Box<dyn OsClipboard>,
    clock: Arc<dyn ClipboardClock>,
    bell: Option<Doorbell>,
) -> (ClipboardBackend, ClipboardBridge) {
    let active_epoch = Arc::new(AtomicU64::new(1));
    let active_transfer = Arc::new(AtomicU64::new(0));
    let staging = StagingLayout::new();
    clipboard_channel_with_executor(
        ClipboardExecutor::Worker(ClipboardWorkerHandle::spawn(
            os,
            bell,
            active_epoch.clone(),
            active_transfer.clone(),
            staging.clone(),
        )),
        clock,
        active_epoch,
        active_transfer,
        staging,
    )
}

fn clipboard_channel_inline(
    os: Box<dyn OsClipboard>,
    clock: Arc<dyn ClipboardClock>,
) -> (ClipboardBackend, ClipboardBridge) {
    let active_epoch = Arc::new(AtomicU64::new(1));
    let active_transfer = Arc::new(AtomicU64::new(0));
    let staging = StagingLayout::new();
    clipboard_channel_with_executor(
        ClipboardExecutor::Inline(ClipboardWorkerState::new(
            os,
            active_epoch.clone(),
            active_transfer.clone(),
            staging.clone(),
        )),
        clock,
        active_epoch,
        active_transfer,
        staging,
    )
}

fn clipboard_channel_with_executor(
    executor: ClipboardExecutor,
    clock: Arc<dyn ClipboardClock>,
    active_epoch: Arc<AtomicU64>,
    active_transfer: Arc<AtomicU64>,
    staging: StagingLayout,
) -> (ClipboardBackend, ClipboardBridge) {
    let (tx, rx) = mpsc::channel();
    let backend = ClipboardBackend {
        tx,
        temporary_directory: staging.temporary_directory().to_string(),
    };
    let bridge = ClipboardBridge {
        rx,
        local_pending: VecDeque::new(),
        executor,
        ready_results: VecDeque::new(),
        clock,
        remote_formats: Vec::new(),
        negotiated_capabilities: ClipboardGeneralCapabilityFlags::empty(),
        capabilities_negotiated: false,
        local_file_snapshot: None,
        locked_local_files: HashMap::new(),
        pending_file_copy: None,
        remote_transfer: None,
        pending_remote_prepare: None,
        pending_remote_publish: None,
        deferred_remote_file_work: None,
        stale_file_stream_ids: HashSet::new(),
        next_transfer_id: 0,
        next_file_stream_id: 0,
        active_transfer,
        paste_state: PasteState::Idle,
        advertise_state: AdvertiseState::Idle,
        initial_format_list_requested: false,
        paste_timeout_ms: DEFAULT_PASTE_TIMEOUT_MS,
        allow_to_remote: true,
        allow_from_remote: true,
        max_image_bytes: MAX_IMAGE_BYTES,
        epoch: 1,
        active_epoch,
        observation_generation: 1,
        next_operation_id: 0,
        pending_poll_id: None,
        pending_format_read: None,
        pending_local_data: VecDeque::new(),
        pending_local_file_data: VecDeque::new(),
        pending_paste_id: None,
        deferred_format_read: false,
        deferred_remote_apply: None,
        worker_active: true,
        file_limit_notification_pending: false,
    };
    (backend, bridge)
}

impl ClipboardBridge {
    /// Apply the session's clipboard policy. The image ceiling only tightens the
    /// built-in safety limit; the paste timeout replaces the default.
    #[must_use]
    pub fn with_policy(mut self, policy: ClipboardPolicy) -> Self {
        self.allow_to_remote = policy.to_remote;
        self.allow_from_remote = policy.from_remote;
        self.max_image_bytes = usize::try_from(policy.max_image_bytes.min(MAX_IMAGE_BYTES as u64))
            .unwrap_or(MAX_IMAGE_BYTES);
        self.paste_timeout_ms = policy.paste_timeout_ms;
        self
    }

    /// Take one pending user-visible notification for an oversized file transfer.
    ///
    /// The session owns the UI, so the bridge reports only this bounded signal and never
    /// carries file names, paths, or bytes across the window boundary.
    pub(crate) fn take_file_limit_notification(&mut self) -> bool {
        std::mem::take(&mut self.file_limit_notification_pending)
    }

    fn note_file_limit_error(&mut self, error: &str) {
        if is_file_limit_error(error) {
            self.file_limit_notification_pending = true;
        }
    }

    /// Formats most recently offered by the remote's clipboard (from the last
    /// `on_remote_copy`).
    pub fn remote_formats(&self) -> &[ClipboardFormat] {
        &self.remote_formats
    }

    /// Processes at most [`MAX_CLIPBOARD_ACTIONS_PER_PUMP`] actions/results, turning protocol
    /// actions into wire messages via `cliprdr`. Reliable actions retain FIFO order; worker
    /// results are admitted one at a time and stale epochs/ids are ignored.
    pub fn pump<R: Role>(&mut self, cliprdr: &mut Cliprdr<R>) -> Vec<CliprdrSvcMessages<R>> {
        self.pump_bounded(cliprdr).0
    }

    /// Like [`Self::pump`], and reports whether the per-turn budget was consumed.
    ///
    /// The session uses this bit to take another immediate turn after a burst instead of
    /// sleeping for its 250 ms idle cadence. An exact-cap drain can cause one harmless extra
    /// turn; it is deliberately conservative because `mpsc::Receiver` has no non-consuming
    /// readiness check.
    pub(crate) fn pump_bounded<R: Role>(
        &mut self,
        cliprdr: &mut Cliprdr<R>,
    ) -> (Vec<CliprdrSvcMessages<R>>, bool) {
        self.check_timeouts();
        // Reliable deferred work gets first claim on a newly freed worker slot. Each is tried
        // once per session turn, so a blocked pasteboard cannot turn retry into a busy loop.
        self.retry_deferred_remote_file_work();
        self.retry_deferred_remote_apply();
        self.retry_deferred_format_read();

        let mut out = Vec::new();
        let mut actions_since_result = 0;
        let mut processed = 0;
        while processed < MAX_CLIPBOARD_ACTIONS_PER_PUMP {
            if self.ready_results.is_empty()
                && let Some(result) = self.executor.try_result()
            {
                self.ready_results.push_back(result);
            }

            if !self.ready_results.is_empty()
                && actions_since_result >= MAX_CLIPBOARD_ACTIONS_BEFORE_RESULT
            {
                let result = self
                    .ready_results
                    .pop_front()
                    .expect("ready result checked above");
                self.handle_result(result, cliprdr, &mut out);
                actions_since_result = 0;
                processed += 1;
            } else if let Some(action) = self.next_action() {
                self.handle_action(action, cliprdr, &mut out);
                actions_since_result += 1;
                processed += 1;
            } else if let Some(result) = self.ready_results.pop_front() {
                self.handle_result(result, cliprdr, &mut out);
                actions_since_result = 0;
                processed += 1;
            } else {
                break;
            }
        }
        (out, processed == MAX_CLIPBOARD_ACTIONS_PER_PUMP)
    }

    /// Throw away queued actions when there is no clipboard channel to send them on.
    ///
    /// A server that never joins CLIPRDR still leaves us polling the local clipboard, and
    /// every change queues an advertise that [`Self::pump`] will never be called to drain.
    /// Over a long session that is an unbounded queue for a channel that does not exist.
    /// State returns to idle: nothing can be in flight when there is nowhere to send it. The
    /// ingress channel is deliberately drained only up to the same per-turn cap; repeated calls
    /// remain bounded even if a peer has queued a large burst.
    pub fn discard_pending(&mut self) {
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.active_epoch.store(self.epoch, Ordering::Release);
        self.observation_generation = self.observation_generation.wrapping_add(1).max(1);
        self.worker_active = false;
        self.local_pending.clear();
        self.ready_results.clear();
        for _ in 0..MAX_CLIPBOARD_ACTIONS_PER_PUMP {
            if self.rx.try_recv().is_err() {
                break;
            }
        }
        self.pending_format_read = None;
        self.pending_poll_id = None;
        self.pending_local_data.clear();
        self.pending_local_file_data.clear();
        self.pending_paste_id = None;
        self.deferred_format_read = false;
        self.deferred_remote_apply = None;
        self.deferred_remote_file_work = None;
        self.file_limit_notification_pending = false;
        self.cancel_remote_transfer();
        self.local_file_snapshot = None;
        self.locked_local_files.clear();
        self.pending_file_copy = None;
        self.paste_state = PasteState::Idle;
        self.advertise_state = AdvertiseState::Idle;
        self.initial_format_list_requested = false;
    }

    /// Resets any paste request that has been pending too long back to `Idle`, so the next
    /// remote copy is not blocked by one that never got an answer. Safe to call often; also
    /// called at the top of every [`Self::pump`].
    pub fn check_timeouts(&mut self) {
        if let PasteState::Requested {
            requested_at_ms, ..
        } = self.paste_state
        {
            let elapsed = self.clock.now_ms().saturating_sub(requested_at_ms);
            if elapsed >= self.paste_timeout_ms {
                warn!(
                    elapsed_ms = elapsed,
                    "clipboard paste request timed out; resetting to idle"
                );
                self.paste_state = PasteState::Idle;
                self.pending_paste_id = None;
            }
        }
        if let Some(transfer) = self.remote_transfer.as_ref() {
            let elapsed = self.clock.now_ms().saturating_sub(transfer.started_at_ms);
            if elapsed >= FILE_TRANSFER_TIMEOUT_MS {
                warn!(elapsed_ms = elapsed, "clipboard file transfer timed out");
                self.cancel_remote_transfer();
            }
        }
    }

    /// Schedules a local clipboard poll. Production returns as soon as the one-deep worker
    /// queue accepts (or coalesces) the advisory operation; the inline executor completes the
    /// same operation immediately for deterministic tests.
    pub fn poll_local_change(&mut self) {
        if !self.allow_to_remote
            || !self.worker_active
            || !self.initial_format_list_requested
            || self.pending_poll_id.is_some()
        {
            return;
        }
        let id = self.next_operation_id();
        let outcome = self.submit_work(ClipboardWork::PollLocal {
            id,
            epoch: self.epoch,
            observation_generation: self.observation_generation,
            allow_to_remote: self.allow_to_remote,
        });
        if matches!(outcome, SubmitOutcome::Queued) {
            self.pending_poll_id = Some(id);
        }
    }

    fn next_action(&mut self) -> Option<ClipboardAction> {
        if let Some(action) = self.local_pending.pop_front() {
            return Some(action);
        }
        self.rx.try_recv().ok()
    }

    fn next_operation_id(&mut self) -> u64 {
        self.next_operation_id = self.next_operation_id.wrapping_add(1).max(1);
        self.next_operation_id
    }

    fn submit_work(&mut self, work: ClipboardWork) -> SubmitOutcome {
        match self.executor.submit(work) {
            SubmitOutcome::Completed(result) => {
                self.ready_results.push_back(result);
                // The operation was accepted and its result is now ready to process.
                SubmitOutcome::Queued
            }
            outcome => outcome,
        }
    }

    fn retry_deferred_remote_apply(&mut self) {
        let Some(work) = self.deferred_remote_apply.take() else {
            return;
        };
        match self.submit_work(work) {
            SubmitOutcome::Full(work) => self.deferred_remote_apply = Some(work),
            SubmitOutcome::Disconnected => {
                warn!("clipboard worker disconnected before applying remote data")
            }
            SubmitOutcome::Completed(_) | SubmitOutcome::Queued => {}
        }
    }

    fn retry_deferred_format_read(&mut self) {
        if !self.deferred_format_read {
            return;
        }
        self.deferred_format_read = false;
        if matches!(self.advertise_state, AdvertiseState::Pending { .. }) {
            self.request_formats();
        }
    }

    fn handle_action<R: Role>(
        &mut self,
        action: ClipboardAction,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        match action {
            ClipboardAction::AdvertiseRequested => self.advertise(),
            ClipboardAction::ProtocolFormatListRequested => {
                self.worker_active = true;
                self.initial_format_list_requested = true;
                self.advertise();
            }
            ClipboardAction::FormatListAcked(ok) => self.handle_format_list_acked(ok, cliprdr, out),
            ClipboardAction::RemoteCopy(formats) => self.handle_remote_copy(formats, cliprdr, out),
            ClipboardAction::LocalDataRequested(format) => {
                self.handle_local_data_requested(format, cliprdr, out)
            }
            ClipboardAction::RemoteDataReceived(response) => {
                self.handle_remote_data_received(response)
            }
            ClipboardAction::RemoteFileList {
                files,
                clip_data_id,
            } => self.handle_remote_file_list(files, clip_data_id),
            ClipboardAction::LocalFileContentsRequested(request) => {
                self.handle_local_file_contents_requested(request, cliprdr, out)
            }
            ClipboardAction::RemoteFileContentsReceived(response) => {
                self.handle_remote_file_contents_received(response, cliprdr, out)
            }
            ClipboardAction::NegotiatedCapabilities(capabilities) => {
                self.negotiated_capabilities = capabilities;
                self.capabilities_negotiated = true;
            }
            ClipboardAction::Lock(data_id) => self.handle_lock(data_id),
            ClipboardAction::Unlock(data_id) => {
                self.locked_local_files.remove(&data_id.0);
            }
            ClipboardAction::OutgoingLocksExpired(ids) => self.handle_outgoing_locks_expired(&ids),
            ClipboardAction::OutgoingLocksCleared(ids) => self.handle_outgoing_locks_cleared(&ids),
        }
    }

    fn handle_result<R: Role>(
        &mut self,
        result: ClipboardResult,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        match result {
            ClipboardResult::LocalPoll { id, epoch, changed } => {
                if epoch != self.epoch || self.pending_poll_id != Some(id) {
                    return;
                }
                self.pending_poll_id = None;
                if !changed {
                    return;
                }
                if !self
                    .local_pending
                    .iter()
                    .any(|action| matches!(action, ClipboardAction::AdvertiseRequested))
                {
                    self.local_pending
                        .push_back(ClipboardAction::AdvertiseRequested);
                }
            }
            ClipboardResult::Formats { id, epoch, result } => {
                if epoch != self.epoch || self.pending_format_read != Some(id) {
                    return;
                }
                self.pending_format_read = None;
                match result {
                    Ok(snapshot) => self.complete_advertise(snapshot, cliprdr, out),
                    Err(error) => {
                        self.note_file_limit_error(&error);
                        warn!(%error, "failed to read OS clipboard for format-list advertise");
                        // Do not leave an older file snapshot available after a failed read:
                        // serving it would expose bytes from a clipboard generation the user no
                        // longer owns. Clear IronRDP's delayed file list when the channel is ready.
                        self.local_file_snapshot = None;
                        self.pending_file_copy = None;
                        if let Ok(messages) = cliprdr.initiate_copy(&[]) {
                            out.push(messages);
                        }
                        self.advertise_state = AdvertiseState::Idle;
                    }
                }
            }
            ClipboardResult::LocalData {
                id,
                epoch,
                response,
            } => {
                if epoch != self.epoch
                    || !self.pending_local_data.iter().any(|pending| *pending == id)
                {
                    return;
                }
                self.pending_local_data.retain(|pending| *pending != id);
                match cliprdr.submit_format_data(response) {
                    Ok(messages) => out.push(messages),
                    Err(error) => warn!(%error, "failed to encode clipboard format-data response"),
                }
            }
            ClipboardResult::LocalFileData {
                id,
                epoch,
                response,
            } => {
                if epoch != self.epoch
                    || !self
                        .pending_local_file_data
                        .iter()
                        .any(|pending| *pending == id)
                {
                    return;
                }
                self.pending_local_file_data
                    .retain(|pending| *pending != id);
                match cliprdr.submit_file_contents(response) {
                    Ok(messages) => out.push(messages),
                    Err(error) => {
                        warn!(%error, "failed to encode clipboard file-contents response")
                    }
                }
            }
            ClipboardResult::RemoteFilesPrepared {
                id,
                epoch,
                transfer_id,
                result,
            } => self.handle_remote_files_prepared(id, epoch, transfer_id, result, cliprdr, out),
            ClipboardResult::RemoteChunkStored {
                id,
                epoch,
                transfer_id,
                index,
                offset,
                len,
                result,
            } => self.handle_remote_chunk_stored(
                StoredRemoteChunk {
                    id,
                    epoch,
                    transfer_id,
                    index,
                    offset,
                    len,
                    result,
                },
                cliprdr,
                out,
            ),
            ClipboardResult::RemoteFilesPublished {
                id,
                epoch,
                transfer_id,
                result,
            } => self.handle_remote_files_published(id, epoch, transfer_id, result),
            ClipboardResult::RemoteFilesAborted => {}
            ClipboardResult::RemoteApplied { id, epoch, result } => {
                if epoch != self.epoch {
                    return;
                }
                if let Err(error) = result {
                    warn!(operation_id = id, %error, "failed to write remote clipboard data");
                }
            }
        }
    }

    fn advertise(&mut self) {
        if !self.initial_format_list_requested {
            return;
        }
        if !self.allow_to_remote {
            // Policy: never announce local content. The server simply sees a client
            // that never copies; nothing in CLIPRDR requires a non-empty list.
            return;
        }
        if let AdvertiseState::Pending { attempt, .. } = self.advertise_state {
            self.advertise_state = AdvertiseState::Pending {
                attempt,
                resend_requested: true,
            };
        } else {
            self.advertise_state = AdvertiseState::Pending {
                attempt: 0,
                resend_requested: false,
            };
            self.request_formats();
        }
    }

    fn request_formats(&mut self) {
        let id = self.next_operation_id();
        self.pending_format_read = Some(id);
        let outcome = self.submit_work(ClipboardWork::ReadFormats {
            id,
            epoch: self.epoch,
        });
        match outcome {
            SubmitOutcome::Full(_) => {
                self.pending_format_read = None;
                self.deferred_format_read = true;
            }
            SubmitOutcome::Disconnected => {
                self.pending_format_read = None;
                self.advertise_state = AdvertiseState::Idle;
                warn!("clipboard worker is unavailable for format-list advertise");
            }
            SubmitOutcome::Completed(_) | SubmitOutcome::Queued => {}
        }
    }

    fn complete_advertise<R: Role>(
        &mut self,
        snapshot: LocalClipboardSnapshot,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        // Taking local clipboard ownership supersedes an eager download of the previous remote
        // clipboard. Cancel before any new FormatList can reach the wire so a stale worker result
        // cannot publish old files after this local generation wins.
        self.cancel_remote_transfer();
        if let Some(files) = snapshot.files {
            self.local_file_snapshot = Some(files.clone());
            self.pending_file_copy = Some(files.clone());
            if self.capabilities_negotiated
                && !self
                    .negotiated_capabilities
                    .contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED)
            {
                warn!("remote does not support clipboard file transfer");
                self.pending_file_copy = None;
                self.advertise_state = AdvertiseState::Idle;
                return;
            }
            match cliprdr.initiate_file_copy(
                files
                    .entries
                    .iter()
                    .map(|entry| entry.descriptor.clone())
                    .collect(),
            ) {
                Ok(messages) => {
                    out.push(messages);
                    self.pending_file_copy = None;
                }
                Err(file_error) => {
                    // The first local copy can arrive while IronRDP is still in its
                    // Initialization state. The empty bootstrap carries capabilities and
                    // temporary-directory PDUs; the descriptor list is sent after its ack.
                    match cliprdr.initiate_copy(&[]) {
                        Ok(messages) => out.push(messages),
                        Err(error) => {
                            warn!(%file_error, %error, "failed to advertise clipboard file list");
                            self.pending_file_copy = None;
                            self.advertise_state = AdvertiseState::Idle;
                        }
                    }
                }
            }
        } else {
            self.local_file_snapshot = None;
            self.pending_file_copy = None;
            match cliprdr.initiate_copy(&snapshot.formats) {
                Ok(messages) => out.push(messages),
                Err(error) => {
                    warn!(%error, "failed to encode clipboard format-list advertise; giving up for now");
                    self.advertise_state = AdvertiseState::Idle;
                }
            }
        }
    }

    fn handle_format_list_acked<R: Role>(
        &mut self,
        ok: bool,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        // An acknowledgement is itself proof that the protocol requested and received an
        // initial list. This keeps synthetic ready-client tests faithful too.
        self.initial_format_list_requested = true;
        match self.advertise_state {
            AdvertiseState::Pending {
                resend_requested: true,
                ..
            } => {
                // Only one FormatList exchange may be in flight. A local change that
                // arrived meanwhile supersedes both this response and its retry count.
                self.advertise_state = AdvertiseState::Pending {
                    attempt: 0,
                    resend_requested: false,
                };
                self.request_formats();
            }
            AdvertiseState::Pending {
                resend_requested: false,
                ..
            } if ok => {
                if let Some(files) = self.pending_file_copy.take() {
                    match cliprdr.initiate_file_copy(
                        files
                            .entries
                            .iter()
                            .map(|entry| entry.descriptor.clone())
                            .collect(),
                    ) {
                        Ok(messages) => out.push(messages),
                        Err(error) => {
                            warn!(%error, "failed to send pending clipboard file list");
                            self.local_file_snapshot = None;
                            if let Ok(messages) = cliprdr.initiate_copy(&[]) {
                                out.push(messages);
                            }
                            self.advertise_state = AdvertiseState::Idle;
                        }
                    }
                } else {
                    self.advertise_state = AdvertiseState::Confirmed;
                }
            }
            AdvertiseState::Pending {
                attempt,
                resend_requested: false,
            } => {
                if attempt + 1 < MAX_ADVERTISE_ATTEMPTS {
                    self.advertise_state = AdvertiseState::Pending {
                        attempt: attempt + 1,
                        resend_requested: false,
                    };
                    debug!(
                        attempt = attempt + 1,
                        "remote rejected clipboard format list, retrying"
                    );
                    self.request_formats();
                } else {
                    warn!(
                        attempts = MAX_ADVERTISE_ATTEMPTS,
                        "remote rejected clipboard format list; giving up until next local change"
                    );
                    self.advertise_state = AdvertiseState::Idle;
                }
            }
            AdvertiseState::Idle | AdvertiseState::Confirmed => {
                if ok {
                    self.advertise_state = AdvertiseState::Confirmed;
                } else {
                    // Never let a stale/late rejection clear state a later success already
                    // established.
                    trace!("ignoring stale clipboard format-list rejection");
                }
            }
        }
    }

    fn handle_remote_copy<R: Role>(
        &mut self,
        formats: Vec<ClipboardFormat>,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        self.worker_active = true;
        // A new remote FormatList supersedes any eager file download from the previous
        // clipboard. Cancel before requesting the new delayed file list so an old staged
        // transfer cannot publish after the clipboard generation changes.
        self.cancel_remote_transfer();
        self.remote_formats = formats;

        if !self.allow_from_remote {
            self.paste_state = PasteState::Idle;
            self.pending_paste_id = None;
            return;
        }

        let allow_remote_files = !self.capabilities_negotiated
            || self
                .negotiated_capabilities
                .contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED);
        let Some(format) = best_content_format(&self.remote_formats, allow_remote_files) else {
            debug!("remote copy offered no supported clipboard format");
            self.paste_state = PasteState::Idle;
            self.pending_paste_id = None;
            return;
        };

        let id = self.next_operation_id();
        self.pending_paste_id = Some(id);
        self.paste_state = PasteState::Requested {
            format,
            requested_at_ms: self.clock.now_ms(),
        };
        match cliprdr.initiate_paste(format) {
            Ok(messages) => out.push(messages),
            Err(error) => {
                warn!(%error, "failed to send clipboard paste request");
                self.paste_state = PasteState::Idle;
                self.pending_paste_id = None;
            }
        }
    }

    fn next_transfer_id(&mut self) -> u64 {
        self.next_transfer_id = self.next_transfer_id.wrapping_add(1).max(1);
        self.next_transfer_id
    }

    fn next_file_stream_id(&mut self) -> u32 {
        for _ in 0..=MAX_STALE_FILE_STREAM_IDS {
            self.next_file_stream_id = self.next_file_stream_id.wrapping_add(1).max(1);
            if !self
                .stale_file_stream_ids
                .contains(&self.next_file_stream_id)
            {
                return self.next_file_stream_id;
            }
        }
        // At most one stream is active and the stale set is bounded, so this is unreachable
        // unless the stream-id bookkeeping invariant is broken.
        unreachable!("no clipboard file stream id is available")
    }

    fn remember_stale_file_stream(&mut self, stream_id: u32) {
        if self.stale_file_stream_ids.len() < MAX_STALE_FILE_STREAM_IDS {
            self.stale_file_stream_ids.insert(stream_id);
        }
    }

    fn retry_deferred_remote_file_work(&mut self) {
        let Some(work) = self.deferred_remote_file_work.take() else {
            return;
        };
        match self.submit_work(work) {
            SubmitOutcome::Full(work) => self.deferred_remote_file_work = Some(work),
            SubmitOutcome::Disconnected => {
                warn!("clipboard worker disconnected during file transfer")
            }
            SubmitOutcome::Completed(_) | SubmitOutcome::Queued => {}
        }
    }

    fn submit_remote_file_work(&mut self, work: ClipboardWork) {
        match self.submit_work(work) {
            SubmitOutcome::Full(work) => {
                if self.deferred_remote_file_work.replace(work).is_some() {
                    warn!("clipboard file transfer work was superseded")
                }
            }
            SubmitOutcome::Disconnected => {
                warn!("clipboard worker disconnected during file transfer")
            }
            SubmitOutcome::Completed(_) | SubmitOutcome::Queued => {}
        }
    }

    fn handle_remote_file_list(&mut self, files: Vec<FileDescriptor>, clip_data_id: Option<u32>) {
        let requested_format = match self.paste_state {
            PasteState::Requested { format, .. } => format,
            PasteState::Idle => return,
        };
        if !self.is_file_list_format(requested_format)
            || !self.allow_from_remote
            || (self.capabilities_negotiated
                && !self
                    .negotiated_capabilities
                    .contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED))
        {
            return;
        }
        if files.is_empty() {
            self.note_file_limit_error("remote clipboard file count exceeds the safety limit");
            warn!("remote clipboard file list is empty or oversized");
            self.paste_state = PasteState::Idle;
            self.pending_paste_id = None;
            self.cancel_remote_transfer();
            return;
        }
        self.paste_state = PasteState::Idle;
        self.pending_paste_id = None;
        self.cancel_remote_transfer();

        let transfer_id = self.next_transfer_id();
        self.active_transfer.store(transfer_id, Ordering::Release);
        let operation_id = self.next_operation_id();
        self.remote_transfer = Some(RemoteTransfer {
            id: transfer_id,
            epoch: self.epoch,
            clip_data_id,
            entries: Vec::new(),
            pending: None,
            started_at_ms: self.clock.now_ms(),
        });
        self.pending_remote_prepare = Some((operation_id, transfer_id));
        self.submit_remote_file_work(ClipboardWork::PrepareRemoteFiles {
            id: operation_id,
            epoch: self.epoch,
            transfer_id,
            files,
        });
    }

    fn is_file_list_format(&self, format: ClipboardFormatId) -> bool {
        self.remote_formats.iter().any(|available| {
            available.id() == format
                && available
                    .name()
                    .is_some_and(|name| name.value() == FORMAT_NAME_FILE_LIST)
        })
    }

    fn handle_remote_files_prepared<R: Role>(
        &mut self,
        id: u64,
        epoch: u64,
        transfer_id: u64,
        result: Result<Vec<RemotePreparedEntry>, String>,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        if epoch != self.epoch || self.pending_remote_prepare != Some((id, transfer_id)) {
            return;
        }
        self.pending_remote_prepare = None;
        let prepared = match result {
            Ok(prepared) => prepared,
            Err(error) => {
                self.note_file_limit_error(&error);
                warn!(operation_id = id, "remote clipboard file list was rejected");
                self.cancel_remote_transfer();
                return;
            }
        };
        if prepared.is_empty() || prepared.len() > MAX_FILE_COUNT {
            self.note_file_limit_error("remote clipboard file count exceeds the safety limit");
            warn!("remote clipboard file list had an invalid count");
            self.cancel_remote_transfer();
            return;
        }
        let Some(transfer) = self.remote_transfer.as_mut() else {
            return;
        };
        if transfer.id != transfer_id {
            return;
        }
        let mut total_known = 0u64;
        let mut entries = Vec::with_capacity(prepared.len());
        for (expected_index, item) in prepared.into_iter().enumerate() {
            if item.index != expected_index
                || (item.is_dir && item.declared_size.is_some_and(|size| size != 0))
            {
                warn!("remote clipboard file list correlation failed");
                self.cancel_remote_transfer();
                return;
            }
            if !item.is_dir
                && let Some(size) = item.declared_size
            {
                total_known = match total_known.checked_add(size) {
                    Some(total) if total <= MAX_FILE_TOTAL_BYTES => total,
                    _ => {
                        self.note_file_limit_error(
                            "remote clipboard files exceed the total size limit",
                        );
                        warn!("remote clipboard files exceed the total size limit");
                        self.cancel_remote_transfer();
                        return;
                    }
                };
            }
            entries.push(RemoteTransferEntry {
                index: expected_index,
                is_dir: item.is_dir,
                declared_size: item.declared_size,
                size: item.declared_size.or_else(|| item.is_dir.then_some(0)),
                received: 0,
            });
        }
        transfer.entries = entries;
        self.request_next_remote_file(cliprdr, out);
    }

    fn request_next_remote_file<R: Role>(
        &mut self,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        let Some(snapshot) = self.remote_transfer.as_ref() else {
            return;
        };
        if snapshot.pending.is_some() {
            return;
        }
        let transfer_id = snapshot.id;
        let epoch = snapshot.epoch;
        let clip_data_id = snapshot.clip_data_id;
        let next = snapshot
            .entries
            .iter()
            .find(|entry| !entry.is_dir && entry.size.is_none())
            .map(|entry| entry.index);
        let range = snapshot
            .entries
            .iter()
            .find(|entry| !entry.is_dir && entry.size.is_some_and(|size| entry.received < size));

        let (index, flags, offset, requested_size, kind) = if let Some(index) = next {
            (
                index,
                FileContentsFlags::SIZE,
                0,
                8,
                RemoteRequestKind::Size,
            )
        } else if let Some(entry) = range {
            let size = entry.size.expect("range entry has size");
            let remaining = size.saturating_sub(entry.received);
            let requested = remaining.min(u64::from(MAX_FILE_CHUNK_BYTES)) as u32;
            if requested == 0 {
                return;
            }
            (
                entry.index,
                FileContentsFlags::RANGE,
                entry.received,
                requested,
                RemoteRequestKind::Range,
            )
        } else {
            let sizes = snapshot
                .entries
                .iter()
                .filter(|entry| !entry.is_dir)
                .filter_map(|entry| entry.size.map(|size| (entry.index, size)))
                .collect();
            let operation_id = self.next_operation_id();
            self.pending_remote_publish = Some((operation_id, transfer_id));
            self.submit_remote_file_work(ClipboardWork::PublishRemoteFiles {
                id: operation_id,
                epoch,
                transfer_id,
                sizes,
            });
            return;
        };

        let stream_id = self.next_file_stream_id();
        let request = FileContentsRequest {
            stream_id,
            index: i32::try_from(index).unwrap_or(i32::MAX),
            flags,
            position: offset,
            requested_size,
            data_id: clip_data_id,
        };
        match cliprdr.request_file_contents(request) {
            Ok(messages) => {
                out.push(messages);
                if let Some(transfer) = self.remote_transfer.as_mut() {
                    transfer.pending = Some(RemotePendingRequest {
                        stream_id,
                        index,
                        offset,
                        requested_size,
                        kind,
                        operation_id: None,
                    });
                }
            }
            Err(error) => {
                warn!(%error, "failed to request remote clipboard file contents");
                self.cancel_remote_transfer();
            }
        }
    }

    fn handle_remote_file_contents_received<R: Role>(
        &mut self,
        response: OwnedFileContentsResponse,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        let stream_id = response.stream_id();
        if self.stale_file_stream_ids.remove(&stream_id) {
            // A response for a canceled transfer may still be in flight in CLIPRDR. It must not
            // consume or cancel a newer transfer's pending request.
            return;
        }
        let Some(transfer) = self.remote_transfer.as_ref() else {
            self.cancel_remote_transfer();
            return;
        };
        let Some(pending) = transfer.pending else {
            self.cancel_remote_transfer();
            return;
        };
        if stream_id != pending.stream_id || response.is_error() {
            warn!("remote clipboard file contents response correlation failed");
            self.cancel_remote_transfer();
            return;
        }
        let transfer_id = transfer.id;
        let pending = self
            .remote_transfer
            .as_mut()
            .and_then(|transfer| transfer.pending.take())
            .expect("pending file response was checked above");
        match pending.kind {
            RemoteRequestKind::Size => {
                let Some(data) = response.data().get(..8) else {
                    self.cancel_remote_transfer();
                    return;
                };
                if response.data().len() != 8 {
                    self.cancel_remote_transfer();
                    return;
                }
                let size = u64::from_le_bytes(data.try_into().expect("size length checked"));
                if size > MAX_FILE_TOTAL_BYTES {
                    self.note_file_limit_error(
                        "remote clipboard file size exceeds the safety limit",
                    );
                    self.cancel_remote_transfer();
                    return;
                }
                let Some(entry) = self
                    .remote_transfer
                    .as_ref()
                    .and_then(|transfer| transfer.entries.get(pending.index))
                else {
                    self.cancel_remote_transfer();
                    return;
                };
                if entry.is_dir || entry.declared_size.is_some_and(|declared| declared != size) {
                    self.cancel_remote_transfer();
                    return;
                }
                let total_known = self
                    .remote_transfer
                    .as_ref()
                    .into_iter()
                    .flat_map(|transfer| transfer.entries.iter())
                    .filter(|entry| !entry.is_dir)
                    .filter_map(|entry| entry.size)
                    .fold(0u64, |total, value| total.saturating_add(value));
                if total_known.saturating_add(size) > MAX_FILE_TOTAL_BYTES {
                    self.note_file_limit_error(
                        "remote clipboard files exceed the total size limit",
                    );
                    self.cancel_remote_transfer();
                    return;
                }
                if let Some(transfer) = self.remote_transfer.as_mut()
                    && let Some(entry) = transfer.entries.get_mut(pending.index)
                {
                    entry.size = Some(size);
                }
                self.request_next_remote_file(cliprdr, out);
            }
            RemoteRequestKind::Range => {
                if response.data().len() != pending.requested_size as usize {
                    self.cancel_remote_transfer();
                    return;
                }
                let Some((size, received)) = self
                    .remote_transfer
                    .as_ref()
                    .and_then(|transfer| transfer.entries.get(pending.index))
                    .and_then(|entry| entry.size.map(|size| (size, entry.received)))
                else {
                    self.cancel_remote_transfer();
                    return;
                };
                let Some(end) = pending
                    .offset
                    .checked_add(u64::from(pending.requested_size))
                else {
                    self.cancel_remote_transfer();
                    return;
                };
                if received != pending.offset || end > size {
                    self.cancel_remote_transfer();
                    return;
                }
                let operation_id = self.next_operation_id();
                let data = response.data().to_vec();
                self.submit_remote_file_work(ClipboardWork::StoreRemoteChunk {
                    id: operation_id,
                    epoch: self.epoch,
                    transfer_id,
                    index: pending.index,
                    offset: pending.offset,
                    expected_size: size,
                    data,
                });
                if let Some(transfer) = self.remote_transfer.as_mut()
                    && let Some(current) = transfer.pending.as_mut()
                {
                    *current = RemotePendingRequest {
                        operation_id: Some(operation_id),
                        ..pending
                    };
                }
            }
        }
    }

    fn handle_remote_chunk_stored<R: Role>(
        &mut self,
        stored: StoredRemoteChunk,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        let StoredRemoteChunk {
            id,
            epoch,
            transfer_id,
            index,
            offset,
            len,
            result,
        } = stored;
        if epoch != self.epoch {
            return;
        }
        let Some(transfer) = self.remote_transfer.as_mut() else {
            return;
        };
        if transfer.id != transfer_id {
            return;
        }
        let Some(pending) = transfer.pending.as_ref() else {
            return;
        };
        if pending.operation_id != Some(id)
            || pending.index != index
            || pending.offset != offset
            || pending.requested_size as usize != len
        {
            return;
        }
        if let Err(error) = &result {
            self.note_file_limit_error(error);
            self.cancel_remote_transfer();
            return;
        }
        let Some(entry) = transfer.entries.get_mut(index) else {
            self.cancel_remote_transfer();
            return;
        };
        entry.received = match entry.received.checked_add(len as u64) {
            Some(received) => received,
            None => {
                self.cancel_remote_transfer();
                return;
            }
        };
        transfer.pending = None;
        self.request_next_remote_file(cliprdr, out);
    }

    fn handle_remote_files_published(
        &mut self,
        id: u64,
        epoch: u64,
        transfer_id: u64,
        result: Result<(), String>,
    ) {
        if epoch != self.epoch || self.pending_remote_publish != Some((id, transfer_id)) {
            return;
        }
        self.pending_remote_publish = None;
        if let Err(error) = &result {
            self.note_file_limit_error(error);
            warn!(
                operation_id = id,
                "remote clipboard files were not published"
            );
        }
        self.active_transfer.store(0, Ordering::Release);
        self.remote_transfer = None;
    }

    fn cancel_remote_transfer(&mut self) {
        let transfer_id = self.remote_transfer.take().map(|transfer| {
            if let Some(pending) = transfer.pending {
                self.remember_stale_file_stream(pending.stream_id);
            }
            transfer.id
        });
        self.active_transfer.store(0, Ordering::Release);
        self.pending_remote_prepare = None;
        self.pending_remote_publish = None;
        if let Some(transfer_id) = transfer_id {
            let work = ClipboardWork::AbortRemoteFiles { transfer_id };
            self.deferred_remote_file_work = None;
            self.submit_remote_file_work(work);
        }
    }

    fn handle_lock(&mut self, data_id: LockDataId) {
        if let Some(snapshot) = &self.local_file_snapshot {
            self.locked_local_files.insert(data_id.0, snapshot.clone());
        }
    }

    fn handle_local_file_contents_requested<R: Role>(
        &mut self,
        request: FileContentsRequest,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        if !self.allow_to_remote {
            self.submit_file_error(cliprdr, out, request.stream_id);
            return;
        }
        let snapshot = match request.data_id {
            Some(data_id) => self.locked_local_files.get(&data_id),
            None => self.local_file_snapshot.as_ref(),
        };
        let Some(snapshot) = snapshot else {
            self.submit_file_error(cliprdr, out, request.stream_id);
            return;
        };
        let Ok(index) = usize::try_from(request.index) else {
            self.submit_file_error(cliprdr, out, request.stream_id);
            return;
        };
        let Some(entry) = snapshot.entries.get(index).cloned() else {
            self.submit_file_error(cliprdr, out, request.stream_id);
            return;
        };
        let id = self.next_operation_id();
        self.pending_local_file_data.push_back(id);
        let outcome = self.submit_work(ClipboardWork::ReadLocalFile {
            id,
            epoch: self.epoch,
            request: request.clone(),
            entry,
        });
        if matches!(
            outcome,
            SubmitOutcome::Full(_) | SubmitOutcome::Disconnected
        ) {
            self.pending_local_file_data
                .retain(|pending| *pending != id);
            self.submit_file_error(cliprdr, out, request.stream_id);
        }
    }

    fn submit_file_error<R: Role>(
        &mut self,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
        stream_id: u32,
    ) {
        match cliprdr.submit_file_contents(OwnedFileContentsResponse::new_error(stream_id)) {
            Ok(messages) => out.push(messages),
            Err(error) => warn!(%error, "failed to encode clipboard file-contents error"),
        }
    }

    fn handle_outgoing_locks_expired(&mut self, ids: &[LockDataId]) {
        if self.remote_transfer.as_ref().is_some_and(|transfer| {
            transfer
                .clip_data_id
                .is_some_and(|id| ids.iter().any(|lock| lock.0 == id))
        }) {
            // IronRDP keeps expired locks alive while requests are in flight. Continue the
            // current transfer; the cleared callback below is the point where the lock is gone.
        }
    }

    fn handle_outgoing_locks_cleared(&mut self, ids: &[LockDataId]) {
        if self.remote_transfer.as_ref().is_some_and(|transfer| {
            transfer
                .clip_data_id
                .is_some_and(|id| ids.iter().any(|lock| lock.0 == id))
        }) {
            self.cancel_remote_transfer();
        }
    }

    fn handle_local_data_requested<R: Role>(
        &mut self,
        format: ClipboardFormatId,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        if !self.allow_to_remote {
            self.submit_error_response(cliprdr, out);
            return;
        }

        let id = self.next_operation_id();
        self.pending_local_data.push_back(id);
        let outcome = self.submit_work(ClipboardWork::ReadLocalData {
            id,
            epoch: self.epoch,
            format,
            max_image_bytes: self.max_image_bytes,
        });
        if matches!(
            outcome,
            SubmitOutcome::Full(_) | SubmitOutcome::Disconnected
        ) {
            self.pending_local_data.retain(|pending| *pending != id);
            // A dropped response wedges the peer's paste. Saturation is therefore an explicit
            // protocol error, never a silent return.
            self.submit_error_response(cliprdr, out);
        }
    }

    fn submit_error_response<R: Role>(
        &mut self,
        cliprdr: &mut Cliprdr<R>,
        out: &mut Vec<CliprdrSvcMessages<R>>,
    ) {
        match cliprdr.submit_format_data(OwnedFormatDataResponse::new_error()) {
            Ok(messages) => out.push(messages),
            Err(error) => warn!(%error, "failed to encode clipboard format-data error response"),
        }
    }

    fn handle_remote_data_received(&mut self, response: OwnedFormatDataResponse) {
        let format = match std::mem::replace(&mut self.paste_state, PasteState::Idle) {
            PasteState::Requested { format, .. } => format,
            PasteState::Idle => {
                debug!("received clipboard data with no pending paste request; ignoring");
                return;
            }
        };
        let id = self.pending_paste_id.take().unwrap_or(0);
        if id == 0 {
            debug!("received clipboard data without a current operation id; ignoring");
            return;
        }

        if response.is_error() {
            warn!("remote reported an error providing clipboard data");
            return;
        }

        if !matches!(
            format,
            ClipboardFormatId::CF_UNICODETEXT
                | ClipboardFormatId::CF_TEXT
                | ClipboardFormatId::CF_DIB
                | ClipboardFormatId::CF_DIBV5
        ) {
            debug!(?format, "received unsupported clipboard format");
            return;
        }

        if !self.allow_from_remote {
            return; // Policy: remote clipboard content never lands locally.
        }
        // A remote paste supersedes any local observation that completed before it. Clearing
        // these slots prevents an old poll or format read from advertising remote-origin data
        // back to the peer. The worker generation resets fingerprint state in execution order.
        self.observation_generation = self.observation_generation.wrapping_add(1).max(1);
        self.pending_poll_id = None;
        self.pending_format_read = None;
        self.deferred_format_read = false;
        self.advertise_state = AdvertiseState::Idle;
        self.local_pending
            .retain(|action| !matches!(action, ClipboardAction::AdvertiseRequested));
        // A response for the newest remote copy supersedes older data still waiting for a
        // worker slot. If the old operation is already executing/queued, serialization puts
        // this newer one after it; if it is only deferred, remove it so it cannot overwrite
        // the newer paste on a later pump.
        self.deferred_remote_apply = None;

        let work = ClipboardWork::ApplyRemote {
            id,
            epoch: self.epoch,
            observation_generation: self.observation_generation,
            format,
            data: response.into_data().into_owned(),
            max_image_bytes: self.max_image_bytes,
        };
        match self.submit_work(work) {
            SubmitOutcome::Full(work) => {
                if self.deferred_remote_apply.replace(work).is_some() {
                    warn!(
                        operation_id = id,
                        "newer remote clipboard data superseded a deferred paste"
                    );
                }
            }
            SubmitOutcome::Disconnected => {
                warn!(
                    operation_id = id,
                    "clipboard worker is unavailable; remote paste was not applied"
                );
            }
            SubmitOutcome::Completed(_) | SubmitOutcome::Queued => {}
        }
    }
}

impl Drop for ClipboardBridge {
    fn drop(&mut self) {
        self.cancel_remote_transfer();
        self.worker_active = false;
        self.epoch = self.epoch.wrapping_add(1).max(1);
        self.active_epoch.store(self.epoch, Ordering::Release);
        let id = self.next_operation_id.wrapping_add(1).max(1);
        self.executor.shutdown(id, self.epoch);
    }
}

fn read_local_snapshot(os: &mut dyn OsClipboard) -> Result<LocalClipboardSnapshot, String> {
    let content = os.get_content()?;
    match content {
        ClipboardContent::Text(_) => Ok(LocalClipboardSnapshot {
            formats: text_formats(),
            files: None,
        }),
        ClipboardContent::Image { .. } => Ok(LocalClipboardSnapshot {
            formats: image_formats(),
            files: None,
        }),
        ClipboardContent::Files(paths) => Ok(LocalClipboardSnapshot {
            formats: vec![
                ClipboardFormat::new(ClipboardFormatId::new(0xC0FE))
                    .with_name(ClipboardFormatName::new_static(FORMAT_NAME_FILE_LIST)),
            ],
            files: Some(build_local_file_snapshot(&paths)?),
        }),
    }
}

fn build_local_file_snapshot(paths: &[PathBuf]) -> Result<LocalFileSnapshot, String> {
    if paths.is_empty() || paths.len() > MAX_FILE_COUNT {
        return Err("clipboard file count exceeds the safety limit".to_string());
    }
    let mut builder = LocalSnapshotBuilder {
        entries: Vec::new(),
        seen: HashSet::new(),
        total_size: 0,
    };
    for path in paths {
        let metadata = fs::symlink_metadata(path)
            .map_err(|_| "clipboard file metadata could not be read".to_string())?;
        if metadata.file_type().is_symlink() {
            return Err("clipboard symlinks are not supported".to_string());
        }
        let canonical = fs::canonicalize(path)
            .map_err(|_| "clipboard file path could not be resolved".to_string())?;
        append_local_file_entry(&mut builder, &canonical, None)?;
    }
    Ok(LocalFileSnapshot {
        entries: builder.entries,
    })
}

struct LocalSnapshotBuilder {
    entries: Vec<LocalFileEntry>,
    seen: HashSet<String>,
    total_size: u64,
}

fn append_local_file_entry(
    builder: &mut LocalSnapshotBuilder,
    path: &PathBuf,
    relative_path: Option<String>,
) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| "clipboard file metadata could not be read".to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("clipboard symlinks are not supported".to_string());
    }
    let canonical = fs::canonicalize(path)
        .map_err(|_| "clipboard file path could not be resolved".to_string())?;
    let metadata = fs::symlink_metadata(&canonical)
        .map_err(|_| "clipboard file metadata could not be read".to_string())?;
    if metadata.file_type().is_symlink() {
        return Err("clipboard symlinks are not supported".to_string());
    }
    let name = canonical
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "clipboard file name is not valid UTF-8".to_string())?;
    validate_file_component(name)?;
    let key = relative_path
        .as_ref()
        .map(|parent| format!("{parent}\\{name}"))
        .unwrap_or_else(|| name.to_string())
        .to_lowercase();
    if !builder.seen.insert(key) {
        return Err("clipboard file paths collide".to_string());
    }
    let wire_len = relative_path
        .as_ref()
        .map_or(0, |parent| wire_component_len(parent) + 1)
        + wire_component_len(name);
    if wire_len > 259 {
        return Err("clipboard file path is too long".to_string());
    }
    let is_dir = metadata.is_dir();
    let len = if is_dir { 0 } else { metadata.len() };
    builder.total_size = builder
        .total_size
        .checked_add(len)
        .ok_or_else(|| "clipboard file sizes overflow".to_string())?;
    if builder.total_size > MAX_FILE_TOTAL_BYTES {
        return Err("clipboard files exceed the total size limit".to_string());
    }
    if builder.entries.len() >= MAX_FILE_COUNT {
        return Err("clipboard file count exceeds the safety limit".to_string());
    }
    let attributes = if is_dir {
        ClipboardFileAttributes::DIRECTORY
    } else {
        ClipboardFileAttributes::NORMAL
    };
    let mut descriptor = FileDescriptor::new(name).with_attributes(attributes);
    if let Some(parent) = &relative_path {
        descriptor = descriptor.with_relative_path(parent.clone());
    }
    if !is_dir {
        descriptor = descriptor.with_file_size(len);
    }
    if let Some(last_write_time) = filetime_from_system_time(metadata.modified().ok()) {
        descriptor = descriptor.with_last_write_time(last_write_time);
    }
    builder.entries.push(LocalFileEntry {
        path: canonical.clone(),
        descriptor,
        identity: file_identity(&metadata),
        is_dir,
    });

    if is_dir {
        // Keep traversal bounded before sorting: a local directory can contain far more
        // entries than the wire limit, and collecting the whole iterator first would let a
        // large directory consume unbounded memory before we reject it.
        let mut children = fs::read_dir(&canonical)
            .map_err(|_| "clipboard directory could not be read".to_string())?
            .take(MAX_FILE_COUNT.saturating_add(1))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| "clipboard directory could not be read".to_string())?;
        if children.len() > MAX_FILE_COUNT {
            return Err("clipboard file count exceeds the safety limit".to_string());
        }
        children.sort_by_key(|child| child.file_name());
        for child in children {
            let child_name = child.file_name();
            let child_name = child_name
                .to_str()
                .ok_or_else(|| "clipboard file name is not valid UTF-8".to_string())?;
            let child_relative_path = Some(match &relative_path {
                Some(parent) => format!("{parent}\\{name}"),
                None => name.to_string(),
            });
            let child_path = canonical.join(child_name);
            append_local_file_entry(builder, &child_path, child_relative_path)?;
        }
    }
    Ok(())
}

fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity {
        len: metadata.len(),
        modified: metadata.modified().ok(),
        is_dir: metadata.is_dir(),
        #[cfg(unix)]
        device: {
            use std::os::unix::fs::MetadataExt;
            metadata.dev()
        },
        #[cfg(unix)]
        inode: {
            use std::os::unix::fs::MetadataExt;
            metadata.ino()
        },
        #[cfg(windows)]
        creation_time: {
            use std::os::windows::fs::MetadataExt;
            metadata.creation_time()
        },
        #[cfg(windows)]
        last_write_time: {
            use std::os::windows::fs::MetadataExt;
            metadata.last_write_time()
        },
    }
}

fn filetime_from_system_time(time: Option<SystemTime>) -> Option<u64> {
    const WINDOWS_EPOCH_OFFSET_SECONDS: u64 = 11_644_473_600;
    let duration = time?.duration_since(UNIX_EPOCH).ok()?;
    let seconds = duration
        .as_secs()
        .checked_add(WINDOWS_EPOCH_OFFSET_SECONDS)?;
    seconds
        .checked_mul(10_000_000)?
        .checked_add(u64::from(duration.subsec_nanos() / 100))
}

fn read_local_file(
    request: &FileContentsRequest,
    entry: &LocalFileEntry,
) -> OwnedFileContentsResponse {
    let error = || OwnedFileContentsResponse::new_error(request.stream_id);
    if request.index < 0 || request.flags.validate().is_err() {
        return error();
    }
    let identity = match fs::symlink_metadata(&entry.path) {
        Ok(metadata) if !metadata.file_type().is_symlink() => file_identity(&metadata),
        _ => return error(),
    };
    if identity != entry.identity {
        return error();
    }
    if request.flags == FileContentsFlags::SIZE {
        if request.position != 0 || request.requested_size != 8 {
            return error();
        }
        let size = if entry.is_dir { 0 } else { entry.identity.len };
        return OwnedFileContentsResponse::new_size_response(request.stream_id, size);
    }
    if request.flags != FileContentsFlags::RANGE
        || entry.is_dir
        || request.requested_size == 0
        || request.requested_size > MAX_FILE_CHUNK_BYTES
    {
        return error();
    }
    let end = match request
        .position
        .checked_add(u64::from(request.requested_size))
    {
        Some(end) if end <= entry.identity.len => end,
        _ => return error(),
    };
    let mut file = match File::open(&entry.path) {
        Ok(file) => file,
        Err(_) => return error(),
    };
    if file.seek(SeekFrom::Start(request.position)).is_err() {
        return error();
    }
    let opened_identity = match file.metadata() {
        Ok(metadata) if !metadata.file_type().is_symlink() => file_identity(&metadata),
        _ => return error(),
    };
    if opened_identity != entry.identity {
        return error();
    }
    let mut data = vec![0u8; request.requested_size as usize];
    if file.read_exact(&mut data).is_err() {
        return error();
    }
    let after_handle = match file.metadata() {
        Ok(metadata) if !metadata.file_type().is_symlink() => file_identity(&metadata),
        _ => return error(),
    };
    let after = match fs::symlink_metadata(&entry.path) {
        Ok(metadata) if !metadata.file_type().is_symlink() => file_identity(&metadata),
        _ => return error(),
    };
    if opened_identity != after_handle
        || after_handle != entry.identity
        || after != entry.identity
        || end > after_handle.len
    {
        return error();
    }
    OwnedFileContentsResponse::new_data_response(request.stream_id, data)
}

fn read_local_data(
    os: &mut dyn OsClipboard,
    format: ClipboardFormatId,
    max_image_bytes: usize,
) -> OwnedFormatDataResponse {
    match os.get_content() {
        Ok(ClipboardContent::Text(text)) if format == ClipboardFormatId::CF_UNICODETEXT => {
            OwnedFormatDataResponse::new_unicode_string(&text)
        }
        Ok(ClipboardContent::Text(text)) if format == ClipboardFormatId::CF_TEXT => {
            OwnedFormatDataResponse::new_string(&text)
        }
        Ok(ClipboardContent::Image {
            width,
            height,
            rgba,
        }) if format == ClipboardFormatId::CF_DIB || format == ClipboardFormatId::CF_DIBV5 => {
            match encode_dib(width, height, &rgba, max_image_bytes) {
                Ok(data) => OwnedFormatDataResponse::new_data(data),
                Err(error) => {
                    warn!(%error, "failed to encode local clipboard image; sending error response");
                    OwnedFormatDataResponse::new_error()
                }
            }
        }
        Ok(_) => {
            debug!(?format, "remote requested an unsupported clipboard format");
            OwnedFormatDataResponse::new_error()
        }
        Err(error) => {
            warn!(%error, "failed to read OS clipboard for remote's paste request; sending error response");
            OwnedFormatDataResponse::new_error()
        }
    }
}

fn apply_remote_data(
    os: &mut dyn OsClipboard,
    last_seen_fingerprint: &mut Option<[u8; 32]>,
    active_epoch: &AtomicU64,
    work_epoch: u64,
    format: ClipboardFormatId,
    data: &[u8],
    max_image_bytes: usize,
) -> Result<(), String> {
    let content = if format == ClipboardFormatId::CF_UNICODETEXT {
        ClipboardContent::Text(decode_utf16le_text(data))
    } else if format == ClipboardFormatId::CF_TEXT {
        ClipboardContent::Text(decode_ansi_text(data))
    } else if format == ClipboardFormatId::CF_DIB || format == ClipboardFormatId::CF_DIBV5 {
        let (width, height, rgba) = decode_dib(data, max_image_bytes)?;
        ClipboardContent::Image {
            width,
            height,
            rgba,
        }
    } else {
        return Err(format!("unsupported clipboard format {format:?}"));
    };

    // Decode may be expensive. Re-check lifecycle immediately before the external side effect
    // so a discarded channel or dropped bridge cannot normally write stale remote content.
    if active_epoch.load(Ordering::Acquire) != work_epoch {
        return Err("stale clipboard write cancelled after session lifecycle changed".to_string());
    }

    // Fingerprint before handing ownership to the OS adapter. Cloning a large RGBA image here
    // would briefly double its memory and add another full-buffer copy to every remote paste.
    let fingerprint = content_fingerprint(&content);
    if active_epoch.load(Ordering::Acquire) != work_epoch {
        return Err("stale clipboard write cancelled before OS mutation".to_string());
    }
    // `set_content` is an external blocking API and cannot be cancelled once entered. The
    // second check narrows cancellation to the final instruction boundary without making the
    // session thread wait on an OS clipboard lock during bounded teardown.
    os.set_content(content)?;
    // Seed echo suppression only after the OS confirms the write. A failed write must remain
    // observable to the next poll rather than being mistaken for applied remote content.
    *last_seen_fingerprint = Some(fingerprint);
    Ok(())
}

fn text_formats() -> Vec<ClipboardFormat> {
    vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]
}

fn image_formats() -> Vec<ClipboardFormat> {
    vec![ClipboardFormat::new(ClipboardFormatId::CF_DIB)]
}

fn best_content_format(
    formats: &[ClipboardFormat],
    allow_file_list: bool,
) -> Option<ClipboardFormatId> {
    if allow_file_list
        && formats.iter().any(|format| {
            format
                .name()
                .is_some_and(|name| name.value() == FORMAT_NAME_FILE_LIST)
        })
    {
        formats
            .iter()
            .find(|format| {
                format
                    .name()
                    .is_some_and(|name| name.value() == FORMAT_NAME_FILE_LIST)
            })
            .map(ClipboardFormat::id)
    } else if formats
        .iter()
        .any(|format| format.id() == ClipboardFormatId::CF_UNICODETEXT)
    {
        Some(ClipboardFormatId::CF_UNICODETEXT)
    } else if formats
        .iter()
        .any(|format| format.id() == ClipboardFormatId::CF_TEXT)
    {
        Some(ClipboardFormatId::CF_TEXT)
    } else if formats
        .iter()
        .any(|format| format.id() == ClipboardFormatId::CF_DIB)
    {
        Some(ClipboardFormatId::CF_DIB)
    } else if formats
        .iter()
        .any(|format| format.id() == ClipboardFormatId::CF_DIBV5)
    {
        Some(ClipboardFormatId::CF_DIBV5)
    } else {
        None
    }
}

/// Size-bounded fingerprint of clipboard content: a SHA-256 over at most the first
/// [`HASH_PREFIX_CAP_BYTES`] bytes, with the full byte length and content kind folded into the
/// same digest.
///
/// For text at or under the cap this is exactly equivalent to hashing the whole payload —
/// no behaviour change from the old full-hash for realistic clipboard content. Above the
/// cap, two different payloads of the same total length that share the same first
/// `HASH_PREFIX_CAP_BYTES` bytes are indistinguishable to this fingerprint: a local edit
/// that only changes content past the prefix, without changing the total length, will not
/// be detected as a new copy.
///
/// This is an accepted trade, not an oversight: it only bites clipboard content larger than
/// the cap that shares a huge common prefix and an unchanged length — a narrow case — and
/// the alternative is hashing every byte on every ~250ms poll for as long as that payload
/// sits on the clipboard, which is the latency-degrading defect this fingerprint exists to
/// fix. See the tests `single_poll_of_a_huge_payload_only_hashes_the_bounded_prefix`,
/// `repeated_polls_of_unchanged_huge_payload_cost_a_constant_capped_amount_each_time`, and
/// `a_tail_only_change_past_the_prefix_cap_with_unchanged_length_is_not_detected`.
fn content_fingerprint(content: &ClipboardContent) -> [u8; 32] {
    let mut hasher = Sha256::new();
    let prefix_len = match content {
        ClipboardContent::Text(text) => {
            hasher.update(b"text");
            hasher.update((0u64).to_le_bytes());
            hasher.update((0u64).to_le_bytes());
            let bytes = text.as_bytes();
            let prefix_len = bytes.len().min(HASH_PREFIX_CAP_BYTES);
            hasher.update(&bytes[..prefix_len]);
            hasher.update((bytes.len() as u64).to_le_bytes());
            prefix_len
        }
        ClipboardContent::Image {
            width,
            height,
            rgba,
        } => {
            hasher.update(b"image");
            hasher.update((*width as u64).to_le_bytes());
            hasher.update((*height as u64).to_le_bytes());
            let prefix_len = rgba.len().min(HASH_PREFIX_CAP_BYTES);
            hasher.update(&rgba[..prefix_len]);
            hasher.update((rgba.len() as u64).to_le_bytes());
            prefix_len
        }
        ClipboardContent::Files(paths) => {
            hasher.update(b"files");
            hasher.update((paths.len() as u64).to_le_bytes());
            let mut hashed = 0usize;
            for path in paths {
                let bytes = path.to_string_lossy();
                let bytes = bytes.as_bytes();
                let remaining = HASH_PREFIX_CAP_BYTES.saturating_sub(hashed);
                let take = bytes.len().min(remaining);
                hasher.update(&bytes[..take]);
                hasher.update((bytes.len() as u64).to_le_bytes());
                hashed += take;
                if hashed == HASH_PREFIX_CAP_BYTES {
                    break;
                }
            }
            hashed
        }
    };

    #[cfg(not(test))]
    let _ = prefix_len;

    #[cfg(test)]
    tests::HASHED_BYTES.with(|cell| cell.set(cell.get() + prefix_len));

    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    out
}

/// Decodes CLIPRDR `CF_UNICODETEXT` wire bytes (UTF-16LE, NUL-terminated) into a `String`.
/// Never panics: an odd trailing byte is dropped, all trailing NUL code units are trimmed,
/// and invalid sequences (including lone surrogates) are replaced with U+FFFD.
fn decode_utf16le_text(bytes: &[u8]) -> String {
    let mut units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    while units.last() == Some(&0) {
        units.pop();
    }
    String::from_utf16_lossy(&units)
}

/// Decodes CLIPRDR `CF_TEXT` wire bytes (single-byte, NUL-terminated) into a `String`.
/// Treated as Latin-1, which is a reasonable minimal decode for the inbound-only fallback
/// format; never panics.
fn decode_ansi_text(bytes: &[u8]) -> String {
    let mut end = bytes.len();
    while end > 0 && bytes[end - 1] == 0 {
        end -= 1;
    }
    bytes[..end].iter().map(|&byte| byte as char).collect()
}

/// Encodes a top-down RGBA image as a 32-bit `CF_DIB` (`BITMAPV5HEADER`, `BI_BITFIELDS`).
/// Explicit channel masks make alpha semantics unambiguous to clients that preserve it.
fn encode_dib(
    width: usize,
    height: usize,
    rgba: &[u8],
    max_bytes: usize,
) -> Result<Vec<u8>, String> {
    validate_image_dimensions(width, height, rgba.len())?;
    let pixel_bytes = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "clipboard image dimensions overflow".to_string())?;
    const DIB_HEADER_SIZE: usize = 124;
    let total = DIB_HEADER_SIZE
        .checked_add(pixel_bytes)
        .ok_or_else(|| "clipboard image is too large".to_string())?;
    if total > max_bytes {
        return Err("clipboard image exceeds the size limit".to_string());
    }

    let width_i32 = i32::try_from(width).map_err(|_| "clipboard image is too wide".to_string())?;
    let height_i32 =
        i32::try_from(height).map_err(|_| "clipboard image is too tall".to_string())?;
    let negative_height = height_i32
        .checked_neg()
        .ok_or_else(|| "clipboard image height is invalid".to_string())?;
    let image_size =
        u32::try_from(pixel_bytes).map_err(|_| "clipboard image is too large".to_string())?;

    let mut out = vec![0u8; total];
    put_u32(&mut out, 0, DIB_HEADER_SIZE as u32);
    put_i32(&mut out, 4, width_i32);
    put_i32(&mut out, 8, negative_height);
    put_u16(&mut out, 12, 1);
    put_u16(&mut out, 14, 32);
    put_u32(&mut out, 16, 3); // BI_BITFIELDS
    put_u32(&mut out, 20, image_size);
    put_u32(&mut out, 40, 0x00ff_0000); // red mask
    put_u32(&mut out, 44, 0x0000_ff00); // green mask
    put_u32(&mut out, 48, 0x0000_00ff); // blue mask
    put_u32(&mut out, 52, 0xff00_0000); // alpha mask
    put_u32(&mut out, 56, 0x7352_4742); // LCS_sRGB

    for (source, destination) in rgba
        .chunks_exact(4)
        .zip(out[DIB_HEADER_SIZE..].chunks_exact_mut(4))
    {
        destination[0] = source[2]; // B
        destination[1] = source[1]; // G
        destination[2] = source[0]; // R
        destination[3] = source[3]; // A (ignored by older Windows consumers)
    }
    Ok(out)
}

/// Decodes the safe, uncompressed subset of `CF_DIB` used by Windows and common desktop
/// applications. The result is RGBA, row-major, top-down.
fn decode_dib(data: &[u8], max_bytes: usize) -> Result<(usize, usize, Vec<u8>), String> {
    if data.len() > max_bytes {
        return Err("remote clipboard image exceeds the size limit".to_string());
    }
    if data.len() < 40 {
        return Err("remote clipboard image has a truncated DIB header".to_string());
    }
    let header_size =
        usize::try_from(get_u32(data, 0)?).map_err(|_| "invalid DIB header".to_string())?;
    if !(40..=124).contains(&header_size) || header_size > data.len() {
        return Err("remote clipboard image has an unsupported DIB header".to_string());
    }
    let width = get_i32(data, 4)?;
    let height = get_i32(data, 8)?;
    if width <= 0 || height == 0 || height == i32::MIN {
        return Err("remote clipboard image has invalid dimensions".to_string());
    }
    let width =
        usize::try_from(width).map_err(|_| "remote clipboard image is too wide".to_string())?;
    let top_down = height < 0;
    let height_abs = height.unsigned_abs();
    let height = usize::try_from(height_abs)
        .map_err(|_| "remote clipboard image is too tall".to_string())?;
    let planes = get_u16(data, 12)?;
    let bits_per_pixel = get_u16(data, 14)?;
    let compression = get_u32(data, 16)?;
    if planes != 1 || !matches!(bits_per_pixel, 24 | 32) {
        return Err("remote clipboard image uses an unsupported pixel format".to_string());
    }
    if compression != 0 && !(compression == 3 && bits_per_pixel == 32) {
        return Err("remote clipboard image uses unsupported compression".to_string());
    }
    let row_bytes = width
        .checked_mul(usize::from(bits_per_pixel) / 8)
        .and_then(|bytes| bytes.checked_add(3))
        .map(|bytes| bytes & !3)
        .ok_or_else(|| "remote clipboard image dimensions overflow".to_string())?;
    let pixel_bytes = row_bytes
        .checked_mul(height)
        .ok_or_else(|| "remote clipboard image dimensions overflow".to_string())?;
    validate_image_dimensions(
        width,
        height,
        width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .unwrap_or(usize::MAX),
    )?;

    let pixel_offset = if compression == 3 && header_size == 40 {
        52usize
    } else {
        header_size
    };
    let pixel_end = pixel_offset
        .checked_add(pixel_bytes)
        .ok_or_else(|| "remote clipboard image dimensions overflow".to_string())?;
    if pixel_offset > data.len() || pixel_end > data.len() {
        return Err("remote clipboard image pixel data is truncated".to_string());
    }

    let (r_mask, g_mask, b_mask, a_mask) = if compression == 3 {
        let mask_offset = 40;
        let r = get_u32(data, mask_offset)?;
        let g = get_u32(data, mask_offset + 4)?;
        let b = get_u32(data, mask_offset + 8)?;
        let a = if header_size >= 56 {
            get_u32(data, mask_offset + 12)?
        } else {
            0
        };
        validate_masks(r, g, b, a)?;
        (r, g, b, a)
    } else {
        (0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0)
    };

    let output_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "remote clipboard image dimensions overflow".to_string())?;
    let mut rgba = vec![0u8; output_len];
    for source_row in 0..height {
        let destination_row = if top_down {
            source_row
        } else {
            height - 1 - source_row
        };
        let row_start = pixel_offset + source_row * row_bytes;
        for x in 0..width {
            let source = row_start + x * (usize::from(bits_per_pixel) / 8);
            let pixel = u32::from_le_bytes([
                data[source],
                data[source + 1],
                data[source + 2],
                if bits_per_pixel == 32 {
                    data[source + 3]
                } else {
                    0
                },
            ]);
            let destination = (destination_row * width + x) * 4;
            if compression == 0 {
                rgba[destination] = data[source + 2];
                rgba[destination + 1] = data[source + 1];
                rgba[destination + 2] = data[source];
                rgba[destination + 3] = 255;
            } else {
                rgba[destination] = mask_component(pixel, r_mask);
                rgba[destination + 1] = mask_component(pixel, g_mask);
                rgba[destination + 2] = mask_component(pixel, b_mask);
                rgba[destination + 3] = if a_mask == 0 {
                    255
                } else {
                    mask_component(pixel, a_mask)
                };
            }
        }
    }
    Ok((width, height, rgba))
}

fn validate_image_dimensions(width: usize, height: usize, rgba_len: usize) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION as usize
        || height > MAX_IMAGE_DIMENSION as usize
    {
        return Err("clipboard image exceeds safe size limits".to_string());
    }
    let pixels = u64::try_from(width)
        .ok()
        .and_then(|w| u64::try_from(height).ok().map(|h| w.saturating_mul(h)))
        .ok_or_else(|| "clipboard image dimensions overflow".to_string())?;
    if pixels > MAX_IMAGE_PIXELS
        || rgba_len
            != usize::try_from(pixels)
                .unwrap_or(usize::MAX)
                .saturating_mul(4)
    {
        return Err("clipboard image exceeds safe size limits".to_string());
    }
    Ok(())
}

fn validate_masks(r: u32, g: u32, b: u32, a: u32) -> Result<(), String> {
    if r == 0
        || g == 0
        || b == 0
        || (r & g) != 0
        || (r & b) != 0
        || (g & b) != 0
        || (a & (r | g | b)) != 0
    {
        return Err("remote clipboard image has invalid channel masks".to_string());
    }
    Ok(())
}

fn mask_component(value: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = mask.count_ones();
    let component = (value & mask) >> shift;
    let max = if bits == 32 {
        u32::MAX
    } else {
        (1u32 << bits) - 1
    };
    ((u64::from(component) * 255 + u64::from(max) / 2) / u64::from(max.max(1))) as u8
}

fn get_u16(data: &[u8], offset: usize) -> Result<u16, String> {
    let end = offset
        .checked_add(2)
        .ok_or_else(|| "invalid DIB header".to_string())?;
    let bytes = data
        .get(offset..end)
        .ok_or_else(|| "remote clipboard image has a truncated DIB header".to_string())?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn get_u32(data: &[u8], offset: usize) -> Result<u32, String> {
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "invalid DIB header".to_string())?;
    let bytes = data
        .get(offset..end)
        .ok_or_else(|| "remote clipboard image has a truncated DIB header".to_string())?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn get_i32(data: &[u8], offset: usize) -> Result<i32, String> {
    Ok(i32::from_le_bytes(get_u32(data, offset)?.to_le_bytes()))
}

fn put_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

fn put_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_i32(data: &mut [u8], offset: usize, value: i32) {
    put_u32(data, offset, value as u32);
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::Condvar;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;
    use std::time::{Duration, Instant};

    use ironrdp_cliprdr::pdu::{ClipboardPdu, FormatListResponse};
    use ironrdp_cliprdr::{Client, CliprdrClient};
    use ironrdp_svc::pdu::{Decode as _, ReadCursor};
    use ironrdp_svc::{SvcMessage, SvcProcessor};

    use super::*;

    thread_local! {
        /// Total bytes fed to the hasher inside [`content_fingerprint`], across every call
        /// made on the current test thread. `cargo test` runs each `#[test]` fn on its own
        /// thread by default, so this is effectively per-test despite being `thread_local`
        /// rather than per-instance — see [`reset_hashed_bytes`] / [`hashed_bytes`].
        pub(crate) static HASHED_BYTES: Cell<usize> = const { Cell::new(0) };
    }

    fn reset_hashed_bytes() {
        HASHED_BYTES.with(|cell| cell.set(0));
    }

    fn hashed_bytes() -> usize {
        HASHED_BYTES.with(|cell| cell.get())
    }

    #[derive(Default)]
    struct FakeClipboardState {
        text: Option<String>,
        image: Option<(usize, usize, Vec<u8>)>,
        files: Option<Vec<PathBuf>>,
        fail_next_get: bool,
        fail_next_set: bool,
        /// How many times the clipboard content was read — the observable proxy for
        /// the "IPC round trip + full allocation" cost `poll_local_change` pays every poll.
        get_text_calls: usize,
    }

    struct FakeOsClipboard(Arc<Mutex<FakeClipboardState>>);

    impl OsClipboard for FakeOsClipboard {
        fn get_content(&mut self) -> Result<ClipboardContent, String> {
            let mut state = self.0.lock().unwrap();
            state.get_text_calls += 1;
            if state.fail_next_get {
                state.fail_next_get = false;
                return Err("fake read failure".to_string());
            }
            if let Some(text) = state.text.clone() {
                Ok(ClipboardContent::Text(text))
            } else if let Some((width, height, rgba)) = state.image.clone() {
                Ok(ClipboardContent::Image {
                    width,
                    height,
                    rgba,
                })
            } else if let Some(files) = state.files.clone() {
                Ok(ClipboardContent::Files(files))
            } else {
                // An empty text clipboard is still a valid clipboard value and must be
                // advertised, rather than treated as an inaccessible pasteboard.
                Ok(ClipboardContent::Text(String::new()))
            }
        }

        fn set_content(&mut self, content: ClipboardContent) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            if state.fail_next_set {
                state.fail_next_set = false;
                return Err("fake write failure".to_string());
            }
            match content {
                ClipboardContent::Text(text) => {
                    state.text = Some(text);
                    state.image = None;
                    state.files = None;
                }
                ClipboardContent::Image {
                    width,
                    height,
                    rgba,
                } => {
                    state.text = None;
                    state.image = Some((width, height, rgba));
                    state.files = None;
                }
                ClipboardContent::Files(files) => {
                    state.text = None;
                    state.image = None;
                    state.files = Some(files);
                }
            }
            Ok(())
        }

        fn get_file_list(&mut self) -> Result<Vec<PathBuf>, String> {
            Ok(self.0.lock().unwrap().files.clone().unwrap_or_default())
        }

        fn set_file_list(&mut self, paths: &[PathBuf]) -> Result<(), String> {
            let mut state = self.0.lock().unwrap();
            state.text = None;
            state.image = None;
            state.files = Some(paths.to_vec());
            Ok(())
        }
    }

    fn fake_clipboard() -> (Arc<Mutex<FakeClipboardState>>, Box<dyn OsClipboard>) {
        let state = Arc::new(Mutex::new(FakeClipboardState::default()));
        (state.clone(), Box::new(FakeOsClipboard(state)))
    }

    struct BlockingClipboard {
        started: Arc<(Mutex<bool>, Condvar)>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl OsClipboard for BlockingClipboard {
        fn get_content(&mut self) -> Result<ClipboardContent, String> {
            let (lock, wake) = &*self.started;
            *lock.lock().unwrap() = true;
            wake.notify_all();

            let (lock, wake) = &*self.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            Ok(ClipboardContent::Text("blocked clipboard".to_string()))
        }

        fn set_content(&mut self, _content: ClipboardContent) -> Result<(), String> {
            Ok(())
        }
    }

    struct GatedClipboard {
        started: Arc<(Mutex<bool>, Condvar)>,
        release: Arc<(Mutex<bool>, Condvar)>,
        content: Arc<Mutex<ClipboardContent>>,
    }

    impl OsClipboard for GatedClipboard {
        fn get_content(&mut self) -> Result<ClipboardContent, String> {
            let (lock, wake) = &*self.started;
            *lock.lock().unwrap() = true;
            wake.notify_all();

            let (lock, wake) = &*self.release;
            let mut released = lock.lock().unwrap();
            while !*released {
                released = wake.wait(released).unwrap();
            }
            Ok(self.content.lock().unwrap().clone())
        }

        fn set_content(&mut self, content: ClipboardContent) -> Result<(), String> {
            *self.content.lock().unwrap() = content;
            Ok(())
        }
    }

    #[test]
    fn production_pump_does_not_wait_for_a_blocked_os_read() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (backend, mut bridge) = clipboard_channel(Box::new(BlockingClipboard {
            started: started.clone(),
            release: release.clone(),
        }));
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr); // process the synthetic ready acknowledgement
        backend_mut(&mut cliprdr).on_request_format_list();

        let (done_tx, done_rx) = mpsc::channel();
        let caller = thread::spawn(move || {
            let started_at = Instant::now();
            bridge.pump(&mut cliprdr);
            done_tx.send(started_at.elapsed()).unwrap();
            (bridge, cliprdr)
        });

        let (lock, wake) = &*started;
        let mut did_start = lock.lock().unwrap();
        let started_deadline = Instant::now() + Duration::from_secs(1);
        while !*did_start && Instant::now() < started_deadline {
            let remaining = started_deadline.saturating_duration_since(Instant::now());
            let (next, _) = wake.wait_timeout(did_start, remaining).unwrap();
            did_start = next;
        }
        assert!(*did_start, "blocking fake did not enter get_content");
        drop(did_start);

        let returned = done_rx.recv_timeout(Duration::from_millis(100)).ok();

        let (lock, wake) = &*release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
        let (_bridge, _cliprdr) = caller.join().expect("pump caller thread");

        assert!(
            returned.is_some(),
            "production pump waited for the blocked OS clipboard read"
        );
    }

    #[test]
    fn production_worker_completion_rings_the_session_doorbell() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (bell, wake_rx) = crate::wake::doorbell().unwrap();
        let listener = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let socket = std::net::TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (_peer, _) = listener.accept().unwrap();

        let (backend, mut bridge) = clipboard_channel_with_waker(
            Box::new(BlockingClipboard {
                started: started.clone(),
                release: release.clone(),
            }),
            bell,
        );
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        wake_rx.drain();
        backend_mut(&mut cliprdr).on_request_format_list();
        bridge.pump(&mut cliprdr);
        wait_for_blocking_read(&started);

        release_blocking_read(&release);
        let ready = crate::wake::wait_readable(&socket, &wake_rx, Duration::from_secs(1)).unwrap();
        assert!(ready.bell, "worker result did not wake the session pump");
    }

    #[test]
    fn a_second_poll_during_a_slow_read_does_not_lose_the_change() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (backend, mut bridge) = clipboard_channel(Box::new(BlockingClipboard {
            started: started.clone(),
            release: release.clone(),
        }));
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr); // process the synthetic ready acknowledgement

        bridge.poll_local_change();
        wait_for_blocking_read(&started);
        bridge.poll_local_change(); // must coalesce instead of superseding the changed result
        release_blocking_read(&release);

        let deadline = Instant::now() + Duration::from_secs(1);
        let mut messages = Vec::new();
        while messages.is_empty() && Instant::now() < deadline {
            messages.extend(bridge.pump(&mut cliprdr));
            thread::yield_now();
        }
        assert!(
            !messages.is_empty(),
            "the first changed result must still advertise after a coalesced poll"
        );
    }

    #[test]
    fn pump_limits_work_and_continues_reliable_actions_in_fifo_order() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr); // drain the bootstrap acknowledgement

        let total = MAX_CLIPBOARD_ACTIONS_PER_PUMP + 5;
        let expected: Vec<_> = (0..total)
            .map(|index| {
                if index % 2 == 0 {
                    ClipboardFormatId::CF_UNICODETEXT
                } else {
                    ClipboardFormatId::CF_TEXT
                }
            })
            .collect();
        for format in &expected {
            backend_mut(&mut cliprdr).on_remote_copy(&[ClipboardFormat::new(*format)]);
        }

        let (first, first_batch_full) = bridge.pump_bounded(&mut cliprdr);
        assert!(
            first_batch_full,
            "the session must immediately revisit work left by a full pump"
        );
        assert_eq!(
            first.len(),
            MAX_CLIPBOARD_ACTIONS_PER_PUMP,
            "one pump must stop at the named action/result cap"
        );
        let first_formats: Vec<_> = first.into_iter().map(paste_format).collect();
        assert_eq!(first_formats, expected[..MAX_CLIPBOARD_ACTIONS_PER_PUMP]);

        let (second, second_batch_full) = bridge.pump_bounded(&mut cliprdr);
        assert!(
            !second_batch_full,
            "the short tail may return to idle sleep"
        );
        assert_eq!(second.len(), total - MAX_CLIPBOARD_ACTIONS_PER_PUMP);
        let second_formats: Vec<_> = second.into_iter().map(paste_format).collect();
        assert_eq!(second_formats, expected[MAX_CLIPBOARD_ACTIONS_PER_PUMP..]);
        assert!(bridge.pump(&mut cliprdr).is_empty());
    }

    #[test]
    fn saturated_local_data_request_gets_an_immediate_error_response() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (backend, mut bridge) = clipboard_channel(Box::new(BlockingClipboard {
            started: started.clone(),
            release: release.clone(),
        }));
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        bridge.poll_local_change();
        wait_for_blocking_read(&started);
        // The poll is in the OS call. A format-list read occupies the one-deep command queue,
        // so a reliable data request arriving now must receive an explicit error.
        backend_mut(&mut cliprdr).on_request_format_list();
        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        let messages = bridge.pump(&mut cliprdr);
        match only_pdu(messages) {
            ClipboardPdu::FormatDataResponse(response) => {
                assert!(
                    response.is_error(),
                    "saturated local read must return an error PDU"
                );
            }
            other => panic!("unexpected PDU: {other:?}"),
        }

        release_blocking_read(&release);
        drop(bridge);
    }

    #[test]
    fn saturated_format_read_is_retried_until_the_advertise_reaches_the_wire() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (backend, mut bridge) = clipboard_channel(Box::new(BlockingClipboard {
            started: started.clone(),
            release: release.clone(),
        }));
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        bridge.poll_local_change();
        wait_for_blocking_read(&started);
        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        bridge.pump(&mut cliprdr); // queue one reliable read behind the blocked poll
        bridge
            .local_pending
            .push_back(ClipboardAction::AdvertiseRequested);
        bridge.pump(&mut cliprdr);
        assert!(
            bridge.deferred_format_read,
            "the full queue must retain the advertise"
        );

        release_blocking_read(&release);
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut advertised = false;
        while !advertised && Instant::now() < deadline {
            advertised |= contains_format_list(bridge.pump(&mut cliprdr));
            thread::yield_now();
        }
        assert!(
            advertised,
            "the retained format read never reached the wire"
        );
    }

    #[test]
    fn a_busy_worker_defers_remote_paste_and_does_not_echo_it_back() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let content = Arc::new(Mutex::new(ClipboardContent::Text("local".to_string())));
        let (backend, mut bridge) = clipboard_channel(Box::new(GatedClipboard {
            started: started.clone(),
            release: release.clone(),
            content: content.clone(),
        }));
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        bridge.poll_local_change();
        wait_for_blocking_read(&started);
        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        bridge.pump(&mut cliprdr); // occupy the command queue behind the blocked poll

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("remote"));
        bridge.pump(&mut cliprdr);
        assert!(
            bridge.deferred_remote_apply.is_some(),
            "a busy worker must retain the remote paste"
        );

        release_blocking_read(&release);
        let deadline = Instant::now() + Duration::from_secs(1);
        while *content.lock().unwrap() != ClipboardContent::Text("remote".to_string())
            && Instant::now() < deadline
        {
            bridge.pump(&mut cliprdr);
            thread::yield_now();
        }
        assert_eq!(
            *content.lock().unwrap(),
            ClipboardContent::Text("remote".to_string())
        );

        bridge.poll_local_change();
        let mut echoed = false;
        let deadline = Instant::now() + Duration::from_secs(1);
        while bridge.pending_poll_id.is_some() && Instant::now() < deadline {
            echoed |= contains_format_list(bridge.pump(&mut cliprdr));
            thread::yield_now();
        }
        assert!(!echoed, "remote-origin content must not be advertised back");
    }

    #[test]
    fn a_new_remote_response_removes_older_data_that_was_only_deferred() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        bridge.deferred_remote_apply = Some(ClipboardWork::ApplyRemote {
            id: 41,
            epoch: bridge.epoch,
            observation_generation: bridge.observation_generation,
            format: ClipboardFormatId::CF_UNICODETEXT,
            data: OwnedFormatDataResponse::new_unicode_string("old")
                .into_data()
                .into_owned(),
            max_image_bytes: bridge.max_image_bytes,
        });
        bridge.paste_state = PasteState::Requested {
            format: ClipboardFormatId::CF_UNICODETEXT,
            requested_at_ms: 0,
        };
        bridge.pending_paste_id = Some(42);
        bridge.handle_remote_data_received(OwnedFormatDataResponse::new_unicode_string("new"));
        bridge.pump(&mut cliprdr);

        assert!(bridge.deferred_remote_apply.is_none());
        assert_eq!(state.lock().unwrap().text.as_deref(), Some("new"));
    }

    #[test]
    fn discarding_a_channel_cancels_its_queued_remote_os_write() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let content = Arc::new(Mutex::new(ClipboardContent::Text("local".to_string())));
        let (backend, mut bridge) = clipboard_channel(Box::new(GatedClipboard {
            started: started.clone(),
            release: release.clone(),
            content: content.clone(),
        }));
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        bridge.poll_local_change();
        wait_for_blocking_read(&started);
        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("stale remote"));
        bridge.pump(&mut cliprdr); // ApplyRemote is queued behind the blocked poll
        bridge.discard_pending();
        release_blocking_read(&release);

        backend_mut(&mut cliprdr).on_request_format_list();
        let deadline = Instant::now() + Duration::from_secs(1);
        let mut new_lifecycle_completed = false;
        while !new_lifecycle_completed && Instant::now() < deadline {
            new_lifecycle_completed |= contains_format_list(bridge.pump(&mut cliprdr));
            thread::yield_now();
        }
        assert!(
            new_lifecycle_completed,
            "new clipboard lifecycle did not recover"
        );
        assert_eq!(
            *content.lock().unwrap(),
            ClipboardContent::Text("local".to_string()),
            "discarded channel work must not mutate the OS clipboard"
        );
    }

    #[test]
    fn dropping_a_blocked_production_worker_is_bounded_and_detaches() {
        let started = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let (backend, mut bridge) = clipboard_channel(Box::new(BlockingClipboard {
            started: started.clone(),
            release: release.clone(),
        }));
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr); // enable local polling
        bridge.poll_local_change();
        wait_for_blocking_read(&started);

        let started_at = Instant::now();
        drop(bridge);
        assert!(
            started_at.elapsed() < Duration::from_millis(250),
            "blocked worker teardown exceeded its bounded wait"
        );

        // Let the detached worker finish its fake OS call and observe the queued stop. The
        // release intentionally happens after the bridge has been dropped.
        release_blocking_read(&release);
        thread::sleep(Duration::from_millis(20));
    }

    fn wait_for_blocking_read(started: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, wake) = &**started;
        let mut did_start = lock.lock().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !*did_start && Instant::now() < deadline {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let (next, _) = wake.wait_timeout(did_start, remaining).unwrap();
            did_start = next;
        }
        assert!(*did_start, "blocking fake did not enter get_content");
    }

    fn release_blocking_read(release: &Arc<(Mutex<bool>, Condvar)>) {
        let (lock, wake) = &**release;
        *lock.lock().unwrap() = true;
        wake.notify_all();
    }

    struct FakeClock(AtomicU64);

    impl FakeClock {
        fn new() -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(0)))
        }

        fn advance(&self, ms: u64) {
            self.0.fetch_add(ms, Ordering::SeqCst);
        }
    }

    impl ClipboardClock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    /// Builds a `CliprdrClient` wrapping `backend` and drives it straight to `Ready` state by
    /// feeding it one real, wire-encoded `FormatListResponse::Ok` PDU — no socket involved,
    /// just the same encode/decode path the real client would use.
    fn ready_client(backend: ClipboardBackend) -> CliprdrClient {
        let mut cliprdr = CliprdrClient::new(Box::new(backend));
        let bytes =
            ironrdp_svc::pdu::encode_vec(&ClipboardPdu::FormatListResponse(FormatListResponse::Ok))
                .expect("encode FormatListResponse::Ok");
        SvcProcessor::process(&mut cliprdr, &bytes).expect("process FormatListResponse::Ok");
        cliprdr
    }

    #[test]
    fn discarding_pending_actions_empties_the_queue_and_returns_to_idle() {
        // A server that never joins CLIPRDR still leaves the local poll queueing
        // advertises. If they are not drained, the channel grows for the life of the
        // session for a channel that does not exist.
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);

        // Queue work from both sources the bridge drains: the backend's channel...
        backend_mut(&mut cliprdr).on_request_format_list();
        // ...and the local-change poll.
        state.lock().unwrap().text = Some("something copied locally".to_string());
        bridge.poll_local_change();

        bridge.discard_pending();

        let out = bridge.pump(&mut cliprdr);
        assert!(
            out.is_empty(),
            "discarded actions must not still be waiting to be sent"
        );
    }

    #[test]
    fn a_discard_does_not_stop_the_clipboard_working_later() {
        // Discarding is not a kill switch: if the channel appears later, or the poll runs
        // again, the next change must still be advertised.
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);

        state.lock().unwrap().text = Some("first".to_string());
        bridge.poll_local_change();
        bridge.discard_pending();

        // A newly available channel starts a fresh Monitor Ready handshake.
        backend_mut(&mut cliprdr).on_request_format_list();
        state.lock().unwrap().text = Some("second".to_string());
        bridge.poll_local_change();
        assert!(
            !bridge.pump(&mut cliprdr).is_empty(),
            "a later change must still be advertised"
        );
    }

    fn backend_mut(cliprdr: &mut CliprdrClient) -> &mut ClipboardBackend {
        cliprdr
            .downcast_backend_mut::<ClipboardBackend>()
            .expect("backend type")
    }

    fn unicode_format() -> ClipboardFormat {
        ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)
    }

    fn only_pdu(msgs: Vec<CliprdrSvcMessages<Client>>) -> ClipboardPdu<'static> {
        let mut svc_messages: Vec<SvcMessage> = Vec::new();
        for group in msgs {
            svc_messages.extend(Vec::<SvcMessage>::from(group));
        }
        assert_eq!(svc_messages.len(), 1, "expected exactly one wire PDU");
        let bytes = svc_messages[0]
            .encode_unframed_pdu()
            .expect("encode wire PDU");
        let mut cursor = ReadCursor::new(&bytes);
        match ClipboardPdu::decode(&mut cursor).expect("decode wire PDU") {
            ClipboardPdu::FormatDataResponse(response) => {
                ClipboardPdu::FormatDataResponse(response.into_owned())
            }
            other => panic!("unexpected pdu variant: {other:?}"),
        }
    }

    fn paste_format(msgs: CliprdrSvcMessages<Client>) -> ClipboardFormatId {
        let mut svc_messages: Vec<SvcMessage> = msgs.into();
        assert_eq!(svc_messages.len(), 1, "expected exactly one paste PDU");
        let bytes = svc_messages
            .pop()
            .expect("paste PDU")
            .encode_unframed_pdu()
            .expect("encode paste PDU");
        let mut cursor = ReadCursor::new(&bytes);
        match ClipboardPdu::decode(&mut cursor).expect("decode paste PDU") {
            ClipboardPdu::FormatDataRequest(request) => request.format,
            other => panic!("unexpected paste PDU: {other:?}"),
        }
    }

    fn contains_format_list(msgs: Vec<CliprdrSvcMessages<Client>>) -> bool {
        msgs.into_iter().any(|group| {
            Vec::<SvcMessage>::from(group).into_iter().any(|message| {
                let bytes = message.encode_unframed_pdu().expect("encode wire PDU");
                let mut cursor = ReadCursor::new(&bytes);
                matches!(
                    ClipboardPdu::decode(&mut cursor).expect("decode wire PDU"),
                    ClipboardPdu::FormatList(_)
                )
            })
        })
    }

    fn only_format_ids(msgs: Vec<CliprdrSvcMessages<Client>>) -> Vec<ClipboardFormatId> {
        let mut svc_messages: Vec<SvcMessage> = Vec::new();
        for group in msgs {
            svc_messages.extend(Vec::<SvcMessage>::from(group));
        }
        assert_eq!(svc_messages.len(), 1, "expected exactly one wire PDU");
        let bytes = svc_messages[0]
            .encode_unframed_pdu()
            .expect("encode wire PDU");
        let mut cursor = ReadCursor::new(&bytes);
        match ClipboardPdu::decode(&mut cursor).expect("decode wire PDU") {
            ClipboardPdu::FormatList(list) => list
                .get_formats(true)
                .expect("decode advertised format list")
                .into_iter()
                .map(|format| format.id())
                .collect(),
            other => panic!("unexpected pdu variant: {other:?}"),
        }
    }

    fn test_image() -> (usize, usize, Vec<u8>) {
        (
            2,
            2,
            vec![
                255, 0, 0, 255, // red, green
                0, 255, 0, 128, // blue, white
                0, 0, 255, 255, 255, 255, 255, 0,
            ],
        )
    }

    #[test]
    fn empty_local_text_is_advertised_and_served_as_empty_unicode() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        // Empty is valid text, not an inaccessible clipboard.
        state.lock().unwrap().text = Some(String::new());
        backend_mut(&mut cliprdr).on_request_format_list();
        let formats = only_format_ids(bridge.pump(&mut cliprdr));
        assert_eq!(formats, vec![ClipboardFormatId::CF_UNICODETEXT]);

        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        match only_pdu(bridge.pump(&mut cliprdr)) {
            ClipboardPdu::FormatDataResponse(response) => {
                assert!(!response.is_error());
                assert_eq!(response.data(), &[0, 0]);
            }
            other => panic!("unexpected pdu variant: {other:?}"),
        }
    }

    #[test]
    fn image_is_advertised_when_no_text_exists_and_unicode_still_wins() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        let image = test_image();
        state.lock().unwrap().image = Some(image.clone());

        backend_mut(&mut cliprdr).on_request_format_list();
        assert_eq!(
            only_format_ids(bridge.pump(&mut cliprdr)),
            vec![ClipboardFormatId::CF_DIB]
        );

        // If both are offered, text remains the stable preferred format.
        backend_mut(&mut cliprdr).on_remote_copy(&[
            ClipboardFormat::new(ClipboardFormatId::CF_DIB),
            unicode_format(),
        ]);
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1);
        assert_eq!(
            bridge.paste_state,
            PasteState::Requested {
                format: ClipboardFormatId::CF_UNICODETEXT,
                requested_at_ms: 0
            }
        );
    }

    #[test]
    fn exact_2x2_rgba_local_wire_and_decode_round_trip() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        let image = test_image();
        state.lock().unwrap().image = Some(image.clone());

        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_DIB,
        });
        let wire = match only_pdu(bridge.pump(&mut cliprdr)) {
            ClipboardPdu::FormatDataResponse(response) => response.data().to_vec(),
            other => panic!("unexpected pdu variant: {other:?}"),
        };
        assert_eq!(
            get_u32(&wire, 56).expect("V5 color space"),
            0x7352_4742,
            "BITMAPV5HEADER must declare sRGB rather than zeroed calibrated-RGB metadata"
        );
        assert_eq!(
            decode_dib(&wire, MAX_IMAGE_BYTES).expect("decode local DIB"),
            image
        );
    }

    #[test]
    fn ordinary_32_bit_bi_rgb_dib_treats_reserved_byte_as_opaque() {
        let mut dib = vec![0u8; 44];
        put_u32(&mut dib, 0, 40);
        put_i32(&mut dib, 4, 1);
        put_i32(&mut dib, 8, -1);
        put_u16(&mut dib, 12, 1);
        put_u16(&mut dib, 14, 32);
        // B, G, R, reserved byte. BI_RGB does not declare an alpha channel.
        dib[40..44].copy_from_slice(&[3, 2, 1, 0]);
        assert_eq!(
            decode_dib(&dib, MAX_IMAGE_BYTES).expect("decode BI_RGB"),
            (1, 1, vec![1, 2, 3, 255])
        );
    }

    #[test]
    fn remote_image_is_written_to_os_and_not_looped_back() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        let image = test_image();
        let wire =
            encode_dib(image.0, image.1, &image.2, MAX_IMAGE_BYTES).expect("encode test DIB");

        backend_mut(&mut cliprdr)
            .on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_DIB)]);
        assert_eq!(bridge.pump(&mut cliprdr).len(), 1);
        backend_mut(&mut cliprdr).on_format_data_response(FormatDataResponse::new_data(wire));
        bridge.pump(&mut cliprdr);

        assert_eq!(state.lock().unwrap().image, Some(image));
        bridge.poll_local_change();
        assert!(bridge.pump(&mut cliprdr).is_empty());
    }

    #[test]
    fn malformed_remote_image_is_rejected_and_next_copy_recovers() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr)
            .on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_DIB)]);
        assert_eq!(bridge.pump(&mut cliprdr).len(), 1);
        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_data(vec![1, 2, 3]));
        bridge.pump(&mut cliprdr);
        assert_eq!(bridge.paste_state, PasteState::Idle);

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        assert_eq!(bridge.pump(&mut cliprdr).len(), 1);
    }

    #[test]
    fn oversized_remote_image_is_rejected_without_allocating_pixels() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        let mut oversized = vec![0u8; 40];
        put_u32(&mut oversized, 0, 40);
        put_i32(&mut oversized, 4, (MAX_IMAGE_DIMENSION + 1) as i32);
        put_i32(&mut oversized, 8, 1);
        put_u16(&mut oversized, 12, 1);
        put_u16(&mut oversized, 14, 32);
        backend_mut(&mut cliprdr)
            .on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::CF_DIB)]);
        assert_eq!(bridge.pump(&mut cliprdr).len(), 1);
        backend_mut(&mut cliprdr).on_format_data_response(FormatDataResponse::new_data(oversized));
        bridge.pump(&mut cliprdr);
        assert_eq!(bridge.paste_state, PasteState::Idle);

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        assert_eq!(bridge.pump(&mut cliprdr).len(), 1);
    }

    #[test]
    fn image_fingerprint_is_content_sensitive_and_bounded() {
        let (width, height, rgba) = test_image();
        let same = ClipboardContent::Image {
            width,
            height,
            rgba: rgba.clone(),
        };
        let mut changed_rgba = rgba;
        changed_rgba[0] ^= 1;
        let changed = ClipboardContent::Image {
            width,
            height,
            rgba: changed_rgba,
        };
        assert_eq!(content_fingerprint(&same), content_fingerprint(&same));
        assert_ne!(content_fingerprint(&same), content_fingerprint(&changed));
        reset_hashed_bytes();
        let huge = ClipboardContent::Image {
            width: 1,
            height: LARGE_PAYLOAD_BYTES / 4,
            rgba: vec![0; LARGE_PAYLOAD_BYTES],
        };
        let _ = content_fingerprint(&huge);
        assert_eq!(hashed_bytes(), HASH_PREFIX_CAP_BYTES);
    }

    #[test]
    fn remote_copy_triggers_paste_request_then_writes_local_clipboard_on_response() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr); // drain the bootstrap FormatListAcked(true)

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1, "expected exactly one paste request");
        assert_eq!(
            bridge.paste_state,
            PasteState::Requested {
                format: ClipboardFormatId::CF_UNICODETEXT,
                requested_at_ms: 0,
            }
        );

        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("hello from remote"));
        bridge.pump(&mut cliprdr);

        assert_eq!(bridge.paste_state, PasteState::Idle);
        assert_eq!(
            state.lock().unwrap().text.as_deref(),
            Some("hello from remote")
        );
    }

    /// The regression test for "stops working after a bit": two independent copy/paste
    /// cycles back to back, proving the state machine actually returns to Idle in between.
    #[test]
    fn two_consecutive_copy_paste_cycles_both_succeed() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        for (cycle, text) in ["first copy", "second copy"].into_iter().enumerate() {
            backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
            let msgs = bridge.pump(&mut cliprdr);
            assert_eq!(
                msgs.len(),
                1,
                "cycle {cycle}: expected a paste request to be sent"
            );
            assert!(
                matches!(bridge.paste_state, PasteState::Requested { .. }),
                "cycle {cycle}: expected Requested state"
            );

            backend_mut(&mut cliprdr)
                .on_format_data_response(FormatDataResponse::new_unicode_string(text));
            bridge.pump(&mut cliprdr);

            assert_eq!(
                bridge.paste_state,
                PasteState::Idle,
                "cycle {cycle}: must return to Idle so the next copy works"
            );
            assert_eq!(
                state.lock().unwrap().text.as_deref(),
                Some(text),
                "cycle {cycle}: OS clipboard should hold the new text"
            );
        }
    }

    #[test]
    fn format_list_rejection_retries_are_bounded() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_request_format_list();
        bridge.pump(&mut cliprdr);
        assert_eq!(
            bridge.advertise_state,
            AdvertiseState::Pending {
                attempt: 0,
                resend_requested: false,
            }
        );

        backend_mut(&mut cliprdr).on_format_list_response(false);
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1, "first rejection should retry");
        assert_eq!(
            bridge.advertise_state,
            AdvertiseState::Pending {
                attempt: 1,
                resend_requested: false,
            }
        );

        backend_mut(&mut cliprdr).on_format_list_response(false);
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1, "second rejection should retry");
        assert_eq!(
            bridge.advertise_state,
            AdvertiseState::Pending {
                attempt: 2,
                resend_requested: false,
            }
        );

        backend_mut(&mut cliprdr).on_format_list_response(false);
        let msgs = bridge.pump(&mut cliprdr);
        assert!(msgs.is_empty(), "must give up rather than retry forever");
        assert_eq!(bridge.advertise_state, AdvertiseState::Idle);
    }

    #[test]
    fn format_list_rejection_never_clears_a_prior_confirmation() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_request_format_list();
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr).on_format_list_response(true);
        bridge.pump(&mut cliprdr);
        assert_eq!(bridge.advertise_state, AdvertiseState::Confirmed);

        // A stray/late rejection must not undo the confirmed state.
        backend_mut(&mut cliprdr).on_format_list_response(false);
        bridge.pump(&mut cliprdr);
        assert_eq!(bridge.advertise_state, AdvertiseState::Confirmed);
    }

    #[test]
    fn from_remote_off_never_writes_the_os_clipboard() {
        let (state, os) = fake_clipboard();
        let (backend, bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut bridge = bridge.with_policy(ClipboardPolicy {
            from_remote: false,
            ..ClipboardPolicy::default()
        });
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("blocked"));
        bridge.pump(&mut cliprdr);

        assert_eq!(
            state.lock().unwrap().text,
            None,
            "remote content must never land locally under from_remote=false"
        );
    }

    #[test]
    fn to_remote_off_never_advertises_a_local_copy() {
        let (state, os) = fake_clipboard();
        let (backend, bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut bridge = bridge.with_policy(ClipboardPolicy {
            to_remote: false,
            ..ClipboardPolicy::default()
        });
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr).on_request_format_list();
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().text = Some("local secret".to_owned());
        bridge.poll_local_change();
        let messages = bridge.pump(&mut cliprdr);
        assert!(
            messages.is_empty(),
            "a local copy must produce no wire messages under to_remote=false"
        );
        assert!(
            bridge.local_pending.is_empty(),
            "the local change must not even be queued"
        );
    }

    #[test]
    fn to_remote_off_rejects_a_data_request_without_reading_the_clipboard() {
        let (state, os) = fake_clipboard();
        let (backend, bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut bridge = bridge.with_policy(ClipboardPolicy {
            to_remote: false,
            ..ClipboardPolicy::default()
        });
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        let reads_before = state.lock().unwrap().get_text_calls;

        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        let messages = bridge.pump(&mut cliprdr);
        assert_eq!(
            messages.len(),
            1,
            "a rejected request must still be answered"
        );
        assert_eq!(
            state.lock().unwrap().get_text_calls,
            reads_before,
            "a disabled to-remote request must not read the OS clipboard"
        );

        match only_pdu(messages) {
            ClipboardPdu::FormatDataResponse(response) => {
                assert!(
                    response.is_error(),
                    "a disabled request must return an error"
                );
            }
            other => panic!("unexpected pdu: {other:?}"),
        }
    }

    #[test]
    fn the_policy_timeout_replaces_the_default() {
        let (_state, os) = fake_clipboard();
        let clock = FakeClock::new();
        let (backend, bridge) = clipboard_channel_with_clock(os, clock.clone());
        let mut bridge = bridge.with_policy(ClipboardPolicy {
            paste_timeout_ms: 1_234,
            ..ClipboardPolicy::default()
        });
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        assert!(matches!(bridge.paste_state, PasteState::Requested { .. }));
        clock.advance(1_300);
        bridge.check_timeouts();
        assert_eq!(
            bridge.paste_state,
            PasteState::Idle,
            "1.3s beats a 1.234s cap"
        );
    }

    #[test]
    fn the_image_ceiling_tightens_but_never_widens() {
        // 4x4 RGBA = 64 bytes of pixels + header; a 32-byte cap must refuse it.
        let rgba = vec![0x7Fu8; 4 * 4 * 4];
        assert!(encode_dib(4, 4, &rgba, 32).is_err());
        assert!(encode_dib(4, 4, &rgba, MAX_IMAGE_BYTES).is_ok());
        let wire = encode_dib(4, 4, &rgba, MAX_IMAGE_BYTES).unwrap();
        assert!(decode_dib(&wire, 8).is_err());
        assert!(decode_dib(&wire, MAX_IMAGE_BYTES).is_ok());
        // The policy cannot widen: with_policy clamps to the built-in ceiling.
        let (_state, os) = fake_clipboard();
        let (_backend, bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let bridge = bridge.with_policy(ClipboardPolicy {
            max_image_bytes: u64::MAX,
            ..ClipboardPolicy::default()
        });
        assert_eq!(bridge.max_image_bytes, MAX_IMAGE_BYTES);
    }

    #[test]
    fn paste_request_timeout_resets_to_idle_and_next_copy_still_works() {
        let (state, os) = fake_clipboard();
        let clock = FakeClock::new();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, clock.clone());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        assert!(matches!(bridge.paste_state, PasteState::Requested { .. }));

        // Remote never answers. Advance well past the timeout.
        clock.advance(DEFAULT_PASTE_TIMEOUT_MS + 1_000);
        bridge.check_timeouts();
        assert_eq!(
            bridge.paste_state,
            PasteState::Idle,
            "a stuck pending flag is the classic clipboard wedge"
        );

        // A subsequent copy must still work.
        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("still works"));
        bridge.pump(&mut cliprdr);

        assert_eq!(bridge.paste_state, PasteState::Idle);
        assert_eq!(state.lock().unwrap().text.as_deref(), Some("still works"));
    }

    #[test]
    fn read_failure_on_remote_request_sends_error_response_not_silence() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().fail_next_get = true;

        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(msgs.len(), 1, "must respond, never stay silent");

        match only_pdu(msgs) {
            ClipboardPdu::FormatDataResponse(response) => {
                assert!(
                    response.is_error(),
                    "a failed local read must produce an explicit error response"
                );
            }
            other => panic!("unexpected pdu: {other:?}"),
        }

        // A malformed local image follows the same anti-wedge rule: answer the request with
        // an explicit error instead of leaving the remote waiting forever.
        state.lock().unwrap().image = Some((2, 2, vec![0, 1, 2]));
        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_DIB,
        });
        let msgs = bridge.pump(&mut cliprdr);
        match only_pdu(msgs) {
            ClipboardPdu::FormatDataResponse(response) => {
                assert!(response.is_error());
            }
            other => panic!("unexpected pdu: {other:?}"),
        }
    }

    #[test]
    fn local_change_is_advertised_once_via_poll_local_change() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().text = Some("user copied this".to_string());
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(
            msgs.len(),
            1,
            "a genuinely new local copy must be advertised"
        );
        assert_eq!(
            bridge.advertise_state,
            AdvertiseState::Pending {
                attempt: 0,
                resend_requested: false,
            }
        );

        // Polling again with unchanged content must not re-advertise.
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert!(
            msgs.is_empty(),
            "unchanged content must not be re-advertised on every poll"
        );
    }

    #[test]
    fn rapid_local_changes_wait_for_the_in_flight_advertise_then_send_the_latest() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().text = Some("zero".to_string());
        bridge.poll_local_change();
        assert_eq!(bridge.pump(&mut cliprdr).len(), 1);

        // Windows allows one FormatList exchange at a time. These changes happen before
        // the first acknowledgement, so they must coalesce rather than race two more PDUs.
        state.lock().unwrap().text = Some("one".to_string());
        bridge.poll_local_change();
        state.lock().unwrap().text = Some("two (latest)".to_string());
        bridge.poll_local_change();
        assert!(
            bridge.pump(&mut cliprdr).is_empty(),
            "an in-flight advertise must serialize later local changes"
        );

        backend_mut(&mut cliprdr).on_format_list_response(true);
        assert_eq!(
            bridge.pump(&mut cliprdr).len(),
            1,
            "the acknowledgement must release one coalesced latest advertise"
        );
        backend_mut(&mut cliprdr).on_format_list_response(true);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        match only_pdu(bridge.pump(&mut cliprdr)) {
            ClipboardPdu::FormatDataResponse(response) => {
                assert_eq!(decode_utf16le_text(response.data()), "two (latest)");
            }
            other => panic!("unexpected pdu variant: {other:?}"),
        }
    }

    #[test]
    fn local_poll_waits_for_the_protocols_initial_format_list_request() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = CliprdrClient::new(Box::new(backend));

        state.lock().unwrap().text = Some("copied before Monitor Ready".to_string());
        bridge.poll_local_change();
        assert!(
            bridge.pump(&mut cliprdr).is_empty(),
            "a local poll must not send initialization PDUs before Monitor Ready"
        );

        backend_mut(&mut cliprdr).on_request_format_list();
        assert_eq!(
            bridge.pump(&mut cliprdr).len(),
            1,
            "the protocol request must release one advertise with current content"
        );
    }

    #[test]
    fn remote_written_text_is_not_re_advertised_by_poll_local_change() {
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        backend_mut(&mut cliprdr).on_remote_copy(&[unicode_format()]);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr)
            .on_format_data_response(FormatDataResponse::new_unicode_string("from remote"));
        bridge.pump(&mut cliprdr);
        assert_eq!(state.lock().unwrap().text.as_deref(), Some("from remote"));

        // The OS clipboard now holds exactly what we just wrote from the remote; polling
        // must see "no change" and must not loop it back as a fresh local advertise.
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert!(
            msgs.is_empty(),
            "must not loop the remote's own content back to it"
        );
    }

    /// Representative of the defect report's "100 MB log tail" / "big spreadsheet region" —
    /// picked smaller only so the test suite stays fast; it is still ~80x the hash cap, so
    /// nothing about the assertions below depends on the exact multiple.
    const LARGE_PAYLOAD_BYTES: usize = 20 * 1024 * 1024;

    #[test]
    fn single_poll_of_a_huge_payload_only_hashes_the_bounded_prefix() {
        // This is the regression test for the reported defect: before the fix,
        // `content_hash` ran SHA-256 over the *entire* payload every poll — 50-100 ms for
        // 100 MB. One poll of a huge payload must now hash exactly the bounded prefix, not
        // a byte more, regardless of how large the payload actually is.
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().text = Some("x".repeat(LARGE_PAYLOAD_BYTES));
        reset_hashed_bytes();

        bridge.poll_local_change();

        assert_eq!(
            hashed_bytes(),
            HASH_PREFIX_CAP_BYTES,
            "a single poll must hash exactly the bounded prefix cap, not the full \
             {LARGE_PAYLOAD_BYTES}-byte payload"
        );
        // And the change is still correctly detected and advertised.
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(
            msgs.len(),
            1,
            "a genuine large local copy must still be advertised"
        );
    }

    #[test]
    fn repeated_polls_of_unchanged_huge_payload_cost_a_constant_capped_amount_each_time() {
        // The actual defect scenario: the same huge payload sits on the clipboard across
        // many poll ticks (a copy that just stays there while the session keeps running).
        // Before the fix, every one of those ticks re-hashed the whole payload — 50-100 ms
        // apiece, compounding for as long as the content sat on the clipboard. Each
        // "nothing changed" poll after the first must now cost exactly the capped amount,
        // never the full payload size, and never zero (detection must stay live).
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        state.lock().unwrap().text = Some("y".repeat(LARGE_PAYLOAD_BYTES));
        bridge.poll_local_change(); // baseline poll: establishes last_seen_fingerprint
        bridge.pump(&mut cliprdr); // drain the resulting advertise

        reset_hashed_bytes();
        let get_text_calls_before = state.lock().unwrap().get_text_calls;

        const REPEAT_POLLS: usize = 10;
        for _ in 0..REPEAT_POLLS {
            bridge.poll_local_change();
            assert!(
                bridge.pump(&mut cliprdr).is_empty(),
                "unchanged content must not advertise"
            );
        }

        assert_eq!(
            hashed_bytes(),
            REPEAT_POLLS * HASH_PREFIX_CAP_BYTES,
            "each unchanged poll must cost exactly the capped amount — not the \
             {LARGE_PAYLOAD_BYTES}-byte payload, and not zero (detection must stay live)"
        );
        let get_text_calls_after = state.lock().unwrap().get_text_calls;
        assert_eq!(
            get_text_calls_after - get_text_calls_before,
            REPEAT_POLLS,
            "clipboard content must still be read on every poll — bounding the hash must not \
             disable change detection"
        );
    }

    #[test]
    fn a_tail_only_change_past_the_prefix_cap_with_unchanged_length_is_not_detected() {
        // Documents the accepted trade-off of a bounded-prefix fingerprint: two payloads of
        // the same length that share the same first HASH_PREFIX_CAP_BYTES bytes are
        // indistinguishable to it. This only bites content larger than the cap that shares
        // a huge common prefix and keeps the same total length — accepted because the
        // alternative is the unbounded full-payload hash this fix exists to remove.
        let (state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);

        let shared_prefix = "p".repeat(HASH_PREFIX_CAP_BYTES);
        let payload_a = format!("{shared_prefix}AAAA");
        let payload_b = format!("{shared_prefix}BBBB"); // same length, differs only past the cap
        assert_eq!(payload_a.len(), payload_b.len());

        state.lock().unwrap().text = Some(payload_a);
        bridge.poll_local_change();
        bridge.pump(&mut cliprdr); // drain the first advertise
        backend_mut(&mut cliprdr).on_format_list_response(true);
        bridge.pump(&mut cliprdr); // complete the one allowed in-flight exchange

        state.lock().unwrap().text = Some(payload_b);
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert!(
            msgs.is_empty(),
            "documented limitation: a same-length change entirely past the prefix cap is \
             not detected by this fingerprint"
        );

        // Sanity check on the other side of that trade-off: a change that alters the
        // *length* is still always detected, even past the cap, because the length is
        // folded into the fingerprint alongside the prefix.
        state.lock().unwrap().text = Some(format!("{shared_prefix}BBBBB"));
        bridge.poll_local_change();
        let msgs = bridge.pump(&mut cliprdr);
        assert_eq!(
            msgs.len(),
            1,
            "a length change past the cap must still be detected"
        );
    }

    #[test]
    fn utf16_decode_strips_trailing_nul() {
        let mut bytes = Vec::new();
        for unit in "hi".encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&0u16.to_le_bytes());
        assert_eq!(decode_utf16le_text(&bytes), "hi");
    }

    #[test]
    fn utf16_decode_of_empty_text_is_empty_string() {
        assert_eq!(decode_utf16le_text(&0u16.to_le_bytes()), "");
        assert_eq!(decode_utf16le_text(&[]), "");
    }

    #[test]
    fn utf16_decode_replaces_lone_surrogate_without_panicking() {
        let lone_high_surrogate: u16 = 0xD800;
        let bytes = lone_high_surrogate.to_le_bytes();
        assert_eq!(decode_utf16le_text(&bytes), "\u{FFFD}");
    }

    fn test_path(label: &str) -> PathBuf {
        static NEXT_TEST_PATH: AtomicU64 = AtomicU64::new(1);
        std::env::temp_dir().join(format!(
            "mdrdp-clipboard-test-{label}-{}-{}",
            std::process::id(),
            NEXT_TEST_PATH.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn local_file_snapshot_reads_bounded_ranges_and_rejects_replacement() {
        let root = test_path("local");
        fs::create_dir_all(&root).unwrap();
        let file_path = root.join("payload.bin");
        let folder_path = root.join("folder");
        fs::create_dir(&folder_path).unwrap();
        let child_path = folder_path.join("child.txt");
        fs::write(&child_path, b"child").unwrap();
        fs::write(&file_path, b"abcdef").unwrap();

        let snapshot =
            build_local_file_snapshot(&[file_path.clone(), folder_path.clone()]).unwrap();
        assert_eq!(snapshot.entries.len(), 3);
        assert_eq!(snapshot.entries[0].descriptor.file_size, Some(6));
        assert!(
            !snapshot.entries[0]
                .descriptor
                .attributes
                .unwrap()
                .contains(ClipboardFileAttributes::DIRECTORY)
        );
        assert!(
            snapshot.entries[1]
                .descriptor
                .attributes
                .unwrap()
                .contains(ClipboardFileAttributes::DIRECTORY)
        );
        assert_eq!(
            snapshot.entries[2].descriptor.relative_path.as_deref(),
            Some("folder")
        );
        assert_eq!(snapshot.entries[2].descriptor.name, "child.txt");

        let size = read_local_file(
            &FileContentsRequest {
                stream_id: 1,
                index: 0,
                flags: FileContentsFlags::SIZE,
                position: 0,
                requested_size: 8,
                data_id: None,
            },
            &snapshot.entries[0],
        );
        assert_eq!(size.data_as_size().unwrap(), 6);

        let range = read_local_file(
            &FileContentsRequest {
                stream_id: 2,
                index: 0,
                flags: FileContentsFlags::RANGE,
                position: 2,
                requested_size: 3,
                data_id: None,
            },
            &snapshot.entries[0],
        );
        assert_eq!(range.data(), b"cde");

        fs::write(&file_path, b"replaced").unwrap();
        let stale = read_local_file(
            &FileContentsRequest {
                stream_id: 3,
                index: 0,
                flags: FileContentsFlags::RANGE,
                position: 0,
                requested_size: 2,
                data_id: None,
            },
            &snapshot.entries[0],
        );
        assert!(stale.is_error());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn remote_staging_handles_folders_and_requires_exact_chunks() {
        let (state, os) = fake_clipboard();
        let staging = StagingLayout::new();
        let active_epoch = Arc::new(AtomicU64::new(1));
        let active_transfer = Arc::new(AtomicU64::new(1));
        let mut worker =
            ClipboardWorkerState::new(os, active_epoch, active_transfer, staging.clone());
        let files = vec![
            FileDescriptor::new("folder").with_attributes(ClipboardFileAttributes::DIRECTORY),
            FileDescriptor::new("payload.bin")
                .with_relative_path("folder")
                .with_attributes(ClipboardFileAttributes::NORMAL)
                .with_file_size(3),
        ];
        let prepared = worker.execute(ClipboardWork::PrepareRemoteFiles {
            id: 1,
            epoch: 1,
            transfer_id: 1,
            files,
        });
        assert!(matches!(
            prepared,
            ClipboardResult::RemoteFilesPrepared { result: Ok(_), .. }
        ));

        let wrong_offset = worker.execute(ClipboardWork::StoreRemoteChunk {
            id: 2,
            epoch: 1,
            transfer_id: 1,
            index: 1,
            offset: 1,
            expected_size: 3,
            data: b"abc".to_vec(),
        });
        assert!(matches!(
            wrong_offset,
            ClipboardResult::RemoteChunkStored { result: Err(_), .. }
        ));
        let stored = worker.execute(ClipboardWork::StoreRemoteChunk {
            id: 3,
            epoch: 1,
            transfer_id: 1,
            index: 1,
            offset: 0,
            expected_size: 3,
            data: b"abc".to_vec(),
        });
        assert!(matches!(
            stored,
            ClipboardResult::RemoteChunkStored { result: Ok(()), .. }
        ));
        let published = worker.execute(ClipboardWork::PublishRemoteFiles {
            id: 4,
            epoch: 1,
            transfer_id: 1,
            sizes: vec![(1, 3)],
        });
        assert!(matches!(
            published,
            ClipboardResult::RemoteFilesPublished { result: Ok(()), .. }
        ));
        let published_paths = state.lock().unwrap().files.clone().unwrap();
        assert_eq!(published_paths.len(), 1);
        assert_eq!(
            fs::read(published_paths[0].join("payload.bin")).unwrap(),
            b"abc"
        );
        let _ = fs::remove_dir_all(staging.root);
    }

    #[test]
    fn cancelled_remote_stage_cannot_publish_stale_files() {
        let (state, os) = fake_clipboard();
        let staging = StagingLayout::new();
        let active_epoch = Arc::new(AtomicU64::new(1));
        let active_transfer = Arc::new(AtomicU64::new(7));
        let worker_transfer = active_transfer.clone();
        let mut worker =
            ClipboardWorkerState::new(os, active_epoch, worker_transfer, staging.clone());
        let files = vec![FileDescriptor::new("stale.txt").with_file_size(0)];
        let prepared = worker.execute(ClipboardWork::PrepareRemoteFiles {
            id: 1,
            epoch: 1,
            transfer_id: 7,
            files,
        });
        assert!(matches!(
            prepared,
            ClipboardResult::RemoteFilesPrepared { result: Ok(_), .. }
        ));

        active_transfer.store(0, Ordering::Release);
        let published = worker.execute(ClipboardWork::PublishRemoteFiles {
            id: 2,
            epoch: 1,
            transfer_id: 7,
            sizes: vec![(0, 0)],
        });
        assert!(matches!(
            published,
            ClipboardResult::RemoteFilesPublished { result: Err(_), .. }
        ));
        assert!(state.lock().unwrap().files.is_none());
        assert!(!staging.root.join("transfer-7").exists());
        let _ = fs::remove_dir_all(staging.root);
    }

    #[test]
    fn stale_root_cleanup_handles_a_non_file_clipboard() {
        let (_state, mut os) = fake_clipboard();
        let parent = test_path("stale-cleanup");
        let staging = StagingLayout::new_in(parent.clone());
        let stale_root = staging.parent.join(format!(
            "{STAGING_ROOT_PREFIX}stale-test-{}",
            NEXT_STAGING_ID.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&stale_root).unwrap();

        let cleanup_now = SystemTime::now() + STAGING_MIN_AGE + Duration::from_secs(1);
        for _ in 0..100 {
            staging.cleanup_abandoned_at(&mut *os, cleanup_now);
            if !stale_root.exists() {
                break;
            }
            thread::yield_now();
        }

        let removed = !stale_root.exists();
        if stale_root.exists() {
            fs::remove_dir(&stale_root).unwrap();
        }
        let _ = fs::remove_dir_all(staging.root);
        let _ = fs::remove_dir_all(parent);
        assert!(removed);
    }

    #[test]
    fn oversized_local_file_snapshot_queues_a_visible_failure() {
        let root = test_path("local-cap-notification");
        fs::create_dir_all(&root).unwrap();
        let oversized = root.join("oversized.bin");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_FILE_TOTAL_BYTES + 1)
            .unwrap();

        let (state, os) = fake_clipboard();
        state.lock().unwrap().files = Some(vec![oversized]);
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        backend_mut(&mut cliprdr).on_request_format_list();
        bridge.pump(&mut cliprdr);

        assert!(bridge.take_file_limit_notification());
        assert!(!bridge.take_file_limit_notification());
        drop(cliprdr);
        drop(bridge);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn oversized_remote_declared_total_queues_a_visible_failure() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        let file_format = ClipboardFormat::new(ClipboardFormatId::new(0xC0FE))
            .with_name(ClipboardFormatName::new_static(FORMAT_NAME_FILE_LIST));
        bridge.remote_formats = vec![file_format.clone()];
        bridge.paste_state = PasteState::Requested {
            format: file_format.id(),
            requested_at_ms: 0,
        };
        bridge.handle_remote_file_list(
            vec![FileDescriptor::new("oversized.bin").with_file_size(MAX_FILE_TOTAL_BYTES + 1)],
            None,
        );
        bridge.pump(&mut cliprdr);

        assert!(bridge.take_file_limit_notification());
        assert!(!bridge.take_file_limit_notification());
    }

    #[test]
    fn oversized_remote_observed_total_queues_a_visible_failure() {
        let (_state, os) = fake_clipboard();
        let (backend, mut bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let mut cliprdr = ready_client(backend);
        bridge.pump(&mut cliprdr);
        let file_format = ClipboardFormat::new(ClipboardFormatId::new(0xC0FE))
            .with_name(ClipboardFormatName::new_static(FORMAT_NAME_FILE_LIST));
        bridge.remote_formats = vec![file_format.clone()];
        bridge.paste_state = PasteState::Requested {
            format: file_format.id(),
            requested_at_ms: 0,
        };
        bridge.handle_remote_file_list(
            vec![
                FileDescriptor::new("first.bin"),
                FileDescriptor::new("second.bin"),
            ],
            None,
        );
        bridge.pump(&mut cliprdr);

        let first_stream = bridge
            .remote_transfer
            .as_ref()
            .and_then(|transfer| transfer.pending)
            .expect("first size request")
            .stream_id;
        let mut out = Vec::new();
        bridge.handle_remote_file_contents_received(
            OwnedFileContentsResponse::new_size_response(first_stream, MAX_FILE_TOTAL_BYTES),
            &mut cliprdr,
            &mut out,
        );
        let second_stream = bridge
            .remote_transfer
            .as_ref()
            .and_then(|transfer| transfer.pending)
            .expect("second size request")
            .stream_id;
        bridge.handle_remote_file_contents_received(
            OwnedFileContentsResponse::new_size_response(second_stream, 1),
            &mut cliprdr,
            &mut out,
        );

        assert!(bridge.take_file_limit_notification());
        assert!(!bridge.take_file_limit_notification());
    }

    #[test]
    fn remote_path_validation_rejects_traversal_reserved_names_and_collisions() {
        let staging = StagingLayout::new();
        let traversal = FileDescriptor::new("escape.txt").with_relative_path("..\\outside");
        assert!(staging.create_remote_stage(1, &[traversal]).is_err());
        let reserved = FileDescriptor::new("CON");
        assert!(staging.create_remote_stage(2, &[reserved]).is_err());
        let collision = vec![
            FileDescriptor::new("same.txt"),
            FileDescriptor::new("SAME.TXT"),
        ];
        assert!(staging.create_remote_stage(3, &collision).is_err());
        let _ = fs::remove_dir_all(staging.root);
    }

    #[test]
    fn backend_advertises_streaming_file_capabilities_without_exposing_paths() {
        let (_state, os) = fake_clipboard();
        let (backend, _bridge) = clipboard_channel_with_clock(os, FakeClock::new());
        let capabilities = backend.client_capabilities();
        assert!(capabilities.contains(ClipboardGeneralCapabilityFlags::STREAM_FILECLIP_ENABLED));
        assert!(capabilities.contains(ClipboardGeneralCapabilityFlags::CAN_LOCK_CLIPDATA));
        assert!(capabilities.contains(ClipboardGeneralCapabilityFlags::HUGE_FILE_SUPPORT_ENABLED));
        assert!(!backend.temporary_directory().is_empty());
        let debug = format!("{backend:?}");
        assert!(!debug.contains(backend.temporary_directory()));
    }
}
