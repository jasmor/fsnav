//! fsnav — a 3D filesystem navigator inspired by SGI's `fsn`.
//!
//! Rust port of John Tsiombikas's original C++ program, using macroquad.
//! Beyond the original visualizer, this build adds practical, everyday tools:
//!   * agent/user access coloring (toggle viewpoint with `v`, mode with `c`)
//!   * content classification: compressed / archive / encrypted / media / exec
//!   * safe file operations: copy, move, and delete-to-Trash, with animation
//!   * search / filter / jump-to-file (`/`)
//!
//! Controls are summarized in the on-screen HUD and the README.

mod access;
mod effects;
mod filetype;
mod fstree;
mod media;
mod ops;
mod pick;
mod render;
mod usage;

use access::Viewpoint;
use fstree::{Kind, LayoutParams, SortMode, Tree};
use macroquad::prelude::*;
use pick::Ray;
use render::ColorMode;

/// Eased camera fly-to, matching the original 0.8s transition.
const TRANS_TIME: f32 = 0.8;
const DOUBLE_CLICK_INTERVAL: f64 = 0.4;

struct CameraState {
    theta: f32,
    phi: f32,
    dist: f32,
    from: Vec3,
    target: Vec3,
    dist_from: f32, // dist at start of fly-to (for animated zoom)
    dist_target: f32, // dist to ease toward during fly-to
    motion_start: f64, // time the current fly-to began
}

impl CameraState {
    fn new() -> Self {
        CameraState {
            theta: 0.0,
            phi: 25.0,
            dist: 5.0,
            from: Vec3::ZERO,
            target: Vec3::ZERO,
            dist_from: 5.0,
            dist_target: 5.0,
            motion_start: -10.0,
        }
    }

    /// Interpolated look-at point for the in-progress fly-to.
    fn focus(&self, now: f64) -> Vec3 {
        let t = (((now - self.motion_start) as f32) / TRANS_TIME).clamp(0.0, 1.0);
        // smoothstep easing for a softer, more modern motion
        let t = t * t * (3.0 - 2.0 * t);
        self.from.lerp(self.target, t)
    }

    /// Eased distance for the in-progress fly-to (zoom toward/away).
    fn cur_dist(&self, now: f64) -> f32 {
        let t = (((now - self.motion_start) as f32) / TRANS_TIME).clamp(0.0, 1.0);
        let t = t * t * (3.0 - 2.0 * t);
        self.dist_from + (self.dist_target - self.dist_from) * t
    }

    /// Eye position derived from orbit angles around the focus point.
    fn eye(&self, focus: Vec3, dist: f32) -> Vec3 {
        let phi = self.phi.to_radians();
        let theta = self.theta.to_radians();
        // Spherical orbit: phi is elevation, theta is azimuth.
        let dir = vec3(
            phi.cos() * theta.sin(),
            phi.sin(),
            phi.cos() * theta.cos(),
        );
        focus + dir * dist
    }
}

/// Vertical gradient backdrop drawn before the 3D scene.
fn draw_background() {
    let h = screen_height();
    let w = screen_width();
    let steps = 64;
    for i in 0..steps {
        let t0 = i as f32 / steps as f32;
        let t1 = (i + 1) as f32 / steps as f32;
        let col = Color::new(
            render::SKY_TOP.r + (render::SKY_BOTTOM.r - render::SKY_TOP.r) * t0,
            render::SKY_TOP.g + (render::SKY_BOTTOM.g - render::SKY_TOP.g) * t0,
            render::SKY_TOP.b + (render::SKY_BOTTOM.b - render::SKY_TOP.b) * t0,
            1.0,
        );
        draw_rectangle(0.0, t0 * h, w, (t1 - t0) * h + 1.0, col);
    }
}

/// Build a mouse ray in world space from the current camera + cursor.
fn mouse_ray(eye: Vec3, focus: Vec3, fov: f32, mouse: Vec2) -> Ray {
    let aspect = screen_width() / screen_height();
    let forward = (focus - eye).normalize();
    let world_up = vec3(0.0, 1.0, 0.0);
    let right = forward.cross(world_up).normalize();
    let up = right.cross(forward).normalize();

    // Normalized device coords in [-1, 1].
    let ndc_x = mouse.x / screen_width() * 2.0 - 1.0;
    let ndc_y = 1.0 - mouse.y / screen_height() * 2.0;

    let tan_half = (fov.to_radians() * 0.5).tan();
    let dir = (forward
        + right * (ndc_x * tan_half * aspect)
        + up * (ndc_y * tan_half))
        .normalize();

    Ray { origin: eye, dir }
}

