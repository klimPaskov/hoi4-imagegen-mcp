//! The provenance ledger: an append-only, hash-chained JSON Lines file.
//!
//! Each entry holds the SHA-256 of the previous one, so a deleted, edited or reordered line is detected by
//! [`Ledger::verify`]. The ledger answers "which references, settings and outputs belong together" and is the
//! evidence the install gate reads. It lives at `.hoi4-imagegen/ledger.jsonl`, next to the project's styles
//! and config, and is meant to be committed with the mod.

use std::fs::{self, OpenOptions, TryLockError};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::{Error, Result};
use crate::fsio::ProjectFs;
use crate::hashing::{canonical_hash, Clock};

/// Project-relative path of the ledger.
pub const LEDGER_PATH: &str = ".hoi4-imagegen/ledger.jsonl";
/// The marker file older versions lock with: 0.3.x and 0.4.0 to 0.4.3 create it exclusively for an append and remove
/// it after, and 0.4.4 locks it at the OS level and leaves it on disk. Every version since 0.4.5 holds it too.
const LOCK_PATH: &str = ".hoi4-imagegen/ledger.lock";
/// Held at the OS level by every 0.4.5 and later process for an append, so they never race each other for the marker.
const MUTEX_PATH: &str = ".hoi4-imagegen/ledger.mutex";
/// How long an append waits for the lock before it gives up.
const LOCK_WAIT: Duration = Duration::from_secs(60);
/// A marker older than this, with nobody holding it, belongs to a crashed process.
const STALE_AFTER: Duration = Duration::from_secs(60);
const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// One ledger line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Entry {
    /// 1-based sequence number.
    pub seq: u64,
    /// RFC 3339 UTC time.
    pub ts: String,
    /// Event kind, see [`crate::records::kind`].
    pub kind: String,
    /// `feature.asset` the event belongs to.
    pub asset_id: Option<String>,
    /// Candidate or processed artifact id.
    pub artifact_id: Option<String>,
    /// Event payload.
    pub data: Value,
    /// Hash of the previous entry (zeros for the first).
    pub prev: String,
    /// Hash of this entry's other fields.
    pub hash: String,
}

fn entry_hash(e: &Entry) -> String {
    canonical_hash(&json!({
        "seq": e.seq, "ts": e.ts, "kind": e.kind, "asset_id": e.asset_id,
        "artifact_id": e.artifact_id, "data": e.data, "prev": e.prev,
    }))
}

/// Result of checking the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VerifyReport {
    /// True when every entry checks out.
    pub ok: bool,
    /// Entries read.
    pub entries: u64,
    /// First bad entry and why.
    pub first_bad: Option<(u64, String)>,
}

/// What [`Ledger::repair`] did, or with `dry_run` what it would do.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepairReport {
    /// True when the file was changed.
    pub repaired: bool,
    /// True when nothing was written because only a look was asked for.
    pub dry_run: bool,
    /// Entries in the ledger before the repair.
    pub entries_before: u64,
    /// The 1-based line of the first bad entry, `None` when the chain was fine.
    pub broken_at_line: Option<u64>,
    /// What was wrong with that line.
    pub why: Option<String>,
    /// Entries from the bad line on, kept and chained after the repair entry.
    pub moved: u64,
    /// Sequence number of the `chain_repair` entry.
    pub repair_seq: Option<u64>,
    /// Project-relative path of the copy made before the change.
    pub backup: Option<String>,
}

/// Handle to the ledger of one project.
#[derive(Clone)]
pub struct Ledger {
    fs: ProjectFs,
    clock: Arc<dyn Clock>,
}

/// The ledger lock of one append. It speaks every protocol in use, so servers of different versions on one machine
/// keep one chain: an OS lock on `ledger.mutex` (0.4.5 and later against each other), the marker file `ledger.lock`,
/// which exists while the append runs (0.3.x and 0.4.0 to 0.4.3 create it exclusively and wait while it exists), and an
/// OS lock on that marker (0.4.4 opens the file once and polls the lock on that open handle).
///
/// A marker is never taken away from anybody who may be waiting on it. A 0.4.4 server holding a handle on the file
/// would otherwise lock a file that no longer has a name while the next writer creates a new one, and both would
/// append at once (the fork of 2026-11-23). So a writer that finds the marker free adopts that very file, locks it and
/// writes its process id into it, and on release marks it `released` and removes it only when nobody else has it open
/// (Windows can tell by an exclusive open; elsewhere the OS offers no such check, so it is removed as before).
struct LockGuard {
    mutex: fs::File,
    marker: Option<fs::File>,
    marker_path: PathBuf,
}

/// What a marker holds once its writer is done with it and could not remove it: nobody owns it any more.
const RELEASED_NOTE: &[u8] = b"released\n";

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(mut marker) = self.marker.take() {
            // say that the marker is free, for the writers that find it left behind (a 0.4.4 server that waited on it)
            let _ = marker.set_len(0);
            let _ = marker.seek(SeekFrom::Start(0));
            let _ = marker.write_all(RELEASED_NOTE);
            // old enough that 0.3.x and 0.4.5 to 0.4.6, which take a marker over after a minute, do not wait for it if it
            // stays (a 0.4.4 waiter, or a virus scanner or a sync client holding a handle for a moment, can keep it)
            let _ = marker.set_modified(SystemTime::now() - 2 * STALE_AFTER);
            let _ = marker.unlock();
            drop(marker);
            remove_marker_unless_open(&self.marker_path, 25);
        }
        let _ = self.mutex.unlock();
    }
}

/// Remove the marker if nobody else has it open. Returns true when it is gone.
#[cfg(windows)]
fn remove_marker_unless_open(path: &Path, tries: u32) -> bool {
    use std::os::windows::fs::OpenOptionsExt;
    const DELETE: u32 = 0x0001_0000;
    const FILE_FLAG_DELETE_ON_CLOSE: u32 = 0x0400_0000;
    // an exclusive open (no sharing at all) fails while any other handle exists, a waiting 0.4.4 server's included, and
    // while it is open nobody can open the file; it goes when this handle closes
    for _ in 0..tries.max(1) {
        match OpenOptions::new().access_mode(DELETE).share_mode(0).custom_flags(FILE_FLAG_DELETE_ON_CLOSE).open(path) {
            Ok(handle) => {
                drop(handle);
                return true;
            }
            Err(e) if e.kind() == ErrorKind::NotFound => return true,
            // another writer probing the marker holds a handle for a moment: try again shortly, and give up for good
            // only when a handle stays (it is a waiting server's, which will take the lock on this very file)
            Err(_) => std::thread::sleep(Duration::from_millis(1)),
        }
    }
    false
}

