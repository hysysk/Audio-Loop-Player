use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use egui::{Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use crate::audio::{
    compute_waveform_peaks, decode_audio, AudioData, AudioEngine, Peak,
};

const NUM_PEAKS: usize = 2000;
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "wav", "ogg", "flac", "m4a", "aac", "opus", "wma",
];

// ─── File entry ──────────────────────────────────────────────────────────────

#[derive(Clone)]
struct FileEntry {
    path: PathBuf,
    name: String,
    is_dir: bool,
}

// ─── App ─────────────────────────────────────────────────────────────────────

pub struct App {
    audio: AudioEngine,

    // File browser
    current_dir: PathBuf,
    dir_entries: Vec<FileEntry>,
    selected_file: Option<PathBuf>,

    // Loaded audio
    loaded_file: Option<PathBuf>,
    waveform_peaks: Vec<Peak>,
    audio_duration: f64,

    // Background decoding thread
    loading_rx: Option<Receiver<Result<AudioData, String>>>,
    is_loading: bool,

    // Waveform drag state
    drag_start: Option<f32>,   // normalized 0..1
    drag_current: Option<f32>, // normalized 0..1

    // Loop region
    loop_enabled: bool,
    loop_start_secs: f64,
    loop_end_secs: f64,

    // Controls (kept here so sliders have persistent state)
    volume: f32,
    speed: f32,

    status: String,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_cjk_font(&cc.egui_ctx);

        let audio = AudioEngine::new().expect("Failed to initialize audio engine");

        let current_dir = dirs_for_start();
        let dir_entries = scan_directory(&current_dir);

