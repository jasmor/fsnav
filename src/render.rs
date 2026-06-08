//! All drawing: node boxes, links, 3D labels and the 2D info card.
//!
//! Combines the roles of the original `vis.cc`, `colorman.cc` and the file
//! stats overlay, but with a cleaner, more modern palette and UI styling.

use crate::fstree::{Kind, Node};
use macroquad::prelude::*;

// --- palette -------------------------------------------------------------

// A calmer, more contemporary scheme than the original SGI-green look.
pub const SKY_TOP: Color = Color::new(0.09, 0.11, 0.16, 1.0);
pub const SKY_BOTTOM: Color = Color::new(0.16, 0.20, 0.28, 1.0);
pub const FLOOR: Color = Color::new(0.13, 0.16, 0.21, 1.0);
pub const GRID: Color = Color::new(0.24, 0.30, 0.39, 1.0);

const DIR_COLOR: Color = Color::new(0.30, 0.52, 0.92, 1.0);
const FILE_COLOR: Color = Color::new(0.85, 0.55, 0.32, 1.0);
const MORE_COLOR: Color = Color::new(0.55, 0.55, 0.62, 1.0); // "+N more" marker

// Access-mode palette: how the viewpoint can interact with a node.
const ACC_NONE: Color = Color::new(0.32, 0.34, 0.40, 1.0); // no read
const ACC_READ: Color = Color::new(0.30, 0.55, 0.85, 1.0); // read-only
const ACC_WRITE: Color = Color::new(0.35, 0.78, 0.55, 1.0); // read+write
const ACC_EXEC: Color = Color::new(0.85, 0.75, 0.35, 1.0); // executable
const ACC_RISK: Color = Color::new(0.95, 0.40, 0.35, 1.0); // world-writable/setuid/escape

/// What the box colors mean right now.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    Kind,   // classic dir/file coloring
    Access, // color by the active viewpoint's effective access
    Usage,  // heat scale by recursive size (disk-usage mode)
}

/// Color a node. `max_bytes` is only used in Usage mode (the largest recursive
/// size in view, for the heat scale).
pub fn node_color(node: &Node, selected: bool, mode: ColorMode, max_bytes: u64) -> Color {
    let base = match node.kind {
        Kind::More => MORE_COLOR,
        _ => match mode {
            ColorMode::Kind => match node.kind {
                Kind::Dir => DIR_COLOR,
                _ => FILE_COLOR,
            },
            ColorMode::Access => access_color(node),
            ColorMode::Usage => usage_color(node, max_bytes),
        },
    };
    if selected {
        // brighten for the hovered node
        Color::new(
            (base.r + 0.18).min(1.0),
            (base.g + 0.18).min(1.0),
            (base.b + 0.18).min(1.0),
            1.0,
        )
    } else {
        base
    }
}

/// Heat scale from cool (small) to hot (large) by recursive size fraction.
fn usage_color(node: &Node, max_bytes: u64) -> Color {
    match node.recursive_size {
        Some(b) if max_bytes > 0 => usage_ramp((b as f32 / max_bytes as f32).clamp(0.0, 1.0)),
        _ => Color::new(0.30, 0.33, 0.40, 1.0), // unmeasured: neutral grey
    }
}

/// Pick a color from a node's computed access (falls back to kind colors if
/// access hasn't been computed yet).
fn access_color(node: &Node) -> Color {
    match node.access {
        Some(a) => {
            if a.any_risk() {
                ACC_RISK
            } else if a.exec && node.kind == Kind::File {
                ACC_EXEC
            } else if a.write {
                ACC_WRITE
            } else if a.read {
                ACC_READ
            } else {
                ACC_NONE
            }
        }
        None => match node.kind {
            Kind::Dir => DIR_COLOR,
            _ => FILE_COLOR,
        },
    }
}

// --- 3D scene ------------------------------------------------------------

/// Draw the ground plane and a faint reference grid.
pub fn draw_env() {
    // Large lit quad as the floor.
    draw_plane(vec3(0.0, 0.0, 0.0), vec2(500.0, 500.0), None, FLOOR);

    // Subtle grid lines fading out from the origin.
    let half = 60.0;
    let step = 2.0;
    let mut x = -half;
    while x <= half {
        draw_line_3d(vec3(x, 0.01, -half), vec3(x, 0.01, half), GRID);
        draw_line_3d(vec3(-half, 0.01, x), vec3(half, 0.01, x), GRID);
        x += step;
    }
}