#[cfg(not(windows))]
fn remove_marker_unless_open(path: &Path, tries: u32) -> bool {
    for _ in 0..tries.max(1) {
        match fs::remove_file(path) {
            Ok(()) => return true,
            Err(e) if e.kind() == ErrorKind::NotFound => return true,
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    false
}

/// Open the marker, sharing it for reading and writing but not for deletion on Windows, so nobody can remove or rename
/// it while it is held. With `create_new` the file must not exist yet.
fn open_marker(path: &Path, create_new: bool) -> std::io::Result<fs::File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0x1 | 0x2); // FILE_SHARE_READ | FILE_SHARE_WRITE
    }
    options.open(path)
}

/// True when a marker that nobody holds at the OS level is not an older server's live lock: it says it was released,
/// is empty and a moment old (0.4.4 leaves its marker empty on disk for good, and 0.3.x writes its process id at once),
/// or is older than [`STALE_AFTER`] (a crashed writer's).
fn marker_is_free(file: &mut fs::File) -> bool {
    let Ok(meta) = file.metadata() else { return false };
    let age = meta.modified().ok().and_then(|t| SystemTime::now().duration_since(t).ok()).unwrap_or_default();
    if age > STALE_AFTER || (meta.len() == 0 && age > Duration::from_secs(2)) {
        return true;
    }
    let mut text = Vec::new();
    file.seek(SeekFrom::Start(0)).is_ok() && file.read_to_end(&mut text).is_ok() && text == RELEASED_NOTE
}

impl Ledger {
    /// Open the ledger of a project (nothing is created until the first append).
    pub fn new(fs: ProjectFs, clock: Arc<dyn Clock>) -> Self {
        Ledger { fs, clock }
    }

    fn lock(&self) -> Result<LockGuard> {
        self.lock_within(LOCK_WAIT)
    }

