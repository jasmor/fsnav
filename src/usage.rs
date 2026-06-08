//! Disk-usage scanning: compute the *recursive* size of each child of the
//! current directory (a folder's true weight = everything inside it), without
//! ever blocking the render loop.
//!
//! A folder can contain millions of files, so summing its size can take
//! seconds. We do that work on a background thread and stream results back
//! through a channel; the UI keeps rendering at full frame rate and grows the
//! boxes as numbers arrive. Navigating away cancels the in-flight scan via a
//! shared atomic flag, so we never leak threads or apply stale results.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::thread;

/// One result: the recursive size (in bytes) of a top-level child path.
pub struct SizeMsg {
    pub path: PathBuf,
    pub bytes: u64,
}

/// A running (or finished) scan of one directory's children.
pub struct UsageScan {
    rx: Receiver<SizeMsg>,
    cancel: Arc<AtomicBool>,
    pub done: bool,
    /// The directory this scan is for; used to ignore stale scans after nav.
    pub dir: PathBuf,
}

impl UsageScan {
    /// Start scanning the recursive size of every entry directly under `dir`.
    /// Each child's total streams back as it completes.
    pub fn start(dir: &Path) -> UsageScan {
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel_thread = cancel.clone();
        let dir_owned = dir.to_path_buf();
        let dir_for_thread = dir_owned.clone();

        thread::spawn(move || {
            let entries = match std::fs::read_dir(&dir_for_thread) {
                Ok(e) => e,
                Err(_) => return,
            };
            for entry in entries.flatten() {
                if cancel_thread.load(Ordering::Relaxed) {
                    return;
                }
                let path = entry.path();
                let bytes = match entry.file_type() {
                    Ok(ft) if ft.is_dir() => dir_size(&path, &cancel_thread),
                    Ok(ft) if ft.is_file() => entry.metadata().map(|m| m.len()).unwrap_or(0),
                    _ => 0, // symlinks/specials count as ~0 to avoid double-counting
                };
                // If the receiver is gone (UI moved on), stop quietly.
                if tx.send(SizeMsg { path, bytes }).is_err() {
                    return;
                }
            }
        });

        UsageScan {
            rx,
            cancel,
            done: false,
            dir: dir_owned,
        }
    }

    /// Drain any results that have arrived since last frame. Returns them so
    /// the caller can fold the sizes into its nodes. Non-blocking.
    pub fn poll(&mut self) -> Vec<SizeMsg> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(msg) => out.push(msg),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.done = true;
                    break;
                }
            }
        }
        out
    }
}

impl Drop for UsageScan {
    fn drop(&mut self) {
        // Signal the worker to stop when we abandon a scan (e.g. on navigation).
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// Recursively sum the byte size of a directory subtree. Iterative (an explicit
/// stack) so deep trees can't overflow; respects the cancel flag between steps.
fn dir_size(root: &Path, cancel: &Arc<AtomicBool>) -> u64 {
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        if cancel.load(Ordering::Relaxed) {
            return total;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue, // unreadable dir: skip rather than fail the whole sum
        };
        for entry in entries.flatten() {
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_symlink() {
                continue; // don't follow links: avoids cycles + double counting
            } else if ft.is_dir() {
                stack.push(entry.path());
            } else if ft.is_file() {
                total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
    }
    total
}
