//! Effective-access analysis: "what can this user or agent actually do to
//! each file?" plus a set of risk flags worth surfacing in an agentic world.
//!
//! The core question this answers: if an AI agent runs as some identity, which
//! files can it read, write, or execute, and which entries are dangerous
//! (world-writable, setuid/setgid, owned by someone else, or symlinks that
//! escape the tree). On Unix we compute this from mode bits + ownership
//! relative to a viewpoint's uid/gids. On other platforms we degrade to a
//! readonly/writable distinction.

use crate::fstree::{Node, Tree};

/// Whose permissions we are visualizing.
#[derive(Clone)]
pub struct Viewpoint {
    pub label: String,
    pub uid: u32,
    pub gids: Vec<u32>,
    /// Root can do anything; we short-circuit the permission math for uid 0.
    pub is_root: bool,
}

impl Viewpoint {
    /// The identity the program is currently running as.
    pub fn current() -> Viewpoint {
        #[cfg(unix)]
        {
            let uid = unsafe { libc_getuid() };
            let mut gids = vec![unsafe { libc_getgid() }];
            gids.extend(supplementary_groups());
            Viewpoint {
                label: format!("me ({})", crate::fstree::lookup_user_public(uid)),
                uid,
                gids,
                is_root: uid == 0,
            }
        }
        #[cfg(not(unix))]
        {
            Viewpoint {
                label: "me".to_string(),
                uid: 0,
                gids: vec![],
                is_root: false,
            }
        }
    }

    /// A named identity (e.g. an agent's service account), resolved from a
    /// username. Falls back to a uid-only viewpoint if the name is unknown.
    pub fn named(name: &str) -> Viewpoint {
        #[cfg(unix)]
        {
            if let Some((uid, gid)) = lookup_user_ids(name) {
                let mut gids = vec![gid];
                gids.extend(groups_for_user(name, gid));
                return Viewpoint {
                    label: format!("agent: {name}"),
                    uid,
                    gids,
                    is_root: uid == 0,
                };
            }
        }
        // Unknown name: treat as an unprivileged "other" with no group matches.
        Viewpoint {
            label: format!("agent: {name} (unresolved)"),
            uid: u32::MAX,
            gids: vec![],
            is_root: false,
        }
    }
}

/// The computed access for one node from one viewpoint.
#[derive(Clone, Copy, Default)]
pub struct Access {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
    // risk flags
    pub world_writable: bool,
    pub setuid: bool,
    pub setgid: bool,
    pub owned_by_other: bool,
    pub symlink_escapes: bool,
}

impl Access {
    pub fn any_risk(&self) -> bool {
        self.world_writable || self.setuid || self.setgid || self.symlink_escapes
    }
}

// Unix mode bit constants (avoid depending on libc just for these).
const S_ISUID: u32 = 0o4000;
const S_ISGID: u32 = 0o2000;
const S_IRUSR: u32 = 0o400;
const S_IWUSR: u32 = 0o200;
const S_IXUSR: u32 = 0o100;
const S_IRGRP: u32 = 0o040;
const S_IWGRP: u32 = 0o020;
const S_IXGRP: u32 = 0o010;
const S_IROTH: u32 = 0o004;
const S_IWOTH: u32 = 0o002;
const S_IXOTH: u32 = 0o001;

/// Compute access for a single node from a viewpoint.
pub fn compute(node: &Node, vp: &Viewpoint) -> Access {
    let mode = node.meta.mode;
    let mut a = Access {
        world_writable: mode & S_IWOTH != 0,
        setuid: mode & S_ISUID != 0,
        setgid: mode & S_ISGID != 0,
        owned_by_other: node.meta.uid != vp.uid,
        symlink_escapes: node.symlink_escapes,
        ..Default::default()
    };

    if vp.is_root {
        // Root reads/writes everything; exec if any exec bit is set.
        a.read = true;
        a.write = true;
        a.exec = mode & (S_IXUSR | S_IXGRP | S_IXOTH) != 0;
        return a;
    }

    // Choose the permission triad that applies: owner, then group, then other.
    let (r, w, x) = if node.meta.uid == vp.uid {
        (S_IRUSR, S_IWUSR, S_IXUSR)
    } else if vp.gids.contains(&node.meta.gid) {
        (S_IRGRP, S_IWGRP, S_IXGRP)
    } else {
        (S_IROTH, S_IWOTH, S_IXOTH)
    };

    a.read = mode & r != 0;
    a.write = mode & w != 0;
    a.exec = mode & x != 0;
    a
}

impl Tree {
    /// (Re)compute access for every node under a viewpoint. Cheap; safe to call
    /// whenever the viewpoint toggles.
    pub fn recompute_access(&mut self, vp: &Viewpoint) {
        for node in &mut self.nodes {
            node.access = Some(compute(node, vp));
        }
    }
}

// --- platform glue -------------------------------------------------------

#[cfg(unix)]
extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
    #[link_name = "getgid"]
    fn libc_getgid() -> u32;
    #[link_name = "getgroups"]
    fn libc_getgroups(size: i32, list: *mut u32) -> i32;
}

#[cfg(unix)]
fn supplementary_groups() -> Vec<u32> {
    unsafe {
        let n = libc_getgroups(0, std::ptr::null_mut());
        if n <= 0 {
            return vec![];
        }
        let mut buf = vec![0u32; n as usize];
        let got = libc_getgroups(n, buf.as_mut_ptr());
        if got < 0 {
            return vec![];
        }
        buf.truncate(got as usize);
        buf
    }
}

#[cfg(unix)]
fn lookup_user_ids(name: &str) -> Option<(u32, u32)> {
    use std::ffi::CString;
    use std::os::raw::c_char;
    #[repr(C)]
    struct Passwd {
        pw_name: *const c_char,
        pw_passwd: *const c_char,
        pw_uid: u32,
        pw_gid: u32,
        pw_gecos: *const c_char,
        pw_dir: *const c_char,
        pw_shell: *const c_char,
    }
    extern "C" {
        fn getpwnam(name: *const c_char) -> *const Passwd;
    }
    let cname = CString::new(name).ok()?;
    unsafe {
        let pw = getpwnam(cname.as_ptr());
        if pw.is_null() {
            None
        } else {
            Some(((*pw).pw_uid, (*pw).pw_gid))
        }
    }
}

/// Best-effort supplementary groups for a named user via getgrouplist.
#[cfg(unix)]
fn groups_for_user(name: &str, primary: u32) -> Vec<u32> {
    use std::ffi::CString;
    use std::os::raw::c_char;
    extern "C" {
        fn getgrouplist(
            user: *const c_char,
            group: u32,
            groups: *mut u32,
            ngroups: *mut i32,
        ) -> i32;
    }
    let cname = match CString::new(name) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let mut n: i32 = 32;
    let mut buf = vec![0u32; n as usize];
    unsafe {
        let rc = getgrouplist(cname.as_ptr(), primary, buf.as_mut_ptr(), &mut n);
        if rc < 0 && n > 0 {
            // buffer too small; retry with the size it told us
            buf = vec![0u32; n as usize];
            getgrouplist(cname.as_ptr(), primary, buf.as_mut_ptr(), &mut n);
        }
        buf.truncate(n.max(0) as usize);
    }
    buf
}