fn window_conf() -> Conf {
    Conf {
        window_title: "fsnav — filesystem visualizer".to_owned(),
        window_width: 1000,
        window_height: 700,
        high_dpi: true,
        sample_count: 4, // MSAA for smoother edges
        ..Default::default()
    }
}

/// A pending operation awaiting Y/N confirmation.
struct PendingConfirm {
    title: String,
    detail: String,
    action: ConfirmAction,
}

enum ConfirmAction {
    Trash(usize),
}

/// A transient status toast.
struct Toast {
    msg: String,
    is_error: bool,
    until: f64,
}

async fn run() {
    // Parse args: [dir] [--agent NAME] [--usage]
    let mut dirname = ".".to_string();
    let mut agent_name: Option<String> = None;
    let mut start_usage = false;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--agent" | "-a" => {
                if i + 1 < args.len() {
                    agent_name = Some(args[i + 1].clone());
                    i += 1;
                }
            }
            "--usage" | "-u" => start_usage = true,
            "--help" | "-h" => {
                print_help();
                return;
            }
            other => dirname = other.to_string(),
        }
        i += 1;
    }

    let mut tree = match Tree::build(&dirname, LayoutParams::default()) {
        Some(t) => t,
        None => {
            eprintln!("could not build tree for '{dirname}'");
            return;
        }
    };

    // Viewpoints: current user, and optionally a named agent. Toggle with `v`.
    let vp_me = Viewpoint::current();
    let vp_agent = agent_name.as_deref().map(Viewpoint::named);
    let mut showing_agent = false;
    tree.recompute_access(pick_vp(&vp_me, &vp_agent, showing_agent));

    let mut cam = CameraState::new();
    let fov = 50.0_f32;
    // Launch into the job the user asked for: disk-usage, else the access view.
    let mut color_mode = if start_usage {
        ColorMode::Usage
    } else {
        ColorMode::Access
    };
    // Experimental fsn-style "platform world" view (toggle with 'm'). Stage 1:
    // visual only — shows the current dir as a deck with subdirectory decks in a
    // ring. Normal single-directory navigation still applies.
    let mut platform_mode = false;
    // Island mode (toggle 'm'): the current directory is shown as a single
    // floating "island" (the deck + its boxes). Navigation is the normal grid
    // interaction — double-click a folder to sail into its island — plus a brief
    // fly-in transition on enter. No subdirectory ring (that proved cumbersome).

    let mut last_left_click = -10.0_f64;
    let mut last_click_pos = Vec2::ZERO;
    let mut prev_mouse = Vec2::from(mouse_position());
    let mut pinned: Option<usize> = None;

    let mut clipboard: Option<(std::path::PathBuf, bool)> = None; // (path, is_cut)
    let mut fx = effects::Effects::default();
    let mut toast: Option<Toast> = None;
    let mut confirm: Option<PendingConfirm> = None;

    // Media: audio playback + image thumbnails.
    let mut media = media::MediaState::default();
    // Disk-usage background scan, active only while in Usage color mode.
    let mut usage_scan: Option<usage::UsageScan> = if start_usage {
        Some(usage::UsageScan::start(&tree.cwd))
    } else {
        None
    };
    // Hover-to-play debounce: which node, and since when.
    let mut hover_node: Option<usize> = None;
    let mut hover_since = 0.0_f64;
    const HOVER_PLAY_DELAY: f64 = 0.5;

    // Search state.
    let mut search_active = false;
    let mut query = String::new();
    let mut matches: Vec<usize> = Vec::new();
    let mut match_cursor = 0usize;

    loop {
        let now = get_time();
        let dt = get_frame_time();
        let dt_mouse = Vec2::from(mouse_position());

        // ============ INPUT ============

        // While a confirm dialog is up, it captures Y/N/Esc and nothing else.
        if confirm.is_some() {
            if is_key_pressed(KeyCode::Y) {
                // take the action out so we no longer borrow `confirm`
                let action = confirm.take().map(|pc| pc.action);
                if let Some(ConfirmAction::Trash(idx)) = action {
                    let vp = pick_vp(&vp_me, &vp_agent, showing_agent).clone();
                    do_trash(&mut tree, idx, &mut fx, &mut toast, now, &vp);
                    // tree was rebuilt; stored indices are now invalid
                    pinned = None;
                    clipboard = None;
                }
            } else if is_key_pressed(KeyCode::N) || is_key_pressed(KeyCode::Escape) {
                confirm = None;
            }
            // fall through to draw, skip other input
        } else if search_active {
            // Search text entry.
            let mut changed = false;
            while let Some(c) = get_char_pressed() {
                if !c.is_control() {
                    query.push(c);
                    changed = true;
                }
            }
            if is_key_pressed(KeyCode::Backspace) {
                query.pop();
                changed = true;
            }
            if is_key_pressed(KeyCode::Escape) {
                search_active = false;
                query.clear();
                matches.clear();
            } else {
                if changed {
                    recompute_matches(&tree, &query, &mut matches, &mut match_cursor);
                    // snap the camera to the first match as you type
                    if !matches.is_empty() {
                        fly_to(&mut cam, &tree, matches[match_cursor], now);
                    }
                }
                if is_key_pressed(KeyCode::Enter) && !matches.is_empty() {
                    // cycle to the next match
                    match_cursor = (match_cursor + 1) % matches.len();
                    fly_to(&mut cam, &tree, matches[match_cursor], now);
                }
            }
        } else {
            // ---- normal input ----

            // camera
            if is_mouse_button_down(MouseButton::Left) {
                let d = dt_mouse - prev_mouse;
                cam.theta -= d.x * 0.5;
                cam.phi = (cam.phi + d.y * 0.5).clamp(5.0, 90.0);
            }
            if is_mouse_button_down(MouseButton::Right) {
                let d = dt_mouse - prev_mouse;
                cam.dist = (cam.dist + d.y * 0.05).max(0.5);
                cam.dist_from = cam.dist;
                cam.dist_target = cam.dist;
            }
            let (_, wheel_y) = mouse_wheel();
            if wheel_y != 0.0 {
                cam.dist = (cam.dist - wheel_y.signum() * 0.5).max(0.5);
                cam.dist_from = cam.dist;
                cam.dist_target = cam.dist;
            }

            // double-click: enter a folder, page the "+N more" marker, or fly
            // to a file. single click (no drag) pins the info card.
            if is_mouse_button_pressed(MouseButton::Left) {
                let moved = (dt_mouse - last_click_pos).length();
                if now - last_left_click < DOUBLE_CLICK_INTERVAL && moved < 4.0 {
                    if let Some(sel) = tree.selection {
                        match tree.nodes[sel].kind {
                            Kind::Dir => {
                                if tree.enter(sel) {
                                    tree.recompute_access(pick_vp(
                                        &vp_me,
                                        &vp_agent,
                                        showing_agent,
                                    ));
                                    pinned = None;
                                    media.stop();
                                    if platform_mode {
                                        // Sail to the child island: swoop in from
                                        // a height onto the new island.
                                        island_fly_in(&mut cam, now);
                                    } else {
                                        reset_camera(&mut cam, now);
                                    }
                                    if color_mode == ColorMode::Usage || tree.sort == SortMode::Size {
                                        usage_scan = Some(usage::UsageScan::start(&tree.cwd));
                                    }
                                }
                            }
                            Kind::More => {
                                // Only a "+N more" marker exists now; clicking
                                // it pages forward (←/→ keys do both ways).
                                tree.next_page();
                                tree.recompute_access(pick_vp(
                                    &vp_me,
                                    &vp_agent,
                                    showing_agent,
                                ));
                                pinned = None;
                                if color_mode == ColorMode::Usage || tree.sort == SortMode::Size {
                                    // New page = new set of children to size.
                                    usage_scan = Some(usage::UsageScan::start(&tree.cwd));
                                }
                            }
                            Kind::File => {
                                // Double-clicking a file flies to it AND pins
                                // its info card (so a sorted list still gives
                                // you details on double-click).
                                fly_to(&mut cam, &tree, sel, now);
                                pinned = Some(sel);
                                tree.ensure_classified(sel);
                            }
                        }
                    }
                    last_left_click = -10.0;
                } else {
                    last_left_click = now;
                    last_click_pos = dt_mouse;
                }
            }
            if is_mouse_button_released(MouseButton::Left)
                && (dt_mouse - last_click_pos).length() < 4.0
            {
                pinned = tree.selection;
                if let Some(idx) = pinned {
                    tree.ensure_classified(idx);
                    // In the spread-out fsn field, a single click flies the
                    // camera across to the clicked box (double-click still
                    // enters a folder). This is the "fly to a directory" feel.
                    if platform_mode {
                        fly_to(&mut cam, &tree, idx, now);
                    }
                }
            }

            // Backspace / B goes back up the directory tree.
            if is_key_pressed(KeyCode::Backspace) || is_key_pressed(KeyCode::B) {
                if tree.go_back() {
                    tree.recompute_access(pick_vp(&vp_me, &vp_agent, showing_agent));
                    pinned = None;
                    media.stop();
                    reset_camera(&mut cam, now);
                    if color_mode == ColorMode::Usage || tree.sort == SortMode::Size {
                        usage_scan = Some(usage::UsageScan::start(&tree.cwd));
                    }
                }
            }

            // Left / Right arrows page through a large directory.
            if is_key_pressed(KeyCode::Right) || is_key_pressed(KeyCode::Left) {
                let before = tree.page_index();
                if is_key_pressed(KeyCode::Right) {
                    tree.next_page();
                } else {
                    tree.prev_page();
                }
                // Only do the post-paging work if the page actually changed.
                if tree.page_index() != before {
                    tree.recompute_access(pick_vp(&vp_me, &vp_agent, showing_agent));
                    pinned = None;
                    if color_mode == ColorMode::Usage || tree.sort == SortMode::Size {
                        usage_scan = Some(usage::UsageScan::start(&tree.cwd));
                    }
                }
            }

            // view toggles
            if is_key_pressed(KeyCode::C) {
                color_mode = match color_mode {
                    ColorMode::Kind => ColorMode::Access,
                    ColorMode::Access => ColorMode::Usage,
                    ColorMode::Usage => ColorMode::Kind,
                };
                // Entering Usage starts a scan of the current dir. Leaving it
                // cancels the scan UNLESS size-sort still needs folder sizes.
                if color_mode == ColorMode::Usage || tree.sort == SortMode::Size {
                    if usage_scan.is_none() {
                        usage_scan = Some(usage::UsageScan::start(&tree.cwd));
                    }
                    set_toast(&mut toast, "scanning sizes...".into(), false, now);
                } else if tree.sort != SortMode::Size {
                    usage_scan = None;
                }
            }
            // Cycle sort order: name -> size -> newest.
            if is_key_pressed(KeyCode::S) {
                tree.set_sort(tree.sort.next());
                set_toast(&mut toast, format!("sort: {}", tree.sort.label()), false, now);
                // Size-sort needs folder sizes; make sure a scan is running.
                if tree.sort == SortMode::Size && usage_scan.is_none() {
                    usage_scan = Some(usage::UsageScan::start(&tree.cwd));
                }
                pinned = None;
            }
            // Toggle fsn field view (wide spread-out field, low camera).
            if is_key_pressed(KeyCode::M) {
                platform_mode = !platform_mode;
                tree.spread = if platform_mode { 3.6 } else { 1.0 };
                tree.layout();
                if platform_mode {
                    if color_mode == ColorMode::Usage {
                        tree.layout_usage();
                    }
                    // Low camera looking across the field, like fsn.
                    cam.phi = 14.0;
                    cam.dist = 38.0;
                    cam.dist_from = 38.0;
                    cam.dist_target = 38.0;
                    cam.from = cam.focus(now);
                    cam.target = Vec3::ZERO;
                    cam.motion_start = now;
                } else {
                    reset_camera(&mut cam, now);
                    cam.phi = 25.0;
                    cam.dist = 5.0;
                    cam.dist_from = 5.0;
                    cam.dist_target = 5.0;
                }
                set_toast(
                    &mut toast,
                    if platform_mode {
                        "fsn field view".into()
                    } else {
                        "grid view".to_string()
                    },
                    false,
                    now,
                );
            }
            if is_key_pressed(KeyCode::V) && vp_agent.is_some() {
                showing_agent = !showing_agent;
                tree.recompute_access(pick_vp(&vp_me, &vp_agent, showing_agent));
                set_toast(
                    &mut toast,
                    format!("viewpoint: {}", pick_vp(&vp_me, &vp_agent, showing_agent).label),
                    false,
                    now,
                );
            }
            if is_key_pressed(KeyCode::I) {
                tree.classify_all();
                set_toast(&mut toast, "scanned all files".into(), false, now);
            }

            // search
            if is_key_pressed(KeyCode::Slash) {
                search_active = true;
                query.clear();
                // drain any queued chars (including the '/') so they don't leak
                while get_char_pressed().is_some() {}
            }

            // ---- file operations ----
            // y = copy (yank), x = cut (move), p = paste, d/Del = trash
            if is_key_pressed(KeyCode::Y) {
                if let Some(idx) = tree.selection {
                    if matches!(tree.nodes[idx].kind, Kind::File | Kind::Dir) {
                        clipboard = Some((tree.nodes[idx].path.clone(), false));
                        set_toast(
                            &mut toast,
                            format!("copied: {}", tree.nodes[idx].name),
                            false,
                            now,
                        );
                    }
                }
            }
            if is_key_pressed(KeyCode::X) {
                if let Some(idx) = tree.selection {
                    if matches!(tree.nodes[idx].kind, Kind::File | Kind::Dir) {
                        clipboard = Some((tree.nodes[idx].path.clone(), true));
                        set_toast(
                            &mut toast,
                            format!("cut: {}", tree.nodes[idx].name),
                            false,
                            now,
                        );
                    }
                }
            }
            if is_key_pressed(KeyCode::P) {
                if let Some((src_path, is_cut)) = clipboard.clone() {
                    // Paste into the hovered folder, else into the current dir.
                    let dst_dir = match tree.selection {
                        Some(i) if tree.nodes[i].kind == Kind::Dir => {
                            tree.nodes[i].path.clone()
                        }
                        _ => tree.cwd.clone(),
                    };
                    let vp = pick_vp(&vp_me, &vp_agent, showing_agent).clone();
                    do_paste(&mut tree, &src_path, &dst_dir, is_cut, &mut fx, &mut toast, now, &vp);
                    pinned = None;
                    if is_cut {
                        clipboard = None; // source no longer exists
                    }
                }
            }
            if is_key_pressed(KeyCode::D) || is_key_pressed(KeyCode::Delete) {
                if let Some(idx) = tree.selection {
                    match tree.nodes[idx].kind {
                        Kind::Dir => {
                            // directory delete needs explicit confirmation
                            confirm = Some(PendingConfirm {
                                title: "Move folder to Trash?".into(),
                                detail: format!("{} and everything in it", tree.nodes[idx].name),
                                action: ConfirmAction::Trash(idx),
                            });
                        }
                        Kind::File => {
                            let vp = pick_vp(&vp_me, &vp_agent, showing_agent).clone();
                            do_trash(&mut tree, idx, &mut fx, &mut toast, now, &vp);
                            pinned = None;
                        }
                        Kind::More => {}
                    }
                }
            }

            // Open the hovered/pinned item in its default app ('o' or Enter).
            // Enter on a focused platform descends into it (platform mode);
            // otherwise 'o'/Enter opens the selected file or folder.
            // Open the hovered/pinned item in its default app ('o' or Enter).
            if is_key_pressed(KeyCode::O) || is_key_pressed(KeyCode::Enter) {
                if let Some(idx) = tree.selection.or(pinned) {
                    if matches!(tree.nodes[idx].kind, Kind::File | Kind::Dir) {
                        match ops::open_path(&tree.nodes[idx].path) {
                            Ok(()) => set_toast(
                                &mut toast,
                                format!("opened {}", tree.nodes[idx].name),
                                false,
                                now,
                            ),
                            Err(e) => set_toast(&mut toast, format!("{e}"), true, now),
                        }
                    }
                }
            }

            if is_key_pressed(KeyCode::Escape) {
                if pinned.is_some() {
                    pinned = None;
                } else {
                    break;
                }
            }
        }

        let hover_info = is_key_down(KeyCode::Space);
        if hover_info {
            pinned = None;
            if let Some(idx) = tree.selection {
                tree.ensure_classified(idx);
            }
        }

        // ============ CAMERA / PROJECTION ============
        let focus = cam.focus(now);
        let cur_dist = cam.cur_dist(now);
        // Once the fly-to settles, commit the eased distance back to cam.dist so
        // wheel/drag zoom continues from where the animation ended.
        let settled = (now - cam.motion_start) as f32 >= TRANS_TIME;
        if settled {
            cam.dist = cam.dist_target;
        }
        let eye = cam.eye(focus, cur_dist);
        let camera = Camera3D {
            position: eye,
            target: focus,
            up: vec3(0.0, 1.0, 0.0),
            fovy: fov.to_radians(),
            aspect: Some(screen_width() / screen_height()),
            projection: Projection::Perspective,
            ..Default::default()
        };
        let proj =
            Mat4::perspective_rh_gl(fov.to_radians(), screen_width() / screen_height(), 0.5, 500.0);
        let view = Mat4::look_at_rh(eye, focus, vec3(0.0, 1.0, 0.0));
        let mvp = proj * view;

        // hover picking (disabled while typing a search)
        if !search_active && confirm.is_none() {
            let ray = mouse_ray(eye, focus, fov, dt_mouse);
            tree.pick(&ray);
        }

        // ---- hover-to-play audio (rest ~0.5s on a sound file to play) ----
        // Track how long the cursor has rested on the current node.
        if tree.selection != hover_node {
            hover_node = tree.selection;
            hover_since = now;
        }
        // After the rest delay, classify a hovered image so its auto-preview
        // card shows full details (cheap + cached; no-op if already done).
        if let Some(idx) = tree.selection {
            if now - hover_since >= HOVER_PLAY_DELAY
                && media::is_image(&tree.nodes[idx].path)
            {
                tree.ensure_classified(idx);
            }
        }
        // Determine the audio file we *should* be playing, if any.
        let want_audio: Option<std::path::PathBuf> = tree.selection.and_then(|idx| {
            let node = &tree.nodes[idx];
            if node.kind == Kind::File
                && media::is_audio(&node.path)
                && now - hover_since >= HOVER_PLAY_DELAY
            {
                Some(node.path.clone())
            } else {
                None
            }
        });
        match want_audio {
            Some(path) => {
                // Non-blocking: kicks off a background decode if needed.
                if !media.is_active(&path) {
                    media.request_audio(&path);
                }
            }
            None => media.stop(),
        }
        // Pick up any finished background decode and start playback.
        media.poll_audio().await;

        fx.update(dt);

        // ---- fold in any directory sizes computed since last frame ----
        // The scan may be running for disk-usage coloring OR for size-sorting,
        // so poll it whenever one is active (not only in Usage color mode).
        if let Some(scan) = usage_scan.as_mut() {
            if scan.dir == tree.cwd {
                let updates = scan.poll();
                for u in updates {
                    tree.record_size(&u.path, u.bytes);
                }
                // If sorting by size, let the order settle as sizes arrive —
                // but keep the pinned card pointed at the same file across the
                // reshuffle by re-resolving its path to the new index.
                let pinned_path = pinned.and_then(|i| tree.nodes.get(i).map(|n| n.path.clone()));
                tree.resort_if_size();
                if let Some(p) = pinned_path {
                    pinned = tree.nodes.iter().position(|n| n.path == p);
                }
            }
        }
        if color_mode == ColorMode::Usage {
            // Cheap (≤20 nodes); keeps box heights in sync as sizes arrive.
            tree.layout_usage();
        }

        let max_bytes = if color_mode == ColorMode::Usage {
            tree.max_recursive_size()
        } else {
            0
        };

        // ============ DRAW ============
        clear_background(BLACK);
        draw_background();

        set_camera(&camera);
        // Both modes now draw the ground plane + grid: fsn mode needs the floor
        // and horizon as orientation anchors (a field floating in void is
        // disorienting). The boxes sit above the floor so there's no z-fight.
        render::draw_env();

        let filtering = search_active && !query.is_empty();
        for (idx, node) in tree.nodes.iter().enumerate() {
            // When searching, dim non-matches.
            let is_match = !filtering || matches.contains(&idx);
            if !is_match {
                continue; // skip dimmed nodes' solid box; draw faint wire only
            }
            let selected = tree.selection == Some(idx);
            let marked = clipboard
                .as_ref()
                .map(|(p, _)| p == &node.path)
                .unwrap_or(false);
            render::draw_node(node, selected, marked, color_mode, max_bytes);
        }
        if filtering {
            // faint wireframes for non-matches so the structure stays visible
            for (idx, node) in tree.nodes.iter().enumerate() {
                if !matches.contains(&idx) {
                    draw_cube_wires(
                        node.vis_pos,
                        node.vis_size,
                        Color::new(0.3, 0.33, 0.4, 0.25),
                    );
                }
            }
        }

        // Spotlight beam on the clicked (pinned) box in fsn field mode — the
        // fsn selection cue. Drawn after boxes so its translucency blends over.
        if platform_mode {
            if let Some(idx) = pinned {
                if idx < tree.nodes.len() {
                    let n = &tree.nodes[idx];
                    render::draw_spotlight(n.vis_pos, n.vis_size);
                }
            }
        }

        fx.draw();

        // 2D overlays
        set_default_camera();

        // Labels: one collision-aware pass. In search mode, restrict to matches.
        let vis_mask: Option<Vec<bool>> = if filtering {
            Some(
                (0..tree.nodes.len())
                    .map(|i| matches.contains(&i))
                    .collect(),
            )
        } else {
            None
        };
        render::draw_labels(
            &tree.nodes,
            &mvp,
            tree.selection,
            vis_mask.as_deref(),
        );

        // Decide what gets the info card:
        //  - hold Space: whatever is hovered (any file/folder)
        //  - click: the pinned item
        //  - otherwise: if you simply rest on an image for the hover delay, show
        //    it automatically — the visual analogue of hover-to-play audio.
        let hovered_image = tree.selection.filter(|&idx| {
            tree.nodes[idx].kind == Kind::File
                && media::is_image(&tree.nodes[idx].path)
                && now - hover_since >= HOVER_PLAY_DELAY
        });
        let info_target = if hover_info {
            tree.selection
        } else {
            pinned.or(hovered_image)
        };
        if let Some(idx) = info_target {
            // Show the card for files and folders (not the "+N more" marker).
            if matches!(tree.nodes[idx].kind, Kind::File | Kind::Dir) {
                // Decode/lookup an image thumbnail for the preview, if any.
                let thumb = if media::is_image(&tree.nodes[idx].path) {
                    media.thumbnail(&tree.nodes[idx].path)
                } else {
                    None
                };
                render::draw_file_card(
                    &tree.nodes[idx],
                    dt_mouse,
                    &pick_vp(&vp_me, &vp_agent, showing_agent).label,
                    thumb.as_ref(),
                    color_mode == ColorMode::Usage || tree.sort == SortMode::Size,
                );
            }
        }

        // Directory-level notices: unreadable folder, or simply empty.
        if let Some(err) = &tree.scan_error {
            render::draw_center_notice("Can't open this folder", err);
        } else if tree.nodes.is_empty() {
            render::draw_center_notice("Empty folder", "nothing here — backspace to go back");
        }

        if color_mode == ColorMode::Access {
            render::draw_access_legend();
        } else if color_mode == ColorMode::Usage {
            let scanning = usage_scan.as_ref().map(|s| !s.done).unwrap_or(false);
            render::draw_usage_legend(scanning);
        }
        if search_active {
            render::draw_search_bar(&query, matches.len());
        }
        if let Some(pc) = &confirm {
            render::draw_confirm(&pc.title, &pc.detail);
        }
        if let Some(t) = &toast {
            let remaining = (t.until - now) as f32;
            let alpha = remaining.clamp(0.0, 1.0);
            render::draw_toast(&t.msg, t.is_error, alpha);
        }
        // Clear an expired toast after the borrow above has ended.
        if toast.as_ref().is_some_and(|t| t.until <= now) {
            toast = None;
        }

        draw_hud(
            &tree.location(),
            tree.nodes.len(),
            tree.total_count(),
            tree.breadcrumb.len(),
            color_mode,
            tree.sort,
            pick_vp(&vp_me, &vp_agent, showing_agent),
            vp_agent.is_some(),
        );

        prev_mouse = dt_mouse;
        next_frame().await;
    }
}