/// Draw a node as a shaded box. macroquad's `draw_cube` uses the cube's
/// center, which matches our `vis_pos`.
///
/// `selected` = hovered; `marked` = on the clipboard (pending copy/move).
pub fn draw_node(node: &Node, selected: bool, marked: bool, mode: ColorMode, max_bytes: u64) {
    let col = node_color(node, selected, mode, max_bytes);
    draw_cube(node.vis_pos, node.vis_size, None, col);
    let edge = Color::new(col.r * 0.5, col.g * 0.5, col.b * 0.5, 1.0);
    draw_cube_wires(node.vis_pos, node.vis_size, edge);

    // A clipboard-marked node gets a bright dashed outline one size up.
    if marked {
        let s = node.vis_size * 1.15 + Vec3::splat(0.05);
        draw_cube_wires(node.vis_pos, s, Color::new(1.0, 0.95, 0.4, 1.0));
    }

    // Risk highlight: a red wire shell around dangerous nodes in access mode.
    if mode == ColorMode::Access {
        if let Some(a) = node.access {
            if a.any_risk() {
                let s = node.vis_size * 1.08 + Vec3::splat(0.03);
                draw_cube_wires(node.vis_pos, s, Color::new(1.0, 0.35, 0.3, 0.9));
            }
        }
    }
}

// --- labels --------------------------------------------------------------

/// Project a world point to screen space using the active camera.
/// Returns `None` if the point is behind the camera.
pub fn project(p: Vec3, mvp: &Mat4) -> Option<Vec2> {
    let clip = *mvp * vec4(p.x, p.y, p.z, 1.0);
    if clip.w <= 0.0 {
        return None;
    }
    let ndc = clip.truncate() / clip.w;
    Some(vec2(
        (ndc.x * 0.5 + 0.5) * screen_width(),
        (1.0 - (ndc.y * 0.5 + 0.5)) * screen_height(),
    ))
}

/// Truncate a string to fit `max_px` at the given font size, adding an ellipsis.
fn fit_text(s: &str, font: f32, max_px: f32) -> String {
    let full = measure_text(s, None, font as u16, 1.0).width;
    if full <= max_px {
        return s.to_string();
    }
    // Binary-ish trim by characters until it fits (names are short, so linear
    // from the end is fine and avoids splitting multibyte chars incorrectly).
    let chars: Vec<char> = s.chars().collect();
    let mut keep = chars.len();
    while keep > 1 {
        keep -= 1;
        let candidate: String = chars[..keep].iter().collect::<String>() + "...";
        if measure_text(&candidate, None, font as u16, 1.0).width <= max_px {
            return candidate;
        }
    }
    "...".to_string()
}

/// A simple screen-space rectangle for overlap tests.
#[derive(Clone, Copy)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}
impl Rect {
    fn overlaps(&self, o: &Rect) -> bool {
        self.x < o.x + o.w && self.x + self.w > o.x && self.y < o.y + o.h && self.y + self.h > o.y
    }
}

