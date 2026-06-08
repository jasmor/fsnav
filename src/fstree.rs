//! Filesystem model and layout — **single-directory view**.
//!
//! Earlier versions scanned the entire filesystem into one arena and laid the
//! whole hierarchy out at once. That doesn't scale: a directory with tens of
//! thousands of entries built that many cubes and could exhaust the stack on
//! deep trees. This version instead shows **one directory at a time**: we scan
//! only the immediate children, cap how many boxes appear (humans struggle to
//! track more than ~20), and descend into a folder on demand. Navigation keeps
//! a breadcrumb stack so you can walk back up.

use macroquad::prelude::Vec3;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// How many child boxes to show at once. Extra entries collapse into a single
/// "+N more" marker that pages through the rest.
pub const VIEW_CAP: usize = 20;

/// Tunable layout parameters (kept from the original for familiarity). A few
/// knobs aren't read by the current single-directory layout but are retained as
/// part of the tuning surface (and to mirror the original fsn parameters).
#[derive(Clone, Copy)]
#[allow(dead_code)]
pub struct LayoutParams {
    pub file_size: f32,
    pub file_spacing: f32,
    pub file_height: f32,
    pub dir_size: f32,
    pub dir_spacing: f32,
    pub dir_height: f32,
    pub dir_dist: f32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        LayoutParams {
            file_size: 0.5,
            file_spacing: 0.1,
            file_height: 0.1,
            dir_size: 0.5 + 0.2,
            dir_spacing: 0.5,
            dir_height: 0.1,
            dir_dist: 5.0,
        }
    }
}

/// Metadata carried only by files (directories ignore most of it).
#[derive(Clone, Default)]
pub struct FileMeta {
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub user: String,
    pub group: String,
    /// Access time — collected for completeness; the info card shows modified
    /// and created times, so this isn't currently displayed.
    #[allow(dead_code)]
    pub atime: Option<SystemTime>,
    pub mtime: Option<SystemTime>,
    pub ctime: Option<SystemTime>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Dir,
    File,
    /// Synthetic node representing "N more items" — clicking pages the view.
    More,
}

/// A single visible item in the current directory.
pub struct Node {
    pub kind: Kind,
    pub name: String,
    pub size: u64,
    pub entry_count: usize, // for dirs: number of immediate children (best-effort)

    pub path: PathBuf,
    pub symlink_target: Option<PathBuf>,
    pub symlink_escapes: bool,

    /// Recursive size in bytes (folders = sum of contents). Filled in by the
    /// background disk-usage scan; `None` until computed. For files this is
    /// just the file size once known.
    pub recursive_size: Option<u64>,

    pub vis_pos: Vec3,
    pub vis_size: Vec3,

    pub meta: FileMeta,

    /// Lazily-filled content classification (see `filetype`).
    pub content: Option<crate::filetype::Content>,
    /// Lazily-filled access summary for the active viewpoint (see `access`).
    pub access: Option<crate::access::Access>,
}

impl Node {
    fn new(kind: Kind, name: String) -> Self {
        Node {
            kind,
            name,
            size: 0,
            entry_count: 0,
            path: PathBuf::new(),
            symlink_target: None,
            symlink_escapes: false,
            recursive_size: None,
            vis_pos: Vec3::ZERO,
            vis_size: Vec3::ZERO,
            meta: FileMeta::default(),
            content: None,
            access: None,
        }
    }
}

/// How the current directory's entries are ordered.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// Directories first, then alphabetical (the classic file-manager order).
    Name,
    /// Largest first, files and folders mixed — find space hogs fast. Folder
    /// sizes settle as the background disk-usage scan reports them.
    Size,
    /// Most recently modified first.
    Newest,
}

impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::Size => "size",
            SortMode::Newest => "newest",
        }
    }
    pub fn next(self) -> SortMode {
        match self {
            SortMode::Name => SortMode::Size,
            SortMode::Size => SortMode::Newest,
            SortMode::Newest => SortMode::Name,
        }
    }
}

/// The current directory view: its path, a breadcrumb of ancestors, and the
/// (capped) set of visible child nodes.
pub struct Tree {
    pub nodes: Vec<Node>,
    pub cwd: PathBuf,
    /// Ancestor paths, deepest last; pop to go "back".
    pub breadcrumb: Vec<PathBuf>,
    pub root_canon: PathBuf,
    pub params: LayoutParams,
    pub selection: Option<usize>,