#[macroquad::main(window_conf)]
async fn main() {
    run().await;
}

// ============ helpers ============

/// Choose the active viewpoint reference.
/// Print CLI usage to stdout (for `--help`).
fn print_help() {
    println!(
        "fsnav — 3D filesystem visualizer\n\
         \n\
         USAGE:\n\
         \x20   fsnav [DIR] [OPTIONS]\n\
         \n\
         ARGS:\n\
         \x20   DIR                 Directory to open (default: current dir)\n\
         \n\
         OPTIONS:\n\
         \x20   -u, --usage         Launch straight into disk-usage mode\n\
         \x20   -a, --agent NAME    Also compute access as user/agent NAME (toggle with 'v')\n\
         \x20   -h, --help          Show this help\n\
         \n\
         KEYS (in-app):\n\
         \x20   drag orbit · wheel zoom · dbl-click open · ←/→ page · click/space info\n\
         \x20   c color-mode (kind/access/disk-usage) · s sort (name/size/newest) · v switch viewpoint\n\
         \x20   y copy · x cut · p paste · d trash · o/enter open · / search · i scan-all\n\
         \x20   backspace/b up · esc close card / quit"
    );
}

fn pick_vp<'a>(
    me: &'a Viewpoint,
    agent: &'a Option<Viewpoint>,
    showing_agent: bool,
) -> &'a Viewpoint {
    match (agent, showing_agent) {
        (Some(a), true) => a,
        _ => me,
    }
}