/// Draw labels for all visible nodes in one pass, with collision suppression.
///
/// Readability strategy:
/// * Every name is truncated with an ellipsis to a sensible max width.
/// * A translucent dark "pill" is drawn behind each label so it stays legible
///   over the boxes and the grid.
/// * If a label's box would overlap one already placed, it's skipped — so we
///   never get the unreadable pile of stacked text.
/// * The hovered node is laid out first (highest priority) and shows its FULL,
///   untruncated name, so the thing you're pointing at is always fully readable.
pub fn draw_labels(
    nodes: &[Node],
    mvp: &Mat4,
    hovered: Option<usize>,
    visible: Option<&[bool]>,
) {
    // Build a draw order: hovered first, then directories, then everything else,
    // nearer-to-camera (larger projected y is lower on screen ≈ closer) leans
    // later. We approximate priority simply; the hovered-first rule is what
    // matters most.
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by_key(|&i| {
        let is_hover = hovered == Some(i);
        let rank = match nodes[i].kind {
            Kind::Dir => 1,
            Kind::More => 1,
            Kind::File => 2,
        };
        // hovered gets rank 0 (drawn/placed first)
        if is_hover {
            0
        } else {
            rank
        }
    });

    let mut placed: Vec<Rect> = Vec::with_capacity(nodes.len());

    for &i in &order {
        // Honor the visibility filter (used by search mode).
        if let Some(vis) = visible {
            if !vis.get(i).copied().unwrap_or(true) {
                continue;
            }
        }
        let node = &nodes[i];
        let is_hover = hovered == Some(i);
        let anchor = node.vis_pos + vec3(0.0, node.vis_size.y, 0.0);
        let sp = match project(anchor, mvp) {
            Some(p) => p,
            None => continue,
        };

        let (font, color) = match node.kind {
            Kind::Dir => (20.0, Color::new(0.95, 0.97, 1.0, 1.0)),
            Kind::More => (19.0, Color::new(0.88, 0.91, 0.98, 1.0)),
            Kind::File => (15.0, Color::new(0.93, 0.93, 0.96, 1.0)),
        };

        // Hovered shows full name; others are capped to a readable width.
        let max_px = if is_hover { 600.0 } else { 150.0 };
        let text = if is_hover {
            node.name.clone()
        } else {
            fit_text(&node.name, font, max_px)
        };

        let dims = measure_text(&text, None, font as u16, 1.0);
        let pad_x = 6.0;
        let pad_y = 3.0;
        let w = dims.width + pad_x * 2.0;
        let h = dims.height + pad_y * 2.0;
        let x = sp.x - w / 2.0;
        let y = sp.y - h - 2.0;

        let rect = Rect { x, y, w, h };

        // Collision: skip if it overlaps something already placed — unless it's
        // the hovered label, which always wins (placed first anyway).
        if !is_hover && placed.iter().any(|r| rect.overlaps(r)) {
            continue;
        }
        placed.push(rect);

        // Pill background for legibility.
        let bg_alpha = if is_hover { 0.92 } else { 0.7 };
        draw_rounded_rect(x, y, w, h, h / 2.0, Color::new(0.06, 0.08, 0.12, bg_alpha));
        if is_hover {
            draw_rounded_rect_lines(
                x,
                y,
                w,
                h,
                h / 2.0,
                1.0,
                Color::new(0.45, 0.62, 0.95, 0.8),
            );
        }

        // draw_text places the BASELINE at (x, y); the glyphs span upward by
        // offset_y. To sit the text inside the pill with `pad_y` above the
        // glyph tops, put the baseline at y + pad_y + offset_y.
        let tx = x + pad_x;
        let baseline = y + pad_y + dims.offset_y;
        draw_text(&text, tx, baseline, font, color);
    }
}

// --- info card -----------------------------------------------------------

/// Format a byte count the way the original `draw_file_stats` did.
fn human_size(bytes: u64) -> String {
    const K: f64 = 1024.0;
    let b = bytes as f64;
    if b < K {
        format!("{bytes} bytes")
    } else if b < K * K {
        format!("{:.1} KB", b / K)
    } else if b < K * K * K {
        format!("{:.1} MB", b / (K * K))
    } else {
        format!("{:.1} GB", b / (K * K * K))
    }
}

/// `rwxr-xr-x`-style permission string from a unix mode.
fn mode_str(mode: u32) -> String {
    let bits = ['r', 'w', 'x'];
    let mut s = String::with_capacity(9);
    for grp in (0..3).rev() {
        for (i, ch) in bits.iter().enumerate() {
            let bit = 1 << (grp * 3 + (2 - i));
            s.push(if mode & bit != 0 { *ch } else { '-' });
        }
    }
    s
}

fn fmt_time(t: Option<std::time::SystemTime>) -> String {
    use std::time::UNIX_EPOCH;
    match t.and_then(|t| t.duration_since(UNIX_EPOCH).ok()) {
        Some(d) => format_unix(d.as_secs() as i64),
        None => "-".to_string(),
    }
}

/// Tiny civil-date formatter (UTC) so we don't need a date crate.
fn format_unix(secs: i64) -> String {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // days since 1970 -> y/m/d (civil calendar algorithm by Howard Hinnant)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02} {h:02}:{mi:02}:{s:02} UTC")
}