    /// Set when the current directory can't be read (permission denied, gone).
    /// The UI shows this instead of an empty scene so it doesn't look broken.
    pub scan_error: Option<String>,

    /// Active sort order. Cycle with the `s` key.
    pub sort: SortMode,

    /// Full, sorted child list for the current dir (paths only). `nodes` is a
    /// capped window into this; paging advances `page_start`.
    all_children: Vec<PathBuf>,
    page_start: usize,

    /// Recursive size (bytes) for every child path, accumulated from the
    /// background scan. Used to sort the *whole* listing by size, not just the
    /// visible page. Files get an entry immediately; folders fill in as scanned.
    sizes: std::collections::HashMap<PathBuf, u64>,
    /// Modified time (seconds since epoch) per child, for the Newest sort.
    mtimes: std::collections::HashMap<PathBuf, u64>,
}

impl Tree {
    /// Open `dirname` as the initial view.
    pub fn build(dirname: &str, params: LayoutParams) -> Option<Tree> {
        let start = Path::new(dirname);
        let canon = fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
        if fs::read_dir(&canon).is_err() {
            eprintln!("failed to open dir: {dirname}");
            return None;
        }
        let mut tree = Tree {
            nodes: Vec::new(),
            cwd: canon.clone(),
            breadcrumb: Vec::new(),
            root_canon: canon.clone(),
            params,
            selection: None,
            scan_error: None,
            sort: SortMode::Name,
            all_children: Vec::new(),
            page_start: 0,
            sizes: std::collections::HashMap::new(),
            mtimes: std::collections::HashMap::new(),
        };
        tree.scan_current();
        Some(tree)
    }

    /// Total entries in the current directory.
    pub fn total_count(&self) -> usize {
        self.all_children.len()
    }

    /// The current page's starting offset — used by callers to detect whether
    /// a next_page/prev_page actually moved (it's a no-op at the ends).
    pub fn page_index(&self) -> usize {
        self.page_start
    }

    /// How many real child entries fit on one page. When the directory has more
    /// than `VIEW_CAP` items we reserve one fixed slot for the "+N more" marker,
    /// keeping the page size stable so ←/→ paging is exactly reversible.
    fn page_capacity(&self) -> usize {
        if self.all_children.len() <= VIEW_CAP {
            self.all_children.len()
        } else {
            VIEW_CAP - 1
        }
    }

    /// True if there is a page after the current one.
    fn has_next(&self) -> bool {
        self.page_start + self.page_capacity() < self.all_children.len()
    }

    /// True if we're not on the first page.
    fn has_prev(&self) -> bool {
        self.page_start > 0
    }

    /// Scan only the current directory's immediate children (bounded work).
    pub fn scan_current(&mut self) {
        self.all_children.clear();
        self.page_start = 0;
        self.selection = None;
        self.scan_error = None;

        let entries = match fs::read_dir(&self.cwd) {
            Ok(e) => e,
            Err(err) => {
                let msg = match err.kind() {
                    std::io::ErrorKind::PermissionDenied => {
                        "permission denied — you don't have access to this folder".to_string()
                    }
                    std::io::ErrorKind::NotFound => "folder no longer exists".to_string(),
                    _ => format!("can't open folder: {err}"),
                };
                eprintln!("{}: {msg}", self.cwd.display());
                self.scan_error = Some(msg);
                self.nodes.clear();
                return;
            }
        };

        // entries.flatten() silently drops entries we can't stat (e.g. due to
        // permissions on individual items); that's the graceful behavior — we
        // show what we can rather than failing the whole listing.
        let paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();

        // Seed per-child metadata used by the size/newest sorts. Files get an
        // exact size right away; folders are left for the background scan to
        // fill in (so size-sort settles as those results arrive).
        self.sizes.clear();
        self.mtimes.clear();
        for p in &paths {
            if let Ok(meta) = fs::symlink_metadata(p) {
                if meta.is_file() {
                    self.sizes.insert(p.clone(), meta.len());
                }
                if let Ok(mt) = meta.modified() {
                    if let Ok(dur) = mt.duration_since(std::time::UNIX_EPOCH) {
                        self.mtimes.insert(p.clone(), dur.as_secs());
                    }
                }
            }
        }

        self.all_children = paths;
        self.sort_children();
        self.rebuild_page();
    }