fn fly_to(cam: &mut CameraState, tree: &Tree, idx: usize, now: f64) {
    cam.from = cam.focus(now);
    cam.target = tree.nodes[idx].vis_pos;
    // Animate a zoom-in toward the clicked box so it flies to center and gets
    // closer (rather than just re-centering at the same distance). Don't zoom
    // closer than we already are.
    cam.dist_from = cam.cur_dist(now);
    cam.dist_target = cam.dist_from.min(14.0);
    cam.motion_start = now;
}

/// Camera transition when entering a child folder in fsn mode: ease the look
/// target from a forward offset so the camera sweeps across into the new field,
/// then settles on the origin. Keeps the low fsn angle (phi/dist unchanged).
fn island_fly_in(cam: &mut CameraState, now: f64) {
    cam.from = cam.focus(now) + vec3(0.0, 3.0, 12.0);
    cam.target = Vec3::ZERO;
    let d = cam.cur_dist(now);
    cam.dist = d;
    cam.dist_from = d;
    cam.dist_target = d;
    cam.motion_start = now;
}

/// Ease the camera back to looking at the grid origin (after navigation).
fn reset_camera(cam: &mut CameraState, now: f64) {
    cam.from = cam.focus(now);
    cam.target = Vec3::ZERO;
    // Hold distance steady across the move (no zoom).
    let d = cam.cur_dist(now);
    cam.dist = d;
    cam.dist_from = d;
    cam.dist_target = d;
    cam.motion_start = now;
}

