//! Visual trim timeline: a strip of thumbnail frames with two draggable
//! handles. Frames are extracted with the bundled ffmpeg on a background
//! thread (one process per frame, so the strip fills in progressively) and
//! cached per input file as egui textures. The geometry is pure functions so
//! it can be unit-tested without a UI.

use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{Receiver, Sender};

pub const THUMB_COUNT: usize = 10;
const THUMB_W: usize = 160;
const THUMB_H: usize = 90;
/// Handles can't be dragged closer than this (seconds).
const MIN_SELECTION: f64 = 0.1;

/// Times (seconds) to grab frames at: the midpoint of each of `n` equal
/// segments, so the first and last frames aren't black fade-in/out frames.
pub fn thumbnail_times(duration: f64, n: usize) -> Vec<f64> {
    if duration <= 0.0 || n == 0 {
        return Vec::new();
    }
    (0..n)
        .map(|i| duration * (i as f64 + 0.5) / n as f64)
        .collect()
}

/// Map an x position inside `[left, left + width]` to a time.
pub fn x_to_time(x: f32, left: f32, width: f32, duration: f64) -> f64 {
    if width <= 0.0 {
        return 0.0;
    }
    (((x - left) / width).clamp(0.0, 1.0) as f64) * duration
}

pub fn time_to_x(t: f64, left: f32, width: f32, duration: f64) -> f32 {
    if duration <= 0.0 {
        return left;
    }
    left + ((t / duration).clamp(0.0, 1.0) as f32) * width
}

/// Keep `start < end` within `[0, duration]` after one handle moved.
/// `moved_start` says which handle the user dragged; the other stays put.
pub fn clamp_range(start: f64, end: f64, duration: f64, moved_start: bool) -> (f64, f64) {
    let gap = MIN_SELECTION.min(duration);
    if moved_start {
        (start.clamp(0.0, (end - gap).max(0.0)), end)
    } else {
        (start, end.clamp((start + gap).min(duration), duration))
    }
}

struct Thumb {
    path: PathBuf,
    index: usize,
    rgba: Vec<u8>,
}

#[derive(Default)]
struct Entry {
    textures: Vec<Option<egui::TextureHandle>>,
}

pub struct ThumbCache {
    entries: HashMap<PathBuf, Entry>,
    sender: Sender<Thumb>,
    receiver: Receiver<Thumb>,
}

impl ThumbCache {
    pub fn new() -> Self {
        let (sender, receiver) = std::sync::mpsc::channel();
        Self {
            entries: HashMap::new(),
            sender,
            receiver,
        }
    }