    /// Reorder `all_children` according to the active sort mode.
    fn sort_children(&mut self) {
        // Snapshot the maps to avoid borrowing self inside the closure.
        let sort = self.sort;
        let sizes = &self.sizes;
        let mtimes = &self.mtimes;

        self.all_children.sort_by(|a, b| {
            match sort {
                SortMode::Name => {
                    // Directories first, then alphabetical.
                    let ad = a.is_dir();
                    let bd = b.is_dir();
                    bd.cmp(&ad).then_with(|| {
                        a.file_name()
                            .unwrap_or_default()
                            .cmp(b.file_name().unwrap_or_default())
                    })
                }
                SortMode::Size => {
                    // Largest first, files and folders mixed. Unknown sizes
                    // (unscanned folders) sort as 0 for now and float up later.
                    let sa = sizes.get(a).copied().unwrap_or(0);
                    let sb = sizes.get(b).copied().unwrap_or(0);
                    sb.cmp(&sa).then_with(|| {
                        a.file_name()
                            .unwrap_or_default()
                            .cmp(b.file_name().unwrap_or_default())
                    })
                }
                SortMode::Newest => {
                    let ma = mtimes.get(a).copied().unwrap_or(0);
                    let mb = mtimes.get(b).copied().unwrap_or(0);
                    mb.cmp(&ma).then_with(|| {
                        a.file_name()
                            .unwrap_or_default()
                            .cmp(b.file_name().unwrap_or_default())
                    })
                }
            }
        });
    }

    /// Change the sort mode, re-sort, and reset to the first page.
    pub fn set_sort(&mut self, mode: SortMode) {
        self.sort = mode;
        self.page_start = 0;
        self.sort_children();
        self.rebuild_page();
    }

    /// Record a child's recursive size (from the background scan) for both the
    /// visible node and the full-listing size map. Does NOT re-sort — call
    /// `resort_if_size` once per frame after applying a batch of updates.
    pub fn record_size(&mut self, path: &Path, bytes: u64) {
        self.sizes.insert(path.to_path_buf(), bytes);
        for n in &mut self.nodes {
            if n.path == path {
                n.recursive_size = Some(bytes);
            }
        }
    }

    /// If sorting by size, re-sort the listing and rebuild the current page so
    /// the order reflects freshly-arrived folder sizes. Cheap to call once per
    /// frame; a no-op in other sort modes. Preserves the current page offset.
    pub fn resort_if_size(&mut self) {
        if self.sort == SortMode::Size {
            let saved = self.page_start;
            self.sort_children();
            // Keep the page offset if still valid, else clamp to the last page.
            self.page_start = if saved < self.all_children.len() {
                saved
            } else {
                0
            };
            self.rebuild_page();
        }
    }

    /// Advance to the next page of children (called by the "+N more" marker).
    pub fn next_page(&mut self) {
        if self.has_next() {
            self.page_start += self.page_capacity();
            self.rebuild_page();
        }
    }

    /// Go back to the previous page (bound to the Left arrow key).
    pub fn prev_page(&mut self) {
        if self.has_prev() {
            let step = self.page_capacity();
            self.page_start = self.page_start.saturating_sub(step);
            self.rebuild_page();
        }
    }

    /// Materialize the current page of children into `nodes` + lay them out.
    fn rebuild_page(&mut self) {
        self.nodes.clear();
        self.selection = None;

        let cap = self.page_capacity();
        let end = (self.page_start + cap).min(self.all_children.len());
        let slice: Vec<PathBuf> = self.all_children[self.page_start..end].to_vec();

        for path in slice {
            if let Some(node) = self.make_node(&path) {
                self.nodes.push(node);
            }
        }

        // A "+N more" marker last, when there are later pages. (Paging is done
        // with the ←/→ arrow keys; there's no in-scene back marker.)
        let hidden = self.all_children.len().saturating_sub(end);
        if hidden > 0 {
            let mut more = Node::new(Kind::More, format!("+{hidden} more"));
            more.path = self.cwd.clone();
            self.nodes.push(more);
        }

        self.layout();
    }