        Self {
            audio,
            current_dir,
            dir_entries,
            selected_file: None,
            loaded_file: None,
            waveform_peaks: Vec::new(),
            audio_duration: 0.0,
            loading_rx: None,
            is_loading: false,
            drag_start: None,
            drag_current: None,
            loop_enabled: false,
            loop_start_secs: 0.0,
            loop_end_secs: 0.0,
            volume: 1.0,
            speed: 1.0,
            status: "Open an audio file to begin.".into(),
        }
    }

    // ─── File loading ─────────────────────────────────────────────────────

    fn start_loading(&mut self, path: PathBuf) {
        if self.is_loading {
            return;
        }
        self.is_loading = true;
        self.audio.stop();
        self.waveform_peaks.clear();
        self.audio_duration = 0.0;
        let fname = display_name(&path);
        self.status = format!("Loading {}…", fname);

        let (tx, rx) = mpsc::channel();
        let path_clone = path.clone();
        std::thread::spawn(move || {
            let result = decode_audio(&path_clone).map_err(|e| e.to_string());
            let _ = tx.send(result);
        });

        self.loading_rx = Some(rx);
        self.loaded_file = Some(path);
    }

    fn poll_loading(&mut self) {
        if !self.is_loading {
            return;
        }
        let result = self.loading_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(result) = result {
            self.loading_rx = None;
            self.is_loading = false;
            match result {
                Ok(data) => {
                    self.waveform_peaks =
                        compute_waveform_peaks(&data.samples, data.channels, NUM_PEAKS);
                    self.audio_duration = data.duration_secs;
                    self.loop_enabled = false;
                    self.loop_start_secs = 0.0;
                    self.loop_end_secs = data.duration_secs;
                    self.audio.load(&data);
                    self.audio.set_volume(self.volume);
                    self.audio.set_speed(self.speed);
                    self.status = format!(
                        "{}  •  {}  •  {} Hz  •  {} ch",
                        display_name(self.loaded_file.as_ref().unwrap()),
                        format_duration(data.duration_secs),
                        data.sample_rate,
                        data.channels,
                    );
                    self.audio.play();
                }
                Err(e) => {
                    self.status = format!("Error: {}", e);
                    self.loaded_file = None;
                }
            }
        }
    }

    // ─── File browser ─────────────────────────────────────────────────────

    fn draw_file_browser(&mut self, ui: &mut Ui) {
        ui.add_space(4.0);

        // Current directory label (truncated to last 2 parts)
        let dir_label = short_path(&self.current_dir);
        ui.small(dir_label.as_str());
        ui.separator();

        egui::ScrollArea::vertical()
            .id_source("filebrowser")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for entry in self.dir_entries.clone() {
                    let is_selected = self.selected_file.as_deref() == Some(entry.path.as_path());

                    let icon = if entry.is_dir { "📁" } else { "🎵" };
                    let label = format!("{} {}", icon, entry.name);

                    let resp = ui.selectable_label(is_selected, &label);
                    if resp.clicked() {
                        if entry.is_dir {
                            self.current_dir = entry.path.clone();
                            self.dir_entries = scan_directory(&self.current_dir);
                            self.selected_file = None;
                        } else {
                            self.selected_file = Some(entry.path.clone());
                            self.start_loading(entry.path.clone());
                        }
                    }
                }
            });
    }

    // ─── Waveform ─────────────────────────────────────────────────────────

    fn draw_waveform(&mut self, ui: &mut Ui) {
        let height = (ui.available_height() - 160.0).max(120.0);
        let desired_size = Vec2::new(ui.available_width(), height);
        let (response, painter) = ui.allocate_painter(desired_size, Sense::click_and_drag());
        let rect = response.rect;

        // Background
        painter.rect_filled(
            rect,
            4.0,
            Color32::from_rgb(20, 22, 35),
        );

        let duration = self.audio_duration;

        if duration <= 0.0 {
            // Placeholder text
            let msg = if self.is_loading { "Loading…" } else { "No file loaded" };
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                msg,
                FontId::proportional(15.0),
                Color32::from_rgb(90, 95, 115),
            );
            painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_rgb(45, 50, 65)));
            return;
        }

        // Loop region highlight + markers
        let (loop_s, loop_e) = (self.loop_start_secs, self.loop_end_secs);
        if self.loop_enabled && loop_e > loop_s {
            let x0 = rect.left() + (loop_s / duration) as f32 * rect.width();
            let x1 = rect.left() + (loop_e / duration) as f32 * rect.width();
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, rect.top()), Pos2::new(x1, rect.bottom())),
                0.0,
                Color32::from_rgba_unmultiplied(80, 140, 255, 45),
            );
            let marker_stroke = Stroke::new(2.0, Color32::from_rgb(100, 160, 255));
            painter.line_segment([Pos2::new(x0, rect.top()), Pos2::new(x0, rect.bottom())], marker_stroke);
            painter.line_segment([Pos2::new(x1, rect.top()), Pos2::new(x1, rect.bottom())], marker_stroke);
        }

        // Active drag selection preview
        if let (Some(ds), Some(dc)) = (self.drag_start, self.drag_current) {
            let x0 = rect.left() + ds.min(dc) * rect.width();
            let x1 = rect.left() + ds.max(dc) * rect.width();
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, rect.top()), Pos2::new(x1, rect.bottom())),
                0.0,
                Color32::from_rgba_unmultiplied(160, 215, 160, 60),
            );
        }

        // Waveform peaks — one column per display pixel
        if !self.waveform_peaks.is_empty() {
            let n = self.waveform_peaks.len();
            let px_count = (rect.width() as usize).max(1);
            let mid_y = rect.center().y;
            let half_h = rect.height() * 0.44;

            for px in 0..px_count {
                let norm = px as f32 / px_count as f32;
                let idx = ((norm * n as f32) as usize).min(n - 1);
                let peak = self.waveform_peaks[idx];

                let y_top = (mid_y - peak.max.clamp(0.0, 1.0) * half_h).max(rect.top());
                let y_bot = (mid_y + (-peak.min).clamp(0.0, 1.0) * half_h).min(rect.bottom());

                let x = rect.left() + px as f32 + 0.5;
                painter.line_segment(
                    [Pos2::new(x, y_top), Pos2::new(x, y_bot)],
                    Stroke::new(1.0, Color32::from_rgb(80, 190, 120)),
                );
            }

            // Zero-line
            painter.line_segment(
                [Pos2::new(rect.left(), mid_y), Pos2::new(rect.right(), mid_y)],
                Stroke::new(1.0, Color32::from_rgba_unmultiplied(255, 255, 255, 15)),
            );
        }

        // Playhead
        let pos_secs = self.audio.state().position_secs;
        if duration > 0.0 {
            let norm = (pos_secs / duration).clamp(0.0, 1.0) as f32;
            let px = rect.left() + norm * rect.width();
            painter.line_segment(
                [Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())],
                Stroke::new(2.0, Color32::WHITE),
            );
        }

        // Border
        painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_rgb(50, 55, 70)));

        // ─── Mouse interaction ────────────────────────────────────────────

        if response.drag_started() {
            if let Some(p) = response.interact_pointer_pos() {
                let xf = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                self.drag_start = Some(xf);
                self.drag_current = Some(xf);
            }
        }

        if response.dragged() {
            if let Some(p) = response.interact_pointer_pos() {
                let xf = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                self.drag_current = Some(xf);
            }
        }

        if response.drag_stopped() {
            if let (Some(ds), Some(dc)) = (self.drag_start, self.drag_current) {
                let s = ds.min(dc) as f64 * duration;
                let e = ds.max(dc) as f64 * duration;
                if e - s > 0.05 {
                    // Commit loop region and seek to A point
                    self.loop_start_secs = s;
                    self.loop_end_secs = e;
                    self.loop_enabled = true;
                    self.audio.set_loop(true, s, e);
                    self.audio.seek(s);
                }
            }
            self.drag_start = None;
            self.drag_current = None;
        }

        // Click (without drag) → seek
        // If the target is outside the active loop region, disable the loop
        // (the region is preserved so the Loop button can re-enable it).
        if response.clicked() {
            if let Some(p) = response.interact_pointer_pos() {
                let xf = ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
                let seek_secs = xf as f64 * duration;
                if self.loop_enabled
                    && (seek_secs < self.loop_start_secs || seek_secs >= self.loop_end_secs)
                {
                    self.loop_enabled = false;
                    self.audio.set_loop(false, self.loop_start_secs, self.loop_end_secs);
                }
                self.audio.seek(seek_secs);
            }
        }

        // Right-click → clear loop
        if response.secondary_clicked() {
            self.loop_enabled = false;
            self.audio.set_loop(false, 0.0, duration);
        }
    }

    // ─── Controls ─────────────────────────────────────────────────────────

    fn draw_controls(&mut self, ui: &mut Ui) {
        let es = self.audio.state();
        let duration = self.audio_duration;
        let has_file = duration > 0.0 && !self.is_loading;

        ui.add_space(6.0);

        // Time row
        ui.horizontal(|ui| {
            ui.monospace(format_duration(es.position_secs));
            ui.label("/");
            ui.monospace(format_duration(duration));

            if self.loop_enabled {
                ui.separator();
                ui.label("🔁");
                ui.monospace(format!(
                    "{}  –  {}",
                    format_duration(self.loop_start_secs),
                    format_duration(self.loop_end_secs)
                ));
            }
        });

        ui.add_space(4.0);

        // Transport row
        ui.horizontal(|ui| {
            let play_icon = if es.is_playing { "⏸  Pause" } else { "▶  Play" };
            if ui
                .add_enabled(
                    has_file,
                    egui::Button::new(play_icon).min_size(Vec2::new(88.0, 28.0)),
                )
                .clicked()
            {
                if es.is_playing {
                    self.audio.pause();
                } else {
                    self.audio.play();
                }
            }

            if ui
                .add_enabled(
                    has_file,
                    egui::Button::new("⏹  Stop").min_size(Vec2::new(78.0, 28.0)),
                )
                .clicked()
            {
                self.audio.stop();
            }

            ui.add_space(8.0);

            let loop_label = if self.loop_enabled {
                "🔁  Loop  ✓"
            } else {
                "🔁  Loop"
            };
            if ui
                .add_enabled(
                    has_file,
                    egui::Button::new(loop_label).min_size(Vec2::new(96.0, 28.0)),
                )
                .on_hover_text("Drag waveform to select loop region\nRight-click waveform to clear")
                .clicked()
            {
                self.loop_enabled = !self.loop_enabled;
                self.audio.set_loop(self.loop_enabled, self.loop_start_secs, self.loop_end_secs);
            }
        });

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        // Volume + speed sliders
        egui::Grid::new("sliders")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Volume");
                if ui
                    .add(
                        egui::Slider::new(&mut self.volume, 0.0..=2.0)
                            .show_value(true)
                            .fixed_decimals(2),
                    )
                    .changed()
                {
                    self.audio.set_volume(self.volume);
                }
                ui.end_row();

                ui.label("Speed").on_hover_text("Changes pitch. Best for quick navigation.");
                if ui
                    .add(
                        egui::Slider::new(&mut self.speed, 0.25..=2.0)
                            .show_value(true)
                            .fixed_decimals(2)
                            .suffix("×"),
                    )
                    .changed()
                {
                    self.audio.set_speed(self.speed);
                }
                ui.end_row();
            });

        ui.add_space(4.0);
        ui.separator();

        // Status bar
        ui.add_space(2.0);
        ui.small(&self.status);
    }
}