/// Draw a rounded info card near the cursor with the file's attributes,
/// content classification, and effective access for the active viewpoint.
/// If `thumbnail` is provided (image files), a preview is drawn atop the card.
pub fn draw_file_card(
    node: &Node,
    anchor: Vec2,
    viewpoint_label: &str,
    thumbnail: Option<&Texture2D>,
) {
    let pad = 14.0;
    let line_h = 22.0;
    let title_h = 30.0;

    // Content classification row (may be "(press i / hover to scan)").
    let content_str = match node.content {
        Some(c) => format!("{}  ·  {:.2} bits/byte", c.category.describe(), c.entropy),
        None => "not scanned".to_string(),
    };

    // Access summary for the current viewpoint.
    let access_str = match node.access {
        Some(a) => {
            let r = if a.read { "r" } else { "-" };
            let w = if a.write { "w" } else { "-" };
            let x = if a.exec { "x" } else { "-" };
            format!("{r}{w}{x}  (as {viewpoint_label})")
        }
        None => "-".to_string(),
    };

    // Collect risk flags into a short list.
    let mut flags: Vec<&str> = Vec::new();
    if let Some(a) = node.access {
        if a.world_writable {
            flags.push("world-writable");
        }
        if a.setuid {
            flags.push("setuid");
        }
        if a.setgid {
            flags.push("setgid");
        }
        if a.owned_by_other {
            flags.push("owned by other");
        }
        if a.symlink_escapes {
            flags.push("symlink escapes tree");
        }
    }
    let flags_str = if flags.is_empty() {
        "none".to_string()
    } else {
        flags.join(", ")
    };

    // Build the rows. Folders and files share some fields but differ on others
    // (folders show recursive size + item count, not content type).
    let is_dir = node.kind == Kind::Dir;

    let size_str = match node.recursive_size {
        Some(b) if is_dir => format!("{} (total)", human_size(b)),
        Some(b) => human_size(b),
        None if is_dir => "scanning... (disk-usage mode)".to_string(),
        None => human_size(node.size),
    };

    let mut rows: Vec<(&str, String)> = Vec::new();
    if is_dir {
        rows.push(("items", format!("{}", node.entry_count)));
        rows.push(("size", size_str.clone()));
    } else {
        rows.push(("type", content_str));
    }
    rows.push(("access", access_str));
    rows.push(("risk", flags_str));
    if !is_dir {
        rows.push(("size", size_str.clone()));
    }
    rows.push(("perms", mode_str(node.meta.mode)));
    rows.push(("user", format!("{} ({})", node.meta.user, node.meta.uid)));
    rows.push(("group", format!("{} ({})", node.meta.group, node.meta.gid)));
    rows.push(("modified", fmt_time(node.meta.mtime)));
    rows.push(("created", fmt_time(node.meta.ctime)));

    // Size the card to its contents.
    let label_w = 92.0;
    let mut content_w: f32 = measure_text(&node.name, None, 22, 1.0).width;
    for (_, v) in &rows {
        let w = label_w + measure_text(v, None, 16, 1.0).width;
        content_w = content_w.max(w);
    }

    // If we have a thumbnail, reserve a preview box (max 260x200, keep aspect).
    let preview = thumbnail.map(|tex| {
        let (tw, th) = (tex.width(), tex.height());
        let max_w = 260.0_f32;
        let max_h = 200.0_f32;
        let scale = (max_w / tw).min(max_h / th).min(1.0);
        (tex, (tw * scale).max(1.0), (th * scale).max(1.0))
    });
    let preview_h = preview.map(|(_, _, h)| h + pad).unwrap_or(0.0);
    if let Some((_, pw, _)) = preview {
        content_w = content_w.max(pw);
    }

    let w = content_w + pad * 2.0;
    let h = title_h + rows.len() as f32 * line_h + pad * 2.0 + preview_h;

    // Keep the card on screen, offset from the cursor.
    let mut x = anchor.x + 18.0;
    let mut y = anchor.y + 18.0;
    if x + w > screen_width() {
        x = anchor.x - w - 18.0;
    }
    if y + h > screen_height() {
        y = screen_height() - h - 8.0;
    }
    x = x.max(8.0);
    y = y.max(8.0);

    // Card background: translucent dark panel with a soft border.
    draw_rounded_rect(x, y, w, h, 12.0, Color::new(0.07, 0.09, 0.13, 0.94));
    draw_rounded_rect_lines(x, y, w, h, 12.0, 1.5, Color::new(0.45, 0.62, 0.95, 0.6));

    // Title (file name) in accent color, with a content badge.
    let mut ty = y + pad + 18.0;
    let title = match node.content {
        Some(c) => format!("[{}] {}", c.category.badge(), node.name),
        None => node.name.clone(),
    };
    draw_text(&title, x + pad, ty, 22.0, Color::new(1.0, 0.80, 0.50, 1.0));
    ty += title_h - 4.0;

    // Image preview, centered, just under the title.
    if let Some((tex, pw, ph)) = preview {
        let px = x + (w - pw) / 2.0;
        draw_texture_ex(
            tex,
            px,
            ty,
            WHITE,
            DrawTextureParams {
                dest_size: Some(vec2(pw, ph)),
                ..Default::default()
            },
        );
        ty += ph + pad;
    }

    let key_col = Color::new(0.60, 0.68, 0.80, 1.0);
    let val_col = Color::new(0.92, 0.94, 0.98, 1.0);
    let risk_col = Color::new(1.0, 0.5, 0.45, 1.0);
    for (k, v) in &rows {
        draw_text(k, x + pad, ty, 16.0, key_col);
        // highlight a non-empty risk row in red
        let vc = if *k == "risk" && v != "none" {
            risk_col
        } else {
            val_col
        };
        draw_text(v, x + pad + label_w, ty, 16.0, vc);
        ty += line_h;
    }
}