    /// Start extraction for `path` the first time it's seen.
    pub fn request(&mut self, ffmpeg: &Path, path: &Path, duration: f64) {
        if self.entries.contains_key(path) {
            return;
        }
        self.entries.insert(
            path.to_path_buf(),
            Entry {
                textures: vec![None; THUMB_COUNT],
            },
        );
        let sender = self.sender.clone();
        let ffmpeg = ffmpeg.to_path_buf();
        let path = path.to_path_buf();
        std::thread::spawn(move || {
            for (index, t) in thumbnail_times(duration, THUMB_COUNT)
                .into_iter()
                .enumerate()
            {
                if let Some(rgba) = extract_frame(&ffmpeg, &path, t) {
                    if sender
                        .send(Thumb {
                            path: path.clone(),
                            index,
                            rgba,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
        });
    }

    /// Turn finished frames into textures. Returns true if any arrived (the
    /// caller repaints so the strip fills in).
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut got = false;
        for thumb in self.receiver.try_iter() {
            let Some(entry) = self.entries.get_mut(&thumb.path) else {
                continue;
            };
            let image = egui::ColorImage::from_rgba_unmultiplied([THUMB_W, THUMB_H], &thumb.rgba);
            let name = format!("thumb:{}:{}", thumb.path.display(), thumb.index);
            if let Some(slot) = entry.textures.get_mut(thumb.index) {
                *slot = Some(ctx.load_texture(name, image, egui::TextureOptions::LINEAR));
                got = true;
            }
        }
        got
    }

    fn textures(&self, path: &Path) -> Option<&[Option<egui::TextureHandle>]> {
        self.entries.get(path).map(|e| e.textures.as_slice())
    }
}

/// One frame at `t` seconds as raw RGBA, letterboxed to a fixed size so no
/// image decoder is needed. `None` if ffmpeg fails or returns a short read.
fn extract_frame(ffmpeg: &Path, input: &Path, t: f64) -> Option<Vec<u8>> {
    let mut cmd = Command::new(ffmpeg);
    cmd.args(["-v", "error", "-ss", &format!("{t:.3}"), "-i"])
        .arg(input)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!(
                "scale={THUMB_W}:{THUMB_H}:force_original_aspect_ratio=decrease,\
                 pad={THUMB_W}:{THUMB_H}:(ow-iw)/2:(oh-ih)/2,format=rgba"
            ),
            "-f",
            "rawvideo",
            "-",
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    mediakit_core::sys::no_console_window(&mut cmd);
    let output = cmd.output().ok()?;
    (output.status.success() && output.stdout.len() == THUMB_W * THUMB_H * 4)
        .then_some(output.stdout)
}

/// Draw the strip and handles. `start`/`end` are the current selection in
/// seconds; returns the new values if the user dragged a handle.
pub fn show(
    ui: &mut egui::Ui,
    cache: &ThumbCache,
    path: &Path,
    duration: f64,
    start: f64,
    end: f64,
) -> Option<(f64, f64)> {
    let size = egui::vec2(ui.available_width().max(200.0), 56.0);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);

    let textures = cache.textures(path);
    let cell_w = rect.width() / THUMB_COUNT as f32;
    for i in 0..THUMB_COUNT {
        let cell = egui::Rect::from_min_size(
            rect.min + egui::vec2(cell_w * i as f32, 0.0),
            egui::vec2(cell_w, rect.height()),
        );
        if let Some(Some(tex)) = textures.map(|t| t.get(i)).and_then(|o| o) {
            painter.image(
                tex.id(),
                cell,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
        }
    }

    let x_start = time_to_x(start, rect.left(), rect.width(), duration);
    let x_end = time_to_x(end, rect.left(), rect.width(), duration);
    let dim = egui::Color32::from_black_alpha(150);
    painter.rect_filled(
        egui::Rect::from_min_max(rect.min, egui::pos2(x_start, rect.bottom())),
        0.0,
        dim,
    );
    painter.rect_filled(
        egui::Rect::from_min_max(egui::pos2(x_end, rect.top()), rect.max),
        0.0,
        dim,
    );
    let accent = ui.visuals().selection.bg_fill;
    painter.rect_stroke(
        egui::Rect::from_min_max(
            egui::pos2(x_start, rect.top()),
            egui::pos2(x_end, rect.bottom()),
        ),
        0.0,
        egui::Stroke::new(2.0, accent),
        egui::StrokeKind::Inside,
    );

    let mut result = None;
    for (is_start, x) in [(true, x_start), (false, x_end)] {
        let handle = egui::Rect::from_center_size(
            egui::pos2(x, rect.center().y),
            egui::vec2(12.0, rect.height()),
        );
        let id = ui.id().with(("trim_handle", is_start));
        let response = ui
            .interact(handle, id, egui::Sense::drag())
            .on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        painter.rect_filled(handle.shrink2(egui::vec2(3.0, 0.0)), 2.0, accent);
        if let Some(pos) = response.interact_pointer_pos() {
            if response.dragged() {
                let t = x_to_time(pos.x, rect.left(), rect.width(), duration);
                let (s, e) = if is_start {
                    clamp_range(t, end, duration, true)
                } else {
                    clamp_range(start, t, duration, false)
                };
                result = Some((s, e));
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_times_are_segment_midpoints() {
        let t = thumbnail_times(100.0, 10);
        assert_eq!(t.len(), 10);
        assert!((t[0] - 5.0).abs() < 1e-9);
        assert!((t[9] - 95.0).abs() < 1e-9);
        assert!(thumbnail_times(0.0, 10).is_empty());
    }

    #[test]
    fn x_and_time_round_trip_and_clamp() {
        assert_eq!(x_to_time(150.0, 100.0, 200.0, 60.0), 15.0);
        assert_eq!(x_to_time(0.0, 100.0, 200.0, 60.0), 0.0);
        assert_eq!(x_to_time(999.0, 100.0, 200.0, 60.0), 60.0);
        assert_eq!(time_to_x(15.0, 100.0, 200.0, 60.0), 150.0);
        assert_eq!(time_to_x(5.0, 100.0, 200.0, 0.0), 100.0);
    }

    #[test]
    fn handles_cannot_cross_or_leave_the_clip() {
        // Dragging start past end stops just short of it.
        let (s, e) = clamp_range(50.0, 20.0, 60.0, true);
        assert!(s < e && (e - s - MIN_SELECTION).abs() < 1e-9);
        // Dragging end before start stops just after it.
        let (s, e) = clamp_range(20.0, 5.0, 60.0, false);
        assert!(e > s && (e - s - MIN_SELECTION).abs() < 1e-9);
        // Beyond the clip is clamped.
        assert_eq!(clamp_range(10.0, 99.0, 60.0, false), (10.0, 60.0));
        assert_eq!(clamp_range(-5.0, 30.0, 60.0, true), (0.0, 30.0));
    }
}