    /// Build a single child node from its path (no recursion).
    fn make_node(&self, path: &Path) -> Option<Node> {
        let link_meta = fs::symlink_metadata(path).ok()?;
        let is_symlink = link_meta.file_type().is_symlink();
        let meta = if is_symlink {
            fs::metadata(path).unwrap_or_else(|_| link_meta.clone())
        } else {
            link_meta.clone()
        };

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());

        let (symlink_target, symlink_escapes) = if is_symlink {
            match fs::canonicalize(path) {
                Ok(t) => {
                    let escapes = !t.starts_with(&self.root_canon);
                    (Some(t), escapes)
                }
                Err(_) => (None, false),
            }
        } else {
            (None, false)
        };

        let kind = if meta.is_dir() { Kind::Dir } else { Kind::File };
        let mut node = Node::new(kind, name);
        node.path = path.to_path_buf();
        node.size = meta.len();
        node.meta = extract_meta(&meta);
        node.symlink_target = symlink_target;
        node.symlink_escapes = symlink_escapes;

        if kind == Kind::Dir {
            // Cheap best-effort child count (used for the info card / box size).
            node.entry_count = fs::read_dir(path).map(|d| d.count()).unwrap_or(0);
        } else {
            // A file's recursive size is just its own size — known immediately.
            node.recursive_size = Some(meta.len());
        }

        Some(node)
    }

    /// Largest recursive size among current nodes that have one (for scaling).
    pub fn max_recursive_size(&self) -> u64 {
        self.nodes
            .iter()
            .filter_map(|n| n.recursive_size)
            .max()
            .unwrap_or(0)
    }

    // --- navigation ---------------------------------------------------------

    /// Descend into a directory node, remembering where we came from.
    pub fn enter(&mut self, idx: usize) -> bool {
        if idx >= self.nodes.len() || self.nodes[idx].kind != Kind::Dir {
            return false;
        }
        let target = self.nodes[idx].path.clone();
        if fs::read_dir(&target).is_err() {
            return false;
        }
        self.breadcrumb.push(self.cwd.clone());
        self.cwd = target;
        self.scan_current();
        true
    }

    /// Go back up to the previous directory. Returns false at the top.
    pub fn go_back(&mut self) -> bool {
        if let Some(prev) = self.breadcrumb.pop() {
            self.cwd = prev;
            self.scan_current();
            true
        } else {
            false
        }
    }

    /// Re-scan the current directory after a filesystem change.
    pub fn refresh(&mut self) {
        let saved = self.page_start;
        self.scan_current();
        if saved < self.all_children.len() {
            self.page_start = saved;
            self.rebuild_page();
        }
    }

    /// A path string for the breadcrumb HUD.
    pub fn location(&self) -> String {
        self.cwd.to_string_lossy().into_owned()
    }

    // --- layout (single level) ----------------------------------------------

    /// Arrange visible nodes in a centered grid on the floor. Bounded to
    /// VIEW_CAP items, so this is always cheap.
    pub fn layout(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        let p = self.params;

        let cols = (n as f32).sqrt().ceil() as usize;
        let cols = cols.max(1);
        let rows = (n + cols - 1) / cols;

        let cell = p.dir_size.max(p.file_size) + p.dir_spacing;
        let grid_w = cols as f32 * cell;
        let grid_d = rows as f32 * cell;
        let x0 = -grid_w / 2.0 + cell / 2.0;
        let z0 = -grid_d / 2.0 + cell / 2.0;

        for (i, node) in self.nodes.iter_mut().enumerate() {
            let r = i / cols;
            let c = i % cols;
            let x = x0 + c as f32 * cell;
            let z = z0 + r as f32 * cell;

            let (w, h) = match node.kind {
                Kind::Dir => (p.dir_size, 0.6),
                Kind::More => (p.dir_size, 0.3),
                Kind::File => {
                    let s = p.file_size * (1.0 + (node.size as f32).max(1.0).log10() * 0.05);
                    (s.min(p.dir_size), p.file_height.max(0.15))
                }
            };
            node.vis_size = Vec3::new(w, h, w);
            node.vis_pos = Vec3::new(x, h / 2.0, z);
        }
    }

    /// Layout for disk-usage mode: same grid footprint, but box HEIGHT scales
    /// with each node's recursive size relative to the largest in view. Tall
    /// boxes = space hogs. Re-run cheaply each frame as sizes stream in.
    pub fn layout_usage(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            return;
        }
        let p = self.params;
        let cols = ((n as f32).sqrt().ceil() as usize).max(1);
        let rows = (n + cols - 1) / cols;
        let cell = p.dir_size.max(p.file_size) + p.dir_spacing;
        let grid_w = cols as f32 * cell;
        let grid_d = rows as f32 * cell;
        let x0 = -grid_w / 2.0 + cell / 2.0;
        let z0 = -grid_d / 2.0 + cell / 2.0;

        let max_bytes = self.max_recursive_size().max(1) as f32;
        const MIN_H: f32 = 0.15;
        const MAX_H: f32 = 6.0;

        for (i, node) in self.nodes.iter_mut().enumerate() {
            let r = i / cols;
            let c = i % cols;
            let x = x0 + c as f32 * cell;
            let z = z0 + r as f32 * cell;

            let footprint = p.dir_size * 0.9;
            let h = match node.kind {
                Kind::More => 0.3,
                _ => match node.recursive_size {
                    // Cube-root maps volume→linear height so a 10x file isn't a
                    // 10x-tall spike; still clearly ordered, but readable.
                    Some(bytes) => {
                        let frac = (bytes as f32 / max_bytes).clamp(0.0, 1.0);
                        MIN_H + frac.cbrt() * (MAX_H - MIN_H)
                    }
                    None => MIN_H, // not yet measured: short until its size arrives
                },
            };
            node.vis_size = Vec3::new(footprint, h, footprint);
            node.vis_pos = Vec3::new(x, h / 2.0, z);
        }
    }

    // --- lazy enrichment -----------------------------------------------------

    pub fn ensure_classified(&mut self, idx: usize) {
        if idx < self.nodes.len()
            && self.nodes[idx].content.is_none()
            && self.nodes[idx].kind == Kind::File
        {
            let path = self.nodes[idx].path.clone();
            let size = self.nodes[idx].size;
            self.nodes[idx].content = Some(crate::filetype::classify(&path, size));
        }
    }

    pub fn classify_all(&mut self) {
        for i in 0..self.nodes.len() {
            self.ensure_classified(i);
        }
    }
}