// --- HUD widgets: legend, toast, search bar, confirm dialog ---------------

/// Color legend for the access view (drawn top-right).
pub fn draw_access_legend() {
    let items = [
        (ACC_NONE, "no access"),
        (ACC_READ, "read"),
        (ACC_WRITE, "read+write"),
        (ACC_EXEC, "executable"),
        (ACC_RISK, "risky (world-writable / setuid / escape)"),
    ];
    let pad = 12.0;
    let line_h = 22.0;
    let sw = 16.0;
    let mut maxw = measure_text("access view", None, 18, 1.0).width;
    for (_, t) in &items {
        maxw = maxw.max(sw + 8.0 + measure_text(t, None, 16, 1.0).width);
    }
    let w = maxw + pad * 2.0;
    let h = 28.0 + items.len() as f32 * line_h + pad;
    let x = screen_width() - w - 12.0;
    let y = 12.0;

    draw_rounded_rect(x, y, w, h, 10.0, Color::new(0.07, 0.09, 0.13, 0.9));
    draw_rounded_rect_lines(x, y, w, h, 10.0, 1.5, Color::new(0.45, 0.62, 0.95, 0.5));
    draw_text("access view", x + pad, y + pad + 12.0, 18.0, Color::new(0.95, 0.97, 1.0, 1.0));
    let mut ty = y + pad + 12.0 + 22.0;
    for (c, t) in &items {
        draw_rectangle(x + pad, ty - 12.0, sw, sw, *c);
        draw_text(t, x + pad + sw + 8.0, ty, 16.0, Color::new(0.88, 0.91, 0.96, 1.0));
        ty += line_h;
    }
}