    fn lock_within(&self, wait: Duration) -> Result<LockGuard> {
        let marker_path = self.fs.resolve(LOCK_PATH)?;
        let mutex_path = self.fs.resolve(MUTEX_PATH)?;
        if let Some(parent) = marker_path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
        }
        let started = Instant::now();
        let mutex = OpenOptions::new().create(true).truncate(false).write(true).open(&mutex_path).map_err(|e| Error::io(mutex_path.display(), e))?;
        loop {
            match mutex.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    if started.elapsed() > wait {
                        return Err(Error::Ledger(format!(
                            "timed out waiting for the ledger lock: another hoi4-imagegen process (this is version {}) is still appending",
                            env!("CARGO_PKG_VERSION")
                        )));
                    }
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(TryLockError::Error(e)) => return Err(Error::io(mutex_path.display(), e)),
            }
        }
        // from here the guard releases the mutex on every way out
        let mut guard = LockGuard { mutex, marker: None, marker_path: marker_path.clone() };
        loop {
            // a marker that does not exist yet is created here and is ours; one that exists is adopted when it is free
            let taken = match open_marker(&marker_path, true) {
                Ok(f) => match f.try_lock() {
                    Ok(()) => Some(f),
                    Err(_) => None,
                },
                Err(e) if matches!(e.kind(), ErrorKind::AlreadyExists | ErrorKind::PermissionDenied | ErrorKind::NotFound) => {
                    // Under the mutex no 0.4.5 or later writer is mid-append, so a marker that is OS-locked belongs to a
                    // live 0.4.4 server, and one that is not is a 0.3.x server's (it holds no OS lock; its process id and
                    // a recent time say so), or free.
                    match open_marker(&marker_path, false) {
                        Ok(mut f) if f.try_lock().is_ok() => {
                            if marker_is_free(&mut f) {
                                Some(f)
                            } else {
                                let _ = f.unlock();
                                None
                            }
                        }
                        _ => None,
                    }
                }
                Err(e) => return Err(Error::io(marker_path.display(), e)),
            };
            if let Some(mut f) = taken {
                let _ = f.set_len(0);
                let _ = f.seek(SeekFrom::Start(0));
                let _ = writeln!(f, "{}", std::process::id());
                guard.marker = Some(f);
                return Ok(guard);
            }
            if started.elapsed() > wait {
                let holder = fs::read_to_string(&marker_path)
                    .ok()
                    .and_then(|t| t.trim().parse::<u32>().ok())
                    .map_or(String::new(), |pid| format!(" (process {pid})"));
                return Err(Error::Ledger(format!(
                    "timed out waiting for the ledger lock: .hoi4-imagegen/ledger.lock is held by another hoi4-imagegen process{holder}, \
                     which may be an older version (this is {}). 0.3.x servers lock with that file and 0.4.4 locks it at the OS level; \
                     from 0.4.5 every version takes both. Close the older server (or wait for its call to end) and retry",
                    env!("CARGO_PKG_VERSION")
                )));
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Append one event. Returns the stored entry.
    pub fn append(&self, kind: &str, asset_id: Option<&str>, artifact_id: Option<&str>, data: impl Serialize) -> Result<Entry> {
        let data = serde_json::to_value(data).map_err(|e| Error::Ledger(format!("cannot serialize event: {e}")))?;
        let _guard = self.lock()?;
        self.append_locked(kind, asset_id, artifact_id, data)
    }

    /// The append itself, for a caller that holds the ledger lock.
    fn append_locked(&self, kind: &str, asset_id: Option<&str>, artifact_id: Option<&str>, data: Value) -> Result<Entry> {
        let path = self.fs.resolve(LEDGER_PATH)?;
        let existing = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => String::new(),
            Err(e) => return Err(Error::io(path.display(), e)),
        };
        if !existing.is_empty() && !existing.ends_with('\n') {
            return Err(Error::Ledger(
                "the ledger ends with a partial line (an interrupted write). Inspect .hoi4-imagegen/ledger.jsonl before adding to it".into(),
            ));
        }
        let (seq, prev) = match existing.lines().last() {
            Some(line) => {
                let last: Entry = serde_json::from_str(line).map_err(|e| Error::Ledger(format!("last ledger line is unreadable: {e}")))?;
                // Refuse to extend a chain whose tail is already wrong: a new entry would link to a bad one and
                // hide where the break is. The whole chain is checked by `Ledger::verify` (hoi4.image_ledger).
                let lines = existing.lines().count() as u64;
                if last.seq != lines || entry_hash(&last) != last.hash {
                    return Err(Error::Ledger(format!(
                        "the ledger chain is broken at its end (last line {lines} says seq {}, or its hash does not match its content), so nothing was added. \
                         Run `hoi4-imagegen ledger verify` to see the first bad entry, then `hoi4-imagegen ledger repair --reason \"<what happened>\"` to chain it \
                         again with a recorded repair entry (the file is backed up first). Do not edit the file by hand",
                        last.seq
                    )));
                }
                (last.seq + 1, last.hash)
            }
            None => (1, GENESIS.to_string()),
        };
        let mut entry = Entry {
            seq,
            ts: self.clock.now_rfc3339(),
            kind: kind.to_string(),
            asset_id: asset_id.map(str::to_string),
            artifact_id: artifact_id.map(str::to_string),
            data,
            prev,
            hash: String::new(),
        };
        entry.hash = entry_hash(&entry);
        let mut line = serde_json::to_string(&entry).map_err(|e| Error::Ledger(e.to_string()))?;
        line.push('\n');
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(&path).map_err(|e| Error::io(path.display(), e))?;
        f.write_all(line.as_bytes()).map_err(|e| Error::io(path.display(), e))?;
        f.sync_all().map_err(|e| Error::io(path.display(), e))?;
        Ok(entry)
    }

    /// Chain a broken ledger again without losing an entry. The first line whose sequence number or previous-hash link
    /// is wrong (two writers appended from the same tail, so the same sequence number or the same previous hash
    /// appears twice) and every line after it are kept with their content unchanged, but get new sequence numbers,
    /// links and hashes after a `chain_repair` entry. That entry names the broken line, what was found, why, and the
    /// old sequence number and hash of every moved entry. Lines before the break stay byte for byte. The whole file
    /// is copied to `.hoi4-imagegen/ledger-backups/` first, and the new file replaces the old one only after the
    /// whole new chain was checked in memory. An entry whose own hash does not match its content was altered after
    /// it was written: that is not repaired.
    pub fn repair(&self, reason: &str, dry_run: bool) -> Result<RepairReport> {
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(Error::Ledger("a repair needs a reason: say what happened, for example --reason \"two writers appended at once\"".into()));
        }
        let _guard = self.lock()?;
        let path = self.fs.resolve(LEDGER_PATH)?;
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(Error::io(path.display(), e)),
        };
        let unchanged = |entries: u64| RepairReport { repaired: false, dry_run, entries_before: entries, ..RepairReport::default() };
        if bytes.is_empty() {
            return Ok(unchanged(0));
        }
        if !bytes.ends_with(b"\n") {
            return Err(Error::Ledger("the ledger ends with a partial line (an interrupted write): inspect it before repairing".into()));
        }
        let text = String::from_utf8(bytes.clone()).map_err(|e| Error::Ledger(format!("the ledger is not UTF-8 text: {e}")))?;
        let mut offsets = Vec::new();
        let mut entries: Vec<Entry> = Vec::new();
        let mut pos = 0usize;
        for (i, line) in text.split_inclusive('\n').enumerate() {
            offsets.push(pos);
            pos += line.len();
            let entry: Entry = serde_json::from_str(line.trim_end_matches(['\n', '\r']))
                .map_err(|e| Error::Ledger(format!("ledger line {} is not a ledger entry ({e}): only whole entries can be chained again", i + 1)))?;
            entries.push(entry);
        }
        // the first line whose sequence number or link is wrong
        let mut prev = GENESIS.to_string();
        let mut broken: Option<(usize, &'static str)> = None;
        for (i, e) in entries.iter().enumerate() {
            if e.seq != i as u64 + 1 {
                broken = Some((i, "sequence number is not contiguous"));
                break;
            }
            if e.prev != prev {
                broken = Some((i, "previous-hash link is broken"));
                break;
            }
            prev = e.hash.clone();
        }
        let Some((at, why)) = broken else {
            return match entries.iter().position(|e| entry_hash(e) != e.hash) {
                Some(i) => Err(Error::Ledger(format!(
                    "ledger line {} does not match its own hash: its content was changed after it was written, which a repair does not hide",
                    i + 1
                ))),
                None => Ok(unchanged(entries.len() as u64)),
            };
        };
        // every kept and every moved entry must be what its writer wrote
        if let Some(i) = entries.iter().position(|e| entry_hash(e) != e.hash) {
            return Err(Error::Ledger(format!(
                "ledger line {} does not match its own hash: its content was changed after it was written, which a repair does not hide",
                i + 1
            )));
        }
        let expected_prev = if at == 0 { GENESIS.to_string() } else { entries[at - 1].hash.clone() };
        let stamp = self.clock.now_rfc3339();
        let backup_rel = format!(".hoi4-imagegen/ledger-backups/ledger-before-repair-{}.jsonl", stamp.replace(':', "-"));
        let found = &entries[at];
        let moved: Vec<Value> = entries[at..]
            .iter()
            .enumerate()
            .map(|(k, e)| {
                json!({"old_seq": e.seq, "old_prev": e.prev, "old_hash": e.hash, "new_seq": at as u64 + 2 + k as u64,
                       "kind": e.kind, "asset_id": e.asset_id, "artifact_id": e.artifact_id, "ts": e.ts})
            })
            .collect();
        let repair_data = json!({
            "reason": reason,
            "tool_version": env!("CARGO_PKG_VERSION"),
            "broken_at_line": at + 1,
            "found": {"seq": found.seq, "prev": found.prev, "hash": found.hash, "kind": found.kind,
                      "expected_seq": at as u64 + 1, "expected_prev": expected_prev, "why": why},
            "moved_entries": moved.len(),
            "moved": moved.iter().take(200).collect::<Vec<_>>(),
            "backup": backup_rel,
        });
        let mut rebuilt: Vec<Entry> = Vec::with_capacity(entries.len() - at + 1);
        let mut link = expected_prev.clone();
        let mut repair_entry = Entry {
            seq: at as u64 + 1,
            ts: stamp.clone(),
            kind: "chain_repair".into(),
            asset_id: None,
            artifact_id: None,
            data: repair_data,
            prev: link.clone(),
            hash: String::new(),
        };
        repair_entry.hash = entry_hash(&repair_entry);
        link = repair_entry.hash.clone();
        rebuilt.push(repair_entry);
        for (k, e) in entries[at..].iter().enumerate() {
            let mut moved = e.clone();
            moved.seq = at as u64 + 2 + k as u64;
            moved.prev = link.clone();
            moved.hash = entry_hash(&moved);
            link = moved.hash.clone();
            rebuilt.push(moved);
        }
        let report = RepairReport {
            repaired: !dry_run,
            dry_run,
            entries_before: entries.len() as u64,
            broken_at_line: Some(at as u64 + 1),
            why: Some(why.to_string()),
            moved: entries.len() as u64 - at as u64,
            repair_seq: Some(at as u64 + 1),
            backup: Some(backup_rel.clone()),
        };
        if dry_run {
            return Ok(report);
        }
        let mut out: Vec<u8> = bytes[..offsets[at]].to_vec();
        for e in &rebuilt {
            out.extend_from_slice(serde_json::to_string(e).map_err(|e| Error::Ledger(e.to_string()))?.as_bytes());
            out.push(b'\n');
        }
        // the new chain, read back from the new bytes, must verify before anything is replaced
        let mut check_prev = GENESIS.to_string();
        for (i, line) in String::from_utf8_lossy(&out).lines().enumerate() {
            let e: Entry =
                serde_json::from_str(line).map_err(|e| Error::Ledger(format!("the repaired chain does not read back at line {}: {e}", i + 1)))?;
            if e.seq != i as u64 + 1 || e.prev != check_prev || entry_hash(&e) != e.hash {
                return Err(Error::Ledger(format!("the repaired chain does not verify at line {}: nothing was changed", i + 1)));
            }
            check_prev = e.hash;
        }
        let backup = self.fs.resolve(&backup_rel)?;
        if let Some(parent) = backup.parent() {
            fs::create_dir_all(parent).map_err(|e| Error::io(parent.display(), e))?;
        }
        fs::write(&backup, &bytes).map_err(|e| Error::io(backup.display(), e))?;
        if fs::read(&backup).map_err(|e| Error::io(backup.display(), e))? != bytes {
            return Err(Error::Ledger("the backup does not match the ledger: nothing was changed".into()));
        }
        let tmp = path.with_file_name("ledger.jsonl.repair-tmp");
        {
            let mut f = OpenOptions::new().create(true).write(true).truncate(true).open(&tmp).map_err(|e| Error::io(tmp.display(), e))?;
            f.write_all(&out).map_err(|e| Error::io(tmp.display(), e))?;
            f.sync_all().map_err(|e| Error::io(tmp.display(), e))?;
        }
        let mut last = None;
        for _ in 0..50 {
            match fs::rename(&tmp, &path) {
                Ok(()) => {
                    last = None;
                    break;
                }
                Err(e) => {
                    last = Some(e);
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        if let Some(e) = last {
            let _ = fs::remove_file(&tmp);
            return Err(Error::io(path.display(), e));
        }
        Ok(report)
    }

    /// Every entry, in order. Fails if a line cannot be parsed.
    pub fn entries(&self) -> Result<Vec<Entry>> {
        let path = self.fs.resolve(LEDGER_PATH)?;
        let text = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::io(path.display(), e)),
        };
        text.lines()
            .enumerate()
            .map(|(i, l)| serde_json::from_str::<Entry>(l).map_err(|e| Error::Ledger(format!("ledger line {} is unreadable: {e}", i + 1))))
            .collect()
    }

    /// Check the whole chain.
    pub fn verify(&self) -> Result<VerifyReport> {
        let entries = match self.entries() {
            Ok(e) => e,
            Err(Error::Ledger(msg)) => return Ok(VerifyReport { ok: false, entries: 0, first_bad: Some((0, msg)) }),
            Err(e) => return Err(e),
        };
        let mut prev = GENESIS.to_string();
        for (i, e) in entries.iter().enumerate() {
            let bad = |why: &str| VerifyReport { ok: false, entries: entries.len() as u64, first_bad: Some((e.seq, why.to_string())) };
            if e.seq != i as u64 + 1 {
                return Ok(bad("sequence number is not contiguous"));
            }
            if e.prev != prev {
                return Ok(bad("previous-hash link is broken"));
            }
            if entry_hash(e) != e.hash {
                return Ok(bad("entry content does not match its hash"));
            }
            prev = e.hash.clone();
        }
        Ok(VerifyReport { ok: true, entries: entries.len() as u64, first_bad: None })
    }

    /// Entries for one asset.
    pub fn for_asset(&self, asset_id: &str) -> Result<Vec<Entry>> {
        Ok(self.entries()?.into_iter().filter(|e| e.asset_id.as_deref() == Some(asset_id)).collect())
    }

    /// Entries for one artifact.
    pub fn for_artifact(&self, artifact_id: &str) -> Result<Vec<Entry>> {
        Ok(self.entries()?.into_iter().filter(|e| e.artifact_id.as_deref() == Some(artifact_id)).collect())
    }

    /// The generate, edit or import entry that used an idempotency key.
    pub fn find_idempotency(&self, key: &str) -> Result<Option<Entry>> {
        Ok(self
            .entries()?
            .into_iter()
            .find(|e| matches!(e.kind.as_str(), "generated" | "edited") && e.data.get("idempotency_key").and_then(Value::as_str) == Some(key)))
    }

    /// Highest version number ever allocated for an asset (so a deleted file's number is not reused).
    pub fn highest_version(&self, asset_id: &str) -> Result<u32> {
        Ok(self
            .for_asset(asset_id)?
            .iter()
            .filter(|e| matches!(e.kind.as_str(), "generated" | "edited" | "imported"))
            .filter_map(|e| e.data.get("version").and_then(Value::as_u64))
            .max()
            .unwrap_or(0) as u32)
    }

    /// Chain of parents from an artifact back to its first ancestor, nearest first.
    pub fn lineage(&self, artifact_id: &str) -> Result<Vec<String>> {
        let entries = self.entries()?;
        let mut chain = vec![artifact_id.to_string()];
        let mut cur = artifact_id.to_string();
        for _ in 0..1000 {
            let parent = entries
                .iter()
                .find(|e| {
                    e.artifact_id.as_deref() == Some(cur.as_str()) && matches!(e.kind.as_str(), "generated" | "edited" | "imported" | "processed")
                })
                .and_then(|e| e.data.get("parent").or_else(|| e.data.get("source_id")))
                .and_then(Value::as_str)
                .map(str::to_string);
            match parent {
                Some(p) if !chain.contains(&p) => {
                    chain.push(p.clone());
                    cur = p;
                }
                _ => break,
            }
        }
        Ok(chain)
    }

    /// Path of the ledger file, for display.
    pub fn path(&self) -> PathBuf {
        self.fs.root().join(LEDGER_PATH)
    }

    /// True when the ledger file exists.
    pub fn exists(&self) -> bool {
        Path::new(&self.path()).is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashing::FixedClock;

    fn ledger(dir: &tempfile::TempDir) -> Ledger {
        Ledger::new(ProjectFs::new(dir.path()).unwrap(), Arc::new(FixedClock(1_793_404_800)))
    }

    #[test]
    fn append_chain_and_verify() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        assert!(l.verify().unwrap().ok);
        let a = l.append("generated", Some("f.a"), Some("f.a-v1"), json!({"version": 1, "idempotency_key": "k1"})).unwrap();
        let b = l.append("validated", Some("f.a"), Some("f.a-v1"), json!({"status": "pass"})).unwrap();
        assert_eq!((a.seq, b.seq), (1, 2));
        assert_eq!(b.prev, a.hash);
        assert_eq!(a.ts, "2026-10-31T00:00:00Z");
        let report = l.verify().unwrap();
        assert!(report.ok && report.entries == 2, "{report:?}");
        assert_eq!(l.for_asset("f.a").unwrap().len(), 2);
        assert_eq!(l.find_idempotency("k1").unwrap().unwrap().seq, 1);
        assert!(l.find_idempotency("nope").unwrap().is_none());
        assert_eq!(l.highest_version("f.a").unwrap(), 1);
    }

    #[test]
    fn tampering_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        for i in 0..3 {
            l.append("generated", Some("f.a"), Some(&format!("f.a-v{i}")), json!({"version": i})).unwrap();
        }
        let path = l.path();
        let original = std::fs::read_to_string(&path).unwrap();

        // edit a payload
        std::fs::write(&path, original.replacen("\"version\":1", "\"version\":9", 1)).unwrap();
        let r = l.verify().unwrap();
        assert!(!r.ok && r.first_bad.as_ref().unwrap().0 == 2, "{r:?}");

        // delete a middle line
        let lines: Vec<&str> = original.lines().collect();
        std::fs::write(&path, format!("{}\n{}\n", lines[0], lines[2])).unwrap();
        assert!(!l.verify().unwrap().ok);

        // swap two lines
        std::fs::write(&path, format!("{}\n{}\n{}\n", lines[1], lines[0], lines[2])).unwrap();
        assert!(!l.verify().unwrap().ok);

        // garbage
        std::fs::write(&path, "not json\n").unwrap();
        let r = l.verify().unwrap();
        assert!(!r.ok);

        // restored
        std::fs::write(&path, original).unwrap();
        assert!(l.verify().unwrap().ok);
    }

    #[test]
    fn partial_last_line_blocks_appends() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        l.append("generated", Some("f.a"), None, json!({})).unwrap();
        let mut f = OpenOptions::new().append(true).open(l.path()).unwrap();
        f.write_all(b"{\"seq\":2").unwrap();
        drop(f);
        assert!(l.append("generated", Some("f.a"), None, json!({})).is_err());
    }

    #[test]
    fn concurrent_appends_keep_the_chain_intact() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let handles: Vec<_> = (0..12)
            .map(|i| {
                let l = l.clone();
                std::thread::spawn(move || l.append("generated", Some("f.a"), Some(&format!("f.a-v{i}")), json!({"version": i})).unwrap().seq)
            })
            .collect();
        let mut seqs: Vec<u64> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=12).collect::<Vec<_>>());
        let r = l.verify().unwrap();
        assert!(r.ok && r.entries == 12, "{r:?}");
        assert!(!dir.path().join(LOCK_PATH).exists(), "the marker is released");
    }

    /// Long floats in an entry's data (the preview metrics of an animation) must survive a write and a read:
    /// without exact float parsing, the reader's hash differs by one bit and the chain reports a false break.
    #[test]
    fn floats_in_the_data_keep_their_hash_across_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let metrics =
            [0.018356250794924905_f64, 0.004013536803274917, 0.007238881702284939, 1.0105794555042673, 12.39319470699433, 0.0, 1e-7, 1.5e300];
        l.append("previewed", Some("f.a"), Some("f.a-v1"), json!({"metrics": metrics})).unwrap();
        l.append("previewed", Some("f.a"), Some("f.a-v2"), json!({"metrics": metrics})).unwrap();
        let r = l.verify().unwrap();
        assert!(r.ok && r.entries == 2, "{r:?}");
        let back = l.entries().unwrap();
        assert_eq!(back[0].data["metrics"][0].as_f64(), Some(0.018356250794924905));
    }

    /// Entry 3763 of a real project ledger (a preview with animation metrics), unchanged. Its stored hash was made
    /// at write time; every later version must recompute the same hash from the line.
    #[test]
    fn a_real_entry_with_long_floats_matches_its_stored_hash() {
        let line = r#"{"seq":3763,"ts":"2026-11-08T20:33:44Z","kind":"previewed","asset_id":"interface_pictograms.researching_scroll_92","artifact_id":"interface_pictograms.researching_scroll_92-v1-p1","data":{"animation":{"effect_file":null,"fps":"15","frame_sheet":"docs/assets/interface_pictograms/contact_sheets/interface_pictograms_researching_scroll_92-v1-p1-preview-frames-v1.png","frame_sheet_sha256":"f8a31d5cbd8b864cfb3f5943c39cda9b11de7ec3812378be554782d0ef15182e","frames":9,"loop_gif":"docs/assets/interface_pictograms/contact_sheets/interface_pictograms_researching_scroll_92-v1-p1-preview-loop-v1.gif","loop_gif_pictures":9,"loop_gif_sha256":"764ce10534057ba310424e05ae43f799d613d1557e34a6fda621546e97679c71","metrics":{"centroid_drift_px":[0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0,0.0],"jump_ratio":1.0105794555042673,"palette_drift":[0.0,0.007238881702284939,0.011834812824328175,0.004013536803275335,0.018356250794934782,0.018356250794924905,0.004013536803274917,0.011834812824334772,0.007238881702290506],"seam_ratio":1.0080986725402616,"silhouette_iou":[1.0,1.0,1.0,1.0,1.0,1.0,1.0,1.0,1.0],"steps":[12.39319470699433,12.263527882797732,12.206994328922494,12.263380198487713,12.355919187145558,12.31187972589792,12.213078922495274,12.209475425330814,12.362771739130435]},"motion":"scroll","note":"each frame is shown for 1 / fps seconds","scale":2},"panels":["native on dark","native on light","on checker","reference 1"],"sha256":"ff979b62deda98ed356c0db5290439821ca92e863e8d2d95fc30dc601d7ca47f","sheet":"docs/assets/interface_pictograms/contact_sheets/interface_pictograms_researching_scroll_92-v1-p1-preview-v1.png","sheet_sha256":"ae6e9c0666772786562ca39fb4739a8f7249ab16c840708c1d885f97cd4f45bf","target":"interface_pictograms.researching_scroll_92-v1-p1","vanilla_check":{"family":"anim_scroll","reason":"the installed game has fewer than 3 measurable vanilla scrolling animated strip images to compare with","reference_set":{"kind":"vanilla","rule":"vanilla_match_v2"},"status":"unavailable","version":"vanilla_match_v2"},"vanilla_sheet":{"unavailable":"the installed game has fewer than 3 measurable vanilla scrolling animated strip images to compare with"}},"prev":"4c33e24d71520f8cacf75d5d446d3fe09a76d996e7a94e8aef9556c5ae2b173f","hash":"03e7e8d2b2e2166e9a6f76cf7d9697867b203d497d92f0e88a60bd32cf6a2e82"}"#;
        let e: Entry = serde_json::from_str(line).unwrap();
        assert_eq!(entry_hash(&e), e.hash);
    }

    #[test]
    fn a_broken_tail_refuses_the_append_with_a_clear_message() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        l.append("generated", Some("f.a"), None, json!({"n": 1})).unwrap();
        l.append("generated", Some("f.a"), None, json!({"n": 2})).unwrap();
        let path = l.path();
        let good = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, good.replacen("\"n\":2", "\"n\":3", 1)).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        let err = l.append("generated", Some("f.a"), None, json!({"n": 4})).unwrap_err().to_string();
        assert!(err.contains("chain is broken") && err.contains("ledger verify") && err.contains("ledger repair"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before, "nothing was appended");
        // a line copied from another writer, with its own sequence number, is refused too
        std::fs::write(&path, format!("{good}{}\n", good.lines().next().unwrap())).unwrap();
        assert!(l.append("generated", Some("f.a"), None, json!({"n": 5})).is_err());
    }

    /// Two writers, each with its own handle (as two MCP processes would have), keep one chain.
    #[test]
    fn two_independent_writers_keep_the_chain_intact() {
        let dir = tempfile::tempdir().unwrap();
        let handles: Vec<_> = (0..2)
            .map(|w| {
                let l = ledger(&dir);
                std::thread::spawn(move || {
                    for i in 0..25 {
                        l.append("generated", Some("f.a"), Some(&format!("f.a-w{w}-{i}")), json!({"writer": w, "i": i, "x": 0.1 * f64::from(i)}))
                            .unwrap();
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        let r = ledger(&dir).verify().unwrap();
        assert!(r.ok && r.entries == 50, "{r:?}");
    }

    /// What 0.3.x does: create the marker exclusively, work, remove it. A writer of this version waits for it.
    fn old_lock(dir: &tempfile::TempDir) -> std::io::Result<fs::File> {
        fs::create_dir_all(dir.path().join(".hoi4-imagegen"))?;
        OpenOptions::new().write(true).create_new(true).open(dir.path().join(LOCK_PATH))
    }

    #[test]
    fn an_older_servers_marker_makes_an_append_wait_until_it_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        l.append("generated", Some("f.a"), None, json!({"n": 0})).unwrap();
        let mut marker = old_lock(&dir).unwrap();
        writeln!(marker, "4242").unwrap();
        let released = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = released.clone();
        let path = dir.path().join(LOCK_PATH);
        let old_writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(400));
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
            drop(marker);
            fs::remove_file(path).unwrap();
        });
        let e = l.append("generated", Some("f.a"), None, json!({"n": 1})).unwrap();
        assert!(released.load(std::sync::atomic::Ordering::SeqCst), "the append waited for the older server to finish");
        assert_eq!(e.seq, 2);
        old_writer.join().unwrap();
        assert!(l.verify().unwrap().ok);
    }

    #[test]
    fn while_this_version_appends_an_older_server_sees_its_marker() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let guard = l.lock().unwrap();
        let err = old_lock(&dir).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::AlreadyExists, "0.3.x finds the marker and waits");
        // another writer of this version waits too, and its error names the version
        let err = l.lock_within(Duration::from_millis(150)).err().unwrap().to_string();
        assert!(err.contains("another hoi4-imagegen process") && err.contains(env!("CARGO_PKG_VERSION")), "{err}");
        drop(guard);
        assert!(!dir.path().join(LOCK_PATH).exists(), "the marker is removed on release");
        drop(old_lock(&dir).unwrap());
    }

    #[test]
    fn a_marker_that_blocks_for_good_is_named_as_a_version_clash() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let mut marker = old_lock(&dir).unwrap();
        writeln!(marker, "4242").unwrap();
        let err = l.lock_within(Duration::from_millis(150)).err().unwrap().to_string();
        assert!(
            err.contains("ledger.lock") && err.contains("older version") && err.contains("process 4242") && err.contains(env!("CARGO_PKG_VERSION")),
            "{err}"
        );
        assert!(dir.path().join(LOCK_PATH).exists(), "a live older server's marker is never removed");
    }

    /// 0.4.4 leaves an empty marker on disk for good and locks it at the OS level while it appends.
    #[test]
    fn a_leftover_empty_marker_does_not_block_and_an_os_locked_one_does() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let leftover = old_lock(&dir).unwrap();
        drop(leftover);
        std::thread::sleep(Duration::from_millis(2100));
        l.append("generated", Some("f.a"), None, json!({"n": 1})).unwrap();
        assert!(!dir.path().join(LOCK_PATH).exists());
        // a 0.4.4 server in the middle of an append
        let held = OpenOptions::new().create(true).truncate(false).write(true).open(dir.path().join(LOCK_PATH)).unwrap();
        held.try_lock().unwrap();
        std::thread::sleep(Duration::from_millis(2100));
        let err = l.lock_within(Duration::from_millis(200)).err().unwrap().to_string();
        assert!(err.contains("ledger.lock"), "{err}");
        held.unlock().unwrap();
        drop(held);
        l.append("generated", Some("f.a"), None, json!({"n": 2})).unwrap();
        assert!(l.verify().unwrap().ok);
    }

    /// An older server crashed in the middle of an append: its marker is old and nobody holds it.
    #[test]
    fn a_crashed_servers_old_marker_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        let mut marker = old_lock(&dir).unwrap();
        writeln!(marker, "4242").unwrap();
        marker.set_modified(SystemTime::now() - Duration::from_secs(120)).unwrap();
        drop(marker);
        l.append("generated", Some("f.a"), None, json!({"n": 1})).unwrap();
        assert!(l.verify().unwrap().ok);
    }

    /// A line a second writer appended from the same tail view: valid on its own (its hash matches), but its
    /// sequence number or link repeats one already in the file.
    fn forged(seq: u64, prev: &str, kind: &str, data: Value) -> Entry {
        let mut e = Entry {
            seq,
            ts: "2026-11-23T16:55:53Z".into(),
            kind: kind.into(),
            asset_id: Some("f.a".into()),
            artifact_id: None,
            data,
            prev: prev.into(),
            hash: String::new(),
        };
        e.hash = entry_hash(&e);
        e
    }

    fn add_line(l: &Ledger, e: &Entry) {
        let mut f = OpenOptions::new().append(true).open(l.path()).unwrap();
        writeln!(f, "{}", serde_json::to_string(e).unwrap()).unwrap();
    }

    #[test]
    fn a_forked_tail_is_chained_again_with_a_repair_entry_and_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        for i in 0..3 {
            l.append("generated", Some("f.a"), Some(&format!("f.a-v{i}")), json!({"i": i})).unwrap();
        }
        let before = l.entries().unwrap();
        // the second writer saw the tail before entry 3 was written
        let dup = forged(3, &before[1].hash, "job_ended", json!({"job_id": "job-1", "outcome": "completed", "x": 0.018356250794924905}));
        add_line(&l, &dup);
        let report = l.verify().unwrap();
        assert!(!report.ok && report.first_bad.as_ref().unwrap().0 == 3, "{report:?}");
        let err = l.append("generated", Some("f.a"), None, json!({})).unwrap_err().to_string();
        assert!(err.contains("ledger repair") && err.contains("ledger verify"), "{err}");

        // a look changes nothing
        let bytes = std::fs::read(l.path()).unwrap();
        let look = l.repair("two writers appended at once", true).unwrap();
        assert!(look.dry_run && !look.repaired && look.broken_at_line == Some(4) && look.moved == 1, "{look:?}");
        assert_eq!(std::fs::read(l.path()).unwrap(), bytes);
        assert!(!dir.path().join(".hoi4-imagegen/ledger-backups").exists());
        assert!(l.repair("  ", false).is_err(), "a repair needs a reason");

        let done = l.repair("two writers appended at once", false).unwrap();
        assert!(done.repaired && done.broken_at_line == Some(4) && done.repair_seq == Some(4) && done.moved == 1, "{done:?}");
        let r = l.verify().unwrap();
        assert!(r.ok && r.entries == 5, "{r:?}");
        // the lines before the break are untouched
        let now = std::fs::read_to_string(l.path()).unwrap();
        let old = String::from_utf8(bytes.clone()).unwrap();
        assert_eq!(now.lines().take(3).collect::<Vec<_>>(), old.lines().take(3).collect::<Vec<_>>());
        // the repair entry names the broken line, the cause and the moved entry
        let all = l.entries().unwrap();
        assert_eq!(all[3].kind, "chain_repair");
        assert_eq!(all[3].data["broken_at_line"], 4);
        assert_eq!(all[3].data["reason"], "two writers appended at once");
        assert_eq!(all[3].data["found"]["why"], "sequence number is not contiguous");
        assert_eq!(all[3].data["moved"][0]["old_hash"], dup.hash);
        assert_eq!(all[3].data["moved"][0]["new_seq"], 5);
        // the moved entry keeps its content, time and kind
        assert_eq!((all[4].kind.as_str(), all[4].ts.as_str(), &all[4].data), ("job_ended", "2026-11-23T16:55:53Z", &dup.data));
        // the backup is the file as it was, and the ledger takes entries again
        let backup = dir.path().join(done.backup.unwrap());
        assert_eq!(std::fs::read(backup).unwrap(), bytes);
        assert_eq!(l.append("generated", Some("f.a"), None, json!({})).unwrap().seq, 6);
        assert!(!l.repair("again", false).unwrap().repaired, "an intact chain needs no repair");
        assert!(!dir.path().join(".hoi4-imagegen/ledger.jsonl.repair-tmp").exists());
    }

    /// A 0.3.x writer does not check the tail and keeps appending after the bad line.
    #[test]
    fn entries_appended_after_a_fork_are_chained_again_too() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        for i in 0..3 {
            l.append("generated", Some("f.a"), None, json!({"i": i})).unwrap();
        }
        let e = l.entries().unwrap();
        let dup = forged(3, &e[1].hash, "job_ended", json!({"n": 1}));
        let next = forged(4, &dup.hash, "validated", json!({"n": 2}));
        let last = forged(5, &next.hash, "processed", json!({"n": 3}));
        for x in [&dup, &next, &last] {
            add_line(&l, x);
        }
        assert!(!l.verify().unwrap().ok);
        let done = l.repair("an older server appended after the fork", false).unwrap();
        assert_eq!((done.broken_at_line, done.moved, done.entries_before), (Some(4), 3, 6));
        let r = l.verify().unwrap();
        assert!(r.ok && r.entries == 7, "{r:?}");
        let kinds: Vec<String> = l.entries().unwrap().into_iter().map(|e| e.kind).collect();
        assert_eq!(kinds[3..], ["chain_repair", "job_ended", "validated", "processed"]);
    }

    #[test]
    fn an_entry_changed_after_it_was_written_is_not_repaired() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        for i in 0..3 {
            l.append("generated", Some("f.a"), None, json!({"version": i})).unwrap();
        }
        let e = l.entries().unwrap();
        add_line(&l, &forged(3, &e[1].hash, "job_ended", json!({})));
        let text = std::fs::read_to_string(l.path()).unwrap();
        std::fs::write(l.path(), text.replacen("\"version\":1", "\"version\":9", 1)).unwrap();
        let before = std::fs::read(l.path()).unwrap();
        let err = l.repair("test", false).unwrap_err().to_string();
        assert!(err.contains("line 2") && err.contains("changed after it was written"), "{err}");
        assert_eq!(std::fs::read(l.path()).unwrap(), before, "nothing was changed");
    }

    /// What a 0.4.4 server does: open the marker once, poll an OS lock on that open handle, append, unlock.
    #[cfg(windows)]
    fn append_like_0_4_4(l: &Ledger, dir: &Path, tag: &str, i: u32) {
        let file = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK_PATH)).unwrap();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => std::hint::spin_loop(),
                Err(TryLockError::Error(e)) => panic!("{e}"),
            }
        }
        l.append_locked("generated", Some("f.a"), Some(&format!("f.a-{tag}-{i}")), json!({"tag": tag, "i": i})).unwrap();
        let _ = file.unlock();
    }

    /// What 0.3.x and 0.4.0 to 0.4.3 do: create the marker exclusively, retry while it exists, append, remove it.
    fn append_like_0_3(l: &Ledger, dir: &Path, tag: &str, i: u32) {
        let path = dir.join(LOCK_PATH);
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = writeln!(f, "{}", std::process::id());
                    break;
                }
                Err(_) => std::thread::sleep(Duration::from_millis(1)),
            }
        }
        l.append_locked("generated", Some("f.a"), Some(&format!("f.a-{tag}-{i}")), json!({"tag": tag, "i": i})).unwrap();
        for _ in 0..200 {
            if fs::remove_file(&path).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// Writers of this version and of an older one, all appending at once from separate threads and handles.
    fn old_and_new_writers_at_once(old: fn(&Ledger, &Path, &str, u32)) {
        let dir = tempfile::tempdir().unwrap();
        ledger(&dir).append("generated", Some("f.a"), None, json!({"start": true})).unwrap();
        let per = 40u32;
        let mut handles = Vec::new();
        for w in 0..3 {
            let (l, path) = (ledger(&dir), dir.path().to_path_buf());
            handles.push(std::thread::spawn(move || {
                for i in 0..per {
                    old(&l, &path, &format!("old{w}"), i);
                    std::thread::sleep(Duration::from_millis(u64::from(i % 3)));
                }
            }));
            let l = ledger(&dir);
            handles.push(std::thread::spawn(move || {
                for i in 0..per {
                    l.append("generated", Some("f.a"), Some(&format!("f.a-new{w}-{i}")), json!({"tag": "new", "i": i})).unwrap();
                    std::thread::sleep(Duration::from_millis(u64::from(i % 2)));
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let r = ledger(&dir).verify().unwrap();
        assert!(r.ok && r.entries == 1 + 6 * u64::from(per), "{r:?}");
    }

    /// The fork of 2026-11-23: a 0.4.4 server waits on the marker file it opened, and the writer that releases it must
    /// not delete that file under it. Windows only: elsewhere the OS cannot tell whether anyone has the file open.
    #[cfg(windows)]
    #[test]
    fn a_0_4_4_writer_and_this_version_appending_at_once_keep_one_chain() {
        old_and_new_writers_at_once(append_like_0_4_4);
    }

    #[test]
    fn a_0_3_writer_and_this_version_appending_at_once_keep_one_chain() {
        old_and_new_writers_at_once(append_like_0_3);
    }

    /// What 0.4.5 and 0.4.6 do: the mutex, then a marker created exclusively, taken over when it is stale, and deleted
    /// on release.
    fn append_like_0_4_6(l: &Ledger, dir: &Path, tag: &str, i: u32) {
        let mutex = OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(MUTEX_PATH)).unwrap();
        loop {
            match mutex.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => std::thread::sleep(Duration::from_millis(1)),
                Err(TryLockError::Error(e)) => panic!("{e}"),
            }
        }
        let path = dir.join(LOCK_PATH);
        let started = Instant::now();
        let marker = loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = f.try_lock();
                    let _ = writeln!(f, "{}", std::process::id());
                    break f;
                }
                Err(_) => {
                    if let Ok(f) = OpenOptions::new().write(true).open(&path) {
                        if f.try_lock().is_ok() {
                            let _ = f.unlock();
                            drop(f);
                            let stale = fs::metadata(&path).ok().is_some_and(|m| {
                                let age = m.modified().ok().and_then(|t| SystemTime::now().duration_since(t).ok()).unwrap_or_default();
                                age > STALE_AFTER || (m.len() == 0 && age > Duration::from_secs(2))
                            });
                            if stale {
                                let _ = fs::remove_file(&path);
                                continue;
                            }
                        }
                    }
                    assert!(started.elapsed() < Duration::from_secs(10), "a 0.4.6 writer waited more than ten seconds for the marker");
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        };
        l.append_locked("generated", Some("f.a"), Some(&format!("f.a-{tag}-{i}")), json!({"tag": tag, "i": i})).unwrap();
        let _ = marker.unlock();
        drop(marker);
        for _ in 0..200 {
            if fs::remove_file(&path).is_ok() || !path.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let _ = mutex.unlock();
    }

    #[test]
    fn a_0_4_6_writer_and_this_version_appending_at_once_keep_one_chain() {
        old_and_new_writers_at_once(append_like_0_4_6);
    }

    /// A virus scanner or a sync client holds a handle on the marker when this version lets go: the marker stays,
    /// marked released and old, and a 0.4.6 writer takes it over at once instead of waiting a minute for it.
    #[test]
    fn a_marker_that_stays_after_a_release_does_not_hold_up_an_older_writer() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        l.append("generated", Some("f.a"), None, json!({"n": 0})).unwrap();
        let guard = l.lock().unwrap();
        // someone else has the file open (sharing it fully, as a scanner does)
        let scanner = OpenOptions::new().read(true).open(dir.path().join(LOCK_PATH)).unwrap();
        drop(guard);
        drop(scanner);
        let marker = dir.path().join(LOCK_PATH);
        if marker.exists() {
            // on Windows the exclusive open failed while the scanner held it, so the marker stayed: released and old
            assert_eq!(fs::read(&marker).unwrap(), RELEASED_NOTE);
            let age = fs::metadata(&marker).unwrap().modified().unwrap().elapsed().unwrap();
            assert!(age > STALE_AFTER, "{age:?}");
        }
        let started = Instant::now();
        append_like_0_4_6(&l, dir.path(), "old", 1);
        assert!(started.elapsed() < Duration::from_secs(5), "the older writer did not wait for the leftover marker");
        l.append("generated", Some("f.a"), None, json!({"n": 2})).unwrap();
        assert!(l.verify().unwrap().ok);
    }

    /// Separate OS processes (this test binary, run again as children) append to one ledger at once.
    #[test]
    fn separate_processes_keep_the_chain_intact() {
        if let Ok(dir) = std::env::var("LEDGER_TEST_CHILD_DIR") {
            let l = Ledger::new(ProjectFs::new(Path::new(&dir)).unwrap(), Arc::new(FixedClock(1_793_404_800)));
            let tag = std::env::var("LEDGER_TEST_CHILD_TAG").unwrap();
            for i in 0..20 {
                l.append("generated", Some("f.a"), Some(&format!("f.a-{tag}-{i}")), json!({"tag": tag, "i": i, "x": 0.1 * f64::from(i)})).unwrap();
            }
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let exe = std::env::current_exe().unwrap();
        let children: Vec<_> = ["a", "b", "c"]
            .iter()
            .map(|tag| {
                std::process::Command::new(&exe)
                    .args(["--exact", "ledger::tests::separate_processes_keep_the_chain_intact", "--test-threads=1"])
                    .env("LEDGER_TEST_CHILD_DIR", dir.path())
                    .env("LEDGER_TEST_CHILD_TAG", tag)
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .spawn()
                    .unwrap()
            })
            .collect();
        for mut c in children {
            assert!(c.wait().unwrap().success());
        }
        let r = ledger(&dir).verify().unwrap();
        assert!(r.ok && r.entries == 60, "{r:?}");
    }

    #[test]
    fn lineage_follows_parents_and_source_ids() {
        let dir = tempfile::tempdir().unwrap();
        let l = ledger(&dir);
        l.append("generated", Some("f.a"), Some("f.a-v1"), json!({"version": 1})).unwrap();
        l.append("edited", Some("f.a"), Some("f.a-v2"), json!({"version": 2, "parent": "f.a-v1"})).unwrap();
        l.append("processed", Some("f.a"), Some("f.a-v2-p1"), json!({"source_id": "f.a-v2"})).unwrap();
        assert_eq!(l.lineage("f.a-v2-p1").unwrap(), vec!["f.a-v2-p1", "f.a-v2", "f.a-v1"]);
        assert_eq!(l.lineage("f.a-v1").unwrap(), vec!["f.a-v1"]);
    }
}
