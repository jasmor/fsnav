//! File operations with guardrails.
//!
//! Design rules, per the project's safety stance:
//!   * Deletes never call `unlink`/`remove_dir_all`; they go to the OS trash
//!     via the `trash` crate (Recycle Bin / Finder Trash / freedesktop trash).
//!   * Copy and move are real, but a directory move/delete requires the caller
//!     to have passed an explicit confirmation (enforced in the UI layer).
//!   * Move tries `rename` first and falls back to copy-then-trash across
//!     filesystems.
//!
//! Each successful op returns an `OpOutcome` the UI uses to animate and to
//! decide how to patch the in-memory tree (so we don't rescan the disk).

use std::fs;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Copy,
    Move,
    Trash,
}

/// The result of a successful file operation. `message` drives the on-screen
/// toast; the structured fields (`kind`/`src`/`dst`) describe what happened and
/// are kept for callers that want to react to the specific operation.
#[allow(dead_code)]
pub struct OpOutcome {
    pub kind: OpKind,
    pub src: PathBuf,
    pub dst: Option<PathBuf>, // None for trash
    pub message: String,
}

#[derive(Debug)]
pub enum OpError {
    Io(String),
    Trash(String),
    Refused(String),
}

impl std::fmt::Display for OpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OpError::Io(s) => write!(f, "{s}"),
            OpError::Trash(s) => write!(f, "trash failed: {s}"),
            OpError::Refused(s) => write!(f, "{s}"),
        }
    }
}

/// Copy a file or directory tree into `dst_dir`, keeping the source's name.
pub fn copy(src: &Path, dst_dir: &Path) -> Result<OpOutcome, OpError> {
    let name = src
        .file_name()
        .ok_or_else(|| OpError::Refused("source has no file name".into()))?;
    let dst = unique_dest(dst_dir, name);

    if src.is_dir() {
        copy_dir_recursive(src, &dst).map_err(|e| OpError::Io(e.to_string()))?;
    } else {
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent).map_err(|e| OpError::Io(e.to_string()))?;
        }
        fs::copy(src, &dst).map_err(|e| OpError::Io(e.to_string()))?;
    }

    Ok(OpOutcome {
        kind: OpKind::Copy,
        src: src.to_path_buf(),
        dst: Some(dst.clone()),
        message: format!("copied to {}", dst.display()),
    })
}

/// Move a file or directory into `dst_dir`. Tries rename, falls back to
/// copy + trash-original on cross-device moves.
pub fn move_to(src: &Path, dst_dir: &Path) -> Result<OpOutcome, OpError> {
    let name = src
        .file_name()
        .ok_or_else(|| OpError::Refused("source has no file name".into()))?;
    let dst = unique_dest(dst_dir, name);

    match fs::rename(src, &dst) {
        Ok(()) => Ok(OpOutcome {
            kind: OpKind::Move,
            src: src.to_path_buf(),
            dst: Some(dst.clone()),
            message: format!("moved to {}", dst.display()),
        }),
        Err(_) => {
            // Likely a cross-filesystem move: copy then send original to trash.
            if src.is_dir() {
                copy_dir_recursive(src, &dst).map_err(|e| OpError::Io(e.to_string()))?;
            } else {
                fs::copy(src, &dst).map_err(|e| OpError::Io(e.to_string()))?;
            }
            trash::delete(src).map_err(|e| OpError::Trash(e.to_string()))?;
            Ok(OpOutcome {
                kind: OpKind::Move,
                src: src.to_path_buf(),
                dst: Some(dst.clone()),
                message: format!("moved (cross-device) to {}", dst.display()),
            })
        }
    }
}

/// Send a file or directory to the OS trash. Never a permanent delete.
pub fn trash(src: &Path) -> Result<OpOutcome, OpError> {
    trash::delete(src).map_err(|e| OpError::Trash(e.to_string()))?;
    Ok(OpOutcome {
        kind: OpKind::Trash,
        src: src.to_path_buf(),
        dst: None,
        message: "moved to Trash".into(),
    })
}

/// Pick a non-colliding destination path: `name`, then `name (copy)`, etc.
fn unique_dest(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let base = dir.join(name);
    if !base.exists() {
        return base;
    }
    let stem = Path::new(name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = Path::new(name)
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    for i in 1.. {
        let candidate = if i == 1 {
            dir.join(format!("{stem} (copy){ext}"))
        } else {
            dir.join(format!("{stem} (copy {i}){ext}"))
        };
        if !candidate.exists() {
            return candidate;
        }
    }
    base
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Open a path in the user's default application (Finder/Explorer/desktop).
/// Uses the platform's standard opener; spawns it without waiting so the UI
/// never blocks. This is a launch, not a file mutation — safe and reversible.
pub fn open_path(path: &Path) -> Result<(), OpError> {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(all(unix, not(target_os = "macos")))]
    let program = "xdg-open";

    std::process::Command::new(program)
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|e| OpError::Io(format!("couldn't open: {e}")))
}