/// Pull mode/uid/gid/timestamps out of platform metadata.
fn extract_meta(meta: &fs::Metadata) -> FileMeta {
    let mut fm = FileMeta {
        atime: meta.accessed().ok(),
        mtime: meta.modified().ok(),
        ctime: meta.created().ok(),
        ..Default::default()
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fm.mode = meta.mode();
        fm.uid = meta.uid();
        fm.gid = meta.gid();
        fm.user = users::lookup_user(fm.uid);
        fm.group = users::lookup_group(fm.gid);
    }
    #[cfg(not(unix))]
    {
        fm.mode = if meta.permissions().readonly() {
            0o444
        } else {
            0o644
        };
        fm.user = "-".to_string();
        fm.group = "-".to_string();
    }

    fm
}

/// Public username lookup for a uid (used by the access viewpoint label).
pub fn lookup_user_public(uid: u32) -> String {
    #[cfg(unix)]
    {
        users::lookup_user(uid)
    }
    #[cfg(not(unix))]
    {
        let _ = uid;
        "me".to_string()
    }
}

/// Minimal uid/gid -> name resolution without pulling in an extra crate.
#[cfg(unix)]
mod users {
    use std::ffi::CStr;
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
    #[repr(C)]
    struct Group {
        gr_name: *const c_char,
        gr_passwd: *const c_char,
        gr_gid: u32,
        gr_mem: *const *const c_char,
    }
    extern "C" {
        fn getpwuid(uid: u32) -> *const Passwd;
        fn getgrgid(gid: u32) -> *const Group;
    }

    pub fn lookup_user(uid: u32) -> String {
        unsafe {
            let pw = getpwuid(uid);
            if pw.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr((*pw).pw_name).to_string_lossy().into_owned()
            }
        }
    }

    pub fn lookup_group(gid: u32) -> String {
        unsafe {
            let gr = getgrgid(gid);
            if gr.is_null() {
                "unknown".to_string()
            } else {
                CStr::from_ptr((*gr).gr_name).to_string_lossy().into_owned()
            }
        }
    }
}