// ─── eframe::App ─────────────────────────────────────────────────────────────

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_loading();

        // Request continuous repaints while playing or loading
        let is_playing = self.audio.atomics.playing.load(Ordering::Relaxed);
        if is_playing || self.is_loading {
            ctx.request_repaint_after(Duration::from_millis(33)); // ~30 fps
        }

        // Apply a dark visuals theme
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(18, 20, 30);
        visuals.window_fill = Color32::from_rgb(18, 20, 30);
        ctx.set_visuals(visuals);

        // ── Left panel: file browser ──────────────────────────────────────
        egui::SidePanel::left("file_browser")
            .resizable(true)
            .default_width(220.0)
            .min_width(150.0)
            .show(ctx, |ui| {
                self.draw_file_browser(ui);
            });

        // ── Central panel: waveform + controls ────────────────────────────
        egui::CentralPanel::default().show(ctx, |ui| {
            // File name header
            let file_name = self
                .loaded_file
                .as_ref()
                .map(|p| display_name(p))
                .unwrap_or_default();

            if !file_name.is_empty() {
                ui.heading(&file_name);
                ui.separator();
                ui.add_space(2.0);
            }

            self.draw_waveform(ui);
            self.draw_controls(ui);
        });
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn scan_directory(path: &std::path::Path) -> Vec<FileEntry> {
    let mut result = Vec::new();

    // ".." entry
    if let Some(parent) = path.parent() {
        result.push(FileEntry {
            path: parent.to_path_buf(),
            name: "..".into(),
            is_dir: true,
        });
    }

    let mut dirs = Vec::new();
    let mut files = Vec::new();

    if let Ok(iter) = std::fs::read_dir(path) {
        for entry in iter.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue; // skip hidden
            }
            let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                dirs.push(FileEntry { path: entry.path(), name, is_dir: true });
            } else {
                let ext = entry
                    .path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| e.to_lowercase())
                    .unwrap_or_default();
                if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
                    files.push(FileEntry { path: entry.path(), name, is_dir: false });
                }
            }
        }
    }

    dirs.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    files.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    result.extend(dirs);
    result.extend(files);
    result
}