fn set_toast(slot: &mut Option<Toast>, msg: String, is_error: bool, now: f64) {
    *slot = Some(Toast {
        msg,
        is_error,
        until: now + 2.5,
    });
}

fn do_paste(
    tree: &mut Tree,
    src_path: &std::path::Path,
    dst_dir: &std::path::Path,
    is_cut: bool,
    fx: &mut effects::Effects,
    toast: &mut Option<Toast>,
    now: f64,
    vp: &Viewpoint,
) {
    // Animate from the source box (if visible in this view) to the center.
    let from = tree
        .nodes
        .iter()
        .find(|n| n.path == src_path)
        .map(|n| n.vis_pos)
        .unwrap_or(Vec3::new(0.0, 4.0, 0.0));
    let to = tree
        .nodes
        .iter()
        .find(|n| n.path == dst_dir)
        .map(|n| n.vis_pos)
        .unwrap_or(Vec3::ZERO);
    let size = Vec3::splat(tree.params.file_size);

    let result = if is_cut {
        ops::move_to(src_path, dst_dir)
    } else {
        ops::copy(src_path, dst_dir)
    };

    match result {
        Ok(outcome) => {
            if is_cut {
                fx.spawn_move(from, to, size);
            } else {
                fx.spawn_copy(from, to, size);
            }
            set_toast(toast, outcome.message, false, now);
            refresh(tree, vp);
        }
        Err(e) => set_toast(toast, format!("{e}"), true, now),
    }
}