/// Legend for disk-usage mode: a small heat gradient (small → large) plus a
/// "scanning…" note while the background size walk is still running.
pub fn draw_usage_legend(scanning: bool) {
    let pad = 12.0;
    let title = "disk usage";
    let note = if scanning {
        "scanning... (taller = bigger)"
    } else {
        "taller / hotter = bigger"
    };
    let bar_w: f32 = 180.0;
    let bar_h = 14.0;
    let w = bar_w.max(measure_text(note, None, 15, 1.0).width) + pad * 2.0;
    let h = 28.0 + 18.0 + bar_h + 22.0 + pad;
    let x = screen_width() - w - 12.0;
    let y = 12.0;

    draw_rounded_rect(x, y, w, h, 10.0, Color::new(0.07, 0.09, 0.13, 0.9));
    draw_rounded_rect_lines(x, y, w, h, 10.0, 1.5, Color::new(0.45, 0.62, 0.95, 0.5));
    draw_text(title, x + pad, y + pad + 12.0, 18.0, Color::new(0.95, 0.97, 1.0, 1.0));

    // Heat gradient bar (sampled in vertical slices across the same ramp the
    // boxes use, so the legend matches the scene).
    let by = y + pad + 22.0;
    let slices = bar_w as i32;
    for i in 0..slices {
        let frac = i as f32 / slices as f32;
        let col = usage_ramp(frac);
        draw_rectangle(x + pad + i as f32, by, 1.5, bar_h, col);
    }
    draw_text("small", x + pad, by + bar_h + 16.0, 14.0, Color::new(0.7, 0.76, 0.86, 1.0));
    let large_w = measure_text("large", None, 14, 1.0).width;
    draw_text(
        "large",
        x + pad + bar_w - large_w,
        by + bar_h + 16.0,
        14.0,
        Color::new(0.7, 0.76, 0.86, 1.0),
    );

    let note_col = if scanning {
        Color::new(1.0, 0.9, 0.5, 1.0)
    } else {
        Color::new(0.7, 0.76, 0.86, 0.9)
    };
    draw_text(note, x + pad, by + bar_h + 38.0, 15.0, note_col);
}

/// The bare heat ramp used by both the boxes and the legend (0=small, 1=large).
fn usage_ramp(frac: f32) -> Color {
    let f = frac.powf(0.5);
    if f < 0.5 {
        let t = f / 0.5;
        Color::new(0.25 + 0.10 * t, 0.45 + 0.35 * t, 0.85 - 0.45 * t, 1.0)
    } else {
        let t = (f - 0.5) / 0.5;
        Color::new(0.35 + 0.60 * t, 0.80 - 0.45 * t, 0.40 - 0.30 * t, 1.0)
    }
}

/// A centered notice for when the current directory can't be shown (permission
/// denied, empty, etc.) — so the scene never just looks broken/blank.
pub fn draw_center_notice(title: &str, detail: &str) {
    let w = 460.0;
    let h = 110.0;
    let x = (screen_width() - w) / 2.0;
    let y = (screen_height() - h) / 2.0;
    draw_rounded_rect(x, y, w, h, 12.0, Color::new(0.10, 0.11, 0.15, 0.92));
    draw_rounded_rect_lines(x, y, w, h, 12.0, 1.5, Color::new(0.5, 0.55, 0.65, 0.5));
    let tw = measure_text(title, None, 22, 1.0).width;
    draw_text(title, x + (w - tw) / 2.0, y + 42.0, 22.0, Color::new(0.95, 0.85, 0.6, 1.0));
    let dw = measure_text(detail, None, 16, 1.0).width;
    draw_text(
        detail,
        x + (w - dw) / 2.0,
        y + 72.0,
        16.0,
        Color::new(0.8, 0.84, 0.92, 1.0),
    );
}

/// A transient status message (op results, errors).
pub fn draw_toast(msg: &str, is_error: bool, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let pad = 14.0;
    let tw = measure_text(msg, None, 18, 1.0).width;
    let w = tw + pad * 2.0;
    let h = 36.0;
    let x = (screen_width() - w) / 2.0;
    let y = screen_height() - h - 20.0;
    let bg = if is_error {
        Color::new(0.35, 0.10, 0.10, 0.92 * alpha)
    } else {
        Color::new(0.08, 0.18, 0.12, 0.92 * alpha)
    };
    let border = if is_error {
        Color::new(1.0, 0.45, 0.4, 0.8 * alpha)
    } else {
        Color::new(0.4, 0.9, 0.55, 0.8 * alpha)
    };
    draw_rounded_rect(x, y, w, h, 10.0, bg);
    draw_rounded_rect_lines(x, y, w, h, 10.0, 1.5, border);
    draw_text(msg, x + pad, y + 24.0, 18.0, Color::new(1.0, 1.0, 1.0, alpha));
}

