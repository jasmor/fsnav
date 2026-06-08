# fsnav (Rust port)

A 3D filesystem navigator inspired by the early-90s **fsn** program on SGI
workstations. This is a Rust rewrite of John Tsiombikas's original C++ program
(<https://github.com/jtsiomb/fsnav>), using [macroquad](https://macroquad.rs)
instead of GLUT / OpenGL / FreeType, with a refreshed, more modern interface.

It visualizes the filesystem starting from the working directory, or any
directory passed as a command-line argument.

## What it does

fsnav now shows **one directory at a time** as a grid of boxes — directories
first, then files — capped at about 20 items so the view stays readable. When a
directory has more than that, the extra entries collapse into a single
**"+N more"** box; opening it pages through the rest. You descend into a folder
by double-clicking it and walk back up with Backspace. This keeps the program
fast and stable even on directories with tens of thousands of entries (the old
whole-tree version could exhaust memory or the stack there).

## Controls

Navigation
| Action | Effect |
| --- | --- |
| Drag (left mouse) | Orbit the camera |
| Drag (right mouse) / wheel | Zoom in and out |
| Double-click a folder | Open (descend into) it |
| Double-click a file | Fly to it and pin its info card |
| **Left** / **Right** arrows | Page through a large directory |
| **Backspace** / **B** | Go back up one directory |
| Click an item | Pin its info card |
| Hold **Space** | Show info for whatever you're hovering |
| **o** / **Enter** | Open the selected item in its default app |
| **/** | Search the current directory; type to filter, **Enter** cycles |
| **Esc** | Close the pinned card, then quit |

Views
| Key | Effect |
| --- | --- |
| **c** | Cycle color mode: *kind* → *access* → *disk usage* |
| **s** | Cycle sort: *name* → *size* → *newest* |
| **v** | Switch viewpoint (you ↔ `--agent`) in access mode |
| **i** | Scan/classify every visible file's content now |

File operations (safe by design)
| Key | Effect |
| --- | --- |
| **y** | Copy (yank) the hovered item to the clipboard |
| **x** | Cut (move) the hovered item to the clipboard |
| **p** | Paste into the hovered folder, or the current folder |
| **d** / **Del** | Send to Trash (folders require Y/N confirmation) |

The clipboard remembers the item's path, so you can copy something, navigate
into another folder, and paste it there.

## Media

**Hover-to-play audio.** Rest the cursor on a sound file (`.mp3`, `.ogg`,
`.wav`, `.flac`, `.m4a`, …) for about half a second and it begins playing,
looped; move away and it stops. Only one clip plays at a time. The short delay
keeps sounds from firing as the cursor sweeps across the grid.

**Image preview.** Rest on an image file (`.png`, `.jpg`, `.gif`, `.bmp`) for
about half a second — the same gesture as hover-to-play audio — and a thumbnail
appears at the top of the info card, scaled to fit. (It also shows on click or
while holding **Space**.) Decoded thumbnails are cached so re-hovering is
instant. Images are decoded with a dedicated decoder and downscaled, so even a
large photo previews quickly.

## Everyday-use features

**Agent / user access view.** Press `c` to color boxes by *effective access*
rather than by kind: grey = no read, blue = read-only, green = read+write,
yellow = executable, red = risky. "Risky" means world-writable, setuid/setgid,
owned by another user, or a symlink that escapes the visualized tree — the kinds
of things worth knowing when an automated agent has the run of a directory.

Run with `--agent NAME` to compute access as a *different* identity (e.g. a
service account an AI agent runs under) and press `v` to flip between that
identity and your own. The answer to "what can this agent actually touch here?"
becomes a color you can see at a glance.

**Content classification.** Files are sniffed (on hover/click, or all at once
with `i`) using magic bytes plus Shannon entropy, and tagged in the info card:
compressed, archive, encrypted, media, executable, plain text, or — honestly —
"high entropy (encrypted or compressed)" when the bytes look random but no known
header is present. Entropy can't *prove* encryption, so the label says so.

**Safe operations with animation.** Copy/move are real filesystem operations;
**delete always goes to the OS Trash**, never a permanent unlink. Deleting a
folder requires an explicit Y/N confirmation. Each operation plays a short
visual effect (a flying box for copy/move, a scatter burst for trash). After any
change the tree re-scans so the view stays in sync.

**Disk-usage mode.** Press `c` to reach *disk usage* coloring (or launch with
`--usage`): each box's height and color scale with its *recursive* size, so the
space hogs literally tower over everything else. Folder sizes are computed on a
background thread and stream in without freezing the UI. Combine with size-sort
(`s`) to put the biggest items on the first page — the fast answer to "what's
eating my disk?".

**Sort.** Press `s` to cycle *name → size → newest*. Size-sort mixes files and
folders together by total size (folders settle as they're measured); newest-sort
surfaces what changed most recently. The current sort is shown in the HUD.

**Search.** Press `/`, type part of a name to filter the current directory
(non-matches fade to faint wireframes) and fly to the first match; **Enter**
cycles through the rest.

## Modernized interface

Compared to the original SGI-green aesthetic, this version uses:

- a soft vertical gradient sky and a subtle reference grid on the floor,
- a cooler blue/amber palette for directories and files with highlight states,
- antialiased (MSAA) geometry,
- crisp screen-space labels instead of tilted 3D text, and
- a rounded, translucent **info card** for file attributes (size, permissions,
  owner/group, content type, effective access, and risk flags) in place of the
  old textured "scope" overlay.


## Build & run

Requires a Rust toolchain (stable). macroquad pulls in the platform's OpenGL.

```sh
cargo run --release            # visualize the current directory
cargo run --release -- /some/path
cargo run --release -- ~/Downloads --usage              # boot into disk-usage mode
cargo run --release -- /srv/data --agent claude-agent   # access as another user
cargo run --release -- --help                           # list flags and keys
```

Flags: `-u`/`--usage` launches straight into disk-usage mode (find what's
eating space); `-a`/`--agent NAME` also computes access as another user/agent
(toggle with `v`); `-h`/`--help` prints usage.

On Linux you may need the usual dev packages for X11/OpenGL/ALSA, e.g. on
Debian/Ubuntu:

```sh
sudo apt install libx11-dev libxi-dev libgl1-mesa-dev libasound2-dev
```

## Project layout

- `src/fstree.rs` — single-directory view model: scans one directory's
  immediate children, caps the visible set (~20) with "+N more" paging, tracks
  a breadcrumb for back-navigation, and lays the boxes out in a grid.
- `src/access.rs` — effective-access analysis for a viewpoint (you or a named
  agent), including the risk flags, via Unix mode bits + ownership.
- `src/filetype.rs` — content classification by magic bytes and entropy.
- `src/media.rs` — hover-to-play audio (looped, debounced, one at a time) and
  cached image thumbnails for the preview.
- `src/ops.rs` — copy / move / trash with cross-device fallback and Trash-only
  deletes.
- `src/effects.rs` — particle bursts and flying-box tweens for operations.
- `src/pick.rs` — ray/box intersection and mouse picking.
- `src/render.rs` — palette, box/label drawing, info card (with image preview),
  legend, toasts, search bar, and the confirmation dialog.
- `src/main.rs` — window setup, camera, input handling, directory navigation,
  the render loop, and the glue tying operations and media to the view.

## Safety notes

- Deletes use the [`trash`](https://crates.io/crates/trash) crate, so items go
  to the OS Trash / Recycle Bin and can be restored. There is no permanent
  delete in the UI.
- Directory deletes require an explicit on-screen confirmation.
- After any operation only the current directory is re-scanned, so it stays
  responsive regardless of how large the overall filesystem is.
- Expensive work runs off the render thread so the UI never freezes: directory
  sizing (disk-usage mode) and audio decoding both happen on background threads
  and stream results back. Image thumbnails are decoded once and cached.
- Unreadable folders (permission denied, removed) show a clear on-screen notice
  instead of a blank scene; individual unreadable entries are skipped rather
  than failing the whole listing.

## License

The original is GPL-3.0-or-later (Copyright © 2009 John Tsiombikas). This port
is distributed under the same terms.