fn do_trash(
    tree: &mut Tree,
    idx: usize,
    fx: &mut effects::Effects,
    toast: &mut Option<Toast>,
    now: f64,
    vp: &Viewpoint,
) {
    let path = tree.nodes[idx].path.clone();
    let at = tree.nodes[idx].vis_pos;
    match ops::trash(&path) {
        Ok(_) => {
            fx.spawn_trash(at);
            set_toast(toast, "moved to Trash".into(), false, now);
            refresh(tree, vp);
        }
        Err(e) => set_toast(toast, format!("{e}"), true, now),
    }
}

/// Re-scan the *current directory* after a mutating op and recompute access.
/// Cheap (one directory) so this is fine to call on every operation.
fn refresh(tree: &mut Tree, vp: &Viewpoint) {
    tree.refresh();
    tree.recompute_access(vp);
}

/// Case-insensitive substring match over node names.
fn recompute_matches(tree: &Tree, query: &str, matches: &mut Vec<usize>, cursor: &mut usize) {
    matches.clear();
    if query.is_empty() {
        *cursor = 0;
        return;
    }
    let q = query.to_lowercase();
    for (i, n) in tree.nodes.iter().enumerate() {
        if n.name.to_lowercase().contains(&q) {
            matches.push(i);
        }
    }
    if *cursor >= matches.len() {
        *cursor = 0;
    }
}