/// The search input bar (drawn bottom-left when search is active).
pub fn draw_search_bar(query: &str, match_count: usize) {
    let pad = 12.0;
    let label = format!("/ {query}");
    let info = format!("{match_count} match{}", if match_count == 1 { "" } else { "es" });
    let w = 360.0;
    let h = 38.0;
    let x = 12.0;
    let y = screen_height() - h - 12.0;
    draw_rounded_rect(x, y, w, h, 10.0, Color::new(0.07, 0.09, 0.13, 0.95));
    draw_rounded_rect_lines(x, y, w, h, 10.0, 1.5, Color::new(0.95, 0.85, 0.4, 0.8));
    draw_text(&label, x + pad, y + 25.0, 20.0, Color::new(1.0, 0.95, 0.7, 1.0));
    let iw = measure_text(&info, None, 16, 1.0).width;
    draw_text(&info, x + w - iw - pad, y + 24.0, 16.0, Color::new(0.7, 0.76, 0.86, 1.0));
}

/// A modal confirmation prompt (e.g. for trashing a directory). Returns its
/// drawn — input handling stays in main.
pub fn draw_confirm(title: &str, detail: &str) {
    // dim the scene
    draw_rectangle(0.0, 0.0, screen_width(), screen_height(), Color::new(0.0, 0.0, 0.0, 0.5));
    let w = 520.0;
    let h = 170.0;
    let x = (screen_width() - w) / 2.0;
    let y = (screen_height() - h) / 2.0;
    draw_rounded_rect(x, y, w, h, 14.0, Color::new(0.10, 0.06, 0.07, 0.98));
    draw_rounded_rect_lines(x, y, w, h, 14.0, 2.0, Color::new(1.0, 0.45, 0.4, 0.9));
    draw_text(title, x + 22.0, y + 42.0, 24.0, Color::new(1.0, 0.7, 0.65, 1.0));
    draw_text(detail, x + 22.0, y + 80.0, 18.0, Color::new(0.92, 0.92, 0.95, 1.0));
    draw_text(
        "press Y to confirm  ·  N or Esc to cancel",
        x + 22.0,
        y + h - 24.0,
        18.0,
        Color::new(0.8, 0.84, 0.92, 1.0),
    );
}

// --- small 2D drawing helpers (rounded rects) ----------------------------

fn draw_rounded_rect(x: f32, y: f32, w: f32, h: f32, r: f32, col: Color) {
    let r = r.min(w / 2.0).min(h / 2.0);
    // center cross + four edge rects
    draw_rectangle(x + r, y, w - 2.0 * r, h, col);
    draw_rectangle(x, y + r, w, h - 2.0 * r, col);
    // rounded corners
    draw_circle(x + r, y + r, r, col);
    draw_circle(x + w - r, y + r, r, col);
    draw_circle(x + r, y + h - r, r, col);
    draw_circle(x + w - r, y + h - r, r, col);
}

fn draw_rounded_rect_lines(x: f32, y: f32, w: f32, h: f32, r: f32, thick: f32, col: Color) {
    let r = r.min(w / 2.0).min(h / 2.0);
    draw_line(x + r, y, x + w - r, y, thick, col);
    draw_line(x + r, y + h, x + w - r, y + h, thick, col);
    draw_line(x, y + r, x, y + h - r, thick, col);
    draw_line(x + w, y + r, x + w, y + h - r, thick, col);
    draw_arc_lines(x + r, y + r, r, thick, col, 180.0, 270.0);
    draw_arc_lines(x + w - r, y + r, r, thick, col, 270.0, 360.0);
    draw_arc_lines(x + r, y + h - r, r, thick, col, 90.0, 180.0);
    draw_arc_lines(x + w - r, y + h - r, r, thick, col, 0.0, 90.0);
}

fn draw_arc_lines(cx: f32, cy: f32, r: f32, thick: f32, col: Color, a0: f32, a1: f32) {
    let segs = 8;
    let mut prev: Option<Vec2> = None;
    for i in 0..=segs {
        let a = (a0 + (a1 - a0) * i as f32 / segs as f32).to_radians();
        let p = vec2(cx + a.cos() * r, cy + a.sin() * r);
        if let Some(pp) = prev {
            draw_line(pp.x, pp.y, p.x, p.y, thick, col);
        }
        prev = Some(p);
    }
}