fn format_duration(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0:00.0".into();
    }
    let total_s = secs as u64;
    let m = total_s / 60;
    let s = total_s % 60;
    let ds = ((secs % 1.0) * 10.0) as u32;
    format!("{}:{:02}.{}", m, s, ds)
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn short_path(path: &std::path::Path) -> String {
    let parts: Vec<_> = path.iter().collect();
    match parts.len() {
        0 => "/".into(),
        1 => parts[0].to_string_lossy().to_string(),
        n => format!(
            "…/{}/{}",
            parts[n - 2].to_string_lossy(),
            parts[n - 1].to_string_lossy()
        ),
    }
}

fn dirs_for_start() -> PathBuf {
    // Try ~/Music, then home, then current dir
    if let Ok(home) = std::env::var("HOME") {
        let music = PathBuf::from(&home).join("Music");
        if music.is_dir() {
            return music;
        }
        return PathBuf::from(home);
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Load a CJK-capable system font and register it as a fallback in egui.
/// Without this, Japanese filenames (and any CJK text) render as □ boxes.
fn setup_cjk_font(ctx: &egui::Context) {
    // Candidate paths: macOS ships Hiragino Sans which covers Japanese fully.
    // Try multiple spellings across macOS versions.
    let candidates: &[(&str, u32)] = &[
        // macOS 10.13+  (Japanese filename, UTF-8 path)
        ("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
        ("/System/Library/Fonts/ヒラギノ角ゴ ProN W3.ttc", 0),
        // ASCII aliases that sometimes exist
        ("/System/Library/Fonts/Hiragino Sans W3.ttc", 0),
        ("/System/Library/Fonts/HiraginoSans-W3.ttc", 0),
        // PingFang covers CJK (less ideal for Japanese but works)
        ("/System/Library/Fonts/PingFang.ttc", 0),
        // Broad fallback
        ("/System/Library/Fonts/Supplemental/Arial Unicode MS.ttf", 0),
    ];

    let mut fonts = egui::FontDefinitions::default();

    for (path, index) in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut data = egui::FontData::from_owned(bytes);
            data.index = *index;
            fonts.font_data.insert("cjk".to_owned(), data);

            // Add after the built-in fonts so ASCII still uses the default,
            // and CJK glyphs fall through to this font.
            for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                fonts.families.entry(family).or_default().push("cjk".to_owned());
            }
            break;
        }
    }

    ctx.set_fonts(fonts);
}