/// On-screen help/legend in the corner.
fn draw_hud(
    location: &str,
    shown: usize,
    total: usize,
    depth: usize,
    mode: ColorMode,
    sort: SortMode,
    vp: &Viewpoint,
    has_agent: bool,
) {
    let mode_str = match mode {
        ColorMode::Kind => "kind",
        ColorMode::Access => "access",
        ColorMode::Usage => "disk usage",
    };
    // Shorten a long path to its last two components for the breadcrumb line.
    let short = {
        let p = std::path::Path::new(location);
        let mut comps: Vec<String> = p
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if comps.len() > 3 {
            let tail = comps.split_off(comps.len() - 2);
            format!(".../{}", tail.join("/"))
        } else {
            location.to_string()
        }
    };
    let count = if total > shown {
        format!("{shown} of {total} items")
    } else {
        format!("{total} items")
    };
    let mut lines = vec![
        format!(
            "{short}    ({count})    sort: {}    color: {mode_str}    as: {}",
            sort.label(),
            vp.label
        ),
        "drag orbit · wheel zoom · dbl-click open · click/space info".to_string(),
        "y copy · x cut · p paste · d trash · o open · / search".to_string(),
        "c color-mode · s sort · m fsn-field · i scan-all · backspace/b up".to_string(),
    ];
    if has_agent {
        lines[3].push_str(" · v viewpoint");
    }
    if depth == 0 {
        // at the start dir, "up" isn't available; note it subtly
        lines[3] = lines[3].replace("backspace/b up", "backspace/b up (at top)");
    }
    // Page navigation hint, placed after the up/back text per request.
    lines[3].push_str(" · left/right arrows: page");
    let mut y = 24.0;
    for (i, line) in lines.iter().enumerate() {
        let size = if i == 0 { 20.0 } else { 16.0 };
        let col = if i == 0 {
            Color::new(0.95, 0.97, 1.0, 0.95)
        } else {
            Color::new(0.70, 0.76, 0.86, 0.8)
        };
        draw_text(line, 16.0, y, size, col);
        y += size + 6.0;
    }
}
