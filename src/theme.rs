use egui::{Color32, Stroke, Visuals};

/// All colours used by the application in one place.
/// To create a new colour scheme, add a constructor like `fn teenage_engineering() -> Self`
/// and change the call in `App::new`.
pub struct Theme {
    // ── Base surfaces ────────────────────────────────────────────────────────
    pub bg: Color32,       // panel / window background
    pub surface: Color32,  // widget background (one step lighter than bg)
    pub border: Color32,   // widget / panel borders

    // ── Text ─────────────────────────────────────────────────────────────────
    pub text: Color32,
    pub text_dim: Color32, // disabled / placeholder text

    // ── Accent ───────────────────────────────────────────────────────────────
    pub accent: Color32,      // solid accent (waveform, hover, active borders)
    pub accent_dim: Color32,  // translucent accent (selection bg)

    // ── Waveform-specific ────────────────────────────────────────────────────
    pub waveform: Color32,   // peak lines
    pub zero_line: Color32,  // centre zero line
    pub loop_fill: Color32,  // committed A–B region fill
    pub drag_fill: Color32,  // in-progress drag selection fill
}

impl Theme {
    /// Teenage Engineering-inspired: near-black base, TE orange accent.
    pub fn teenage_engineering() -> Self {
        let accent = Color32::from_rgb(255, 102, 0);
        Self {
            bg:         Color32::from_rgb(10, 10, 10),
            surface:    Color32::from_rgb(20, 20, 20),
            border:     Color32::from_rgb(42, 42, 42),
            text:       Color32::from_rgb(224, 224, 224),
            text_dim:   Color32::from_rgb(64, 64, 64),
            accent,
            accent_dim: Color32::from_rgba_unmultiplied(255, 102, 0, 30),
            waveform:   accent,
            zero_line:  Color32::from_rgba_unmultiplied(255, 255, 255, 12),
            loop_fill:  Color32::from_rgba_unmultiplied(255, 102, 0, 28),
            drag_fill:  Color32::from_rgba_unmultiplied(255, 153, 0, 45),
        }
    }

    /// Build and apply egui `Visuals` from this theme.
    pub fn apply_visuals(&self, ctx: &egui::Context) {
        let mut v = Visuals::dark();
        let t = self;

        v.panel_fill            = t.bg;
        v.window_fill           = t.bg;
        v.faint_bg_color        = t.surface;
        v.extreme_bg_color      = Color32::from_rgb(6, 6, 6);
        v.override_text_color   = Some(t.text);
        v.window_stroke         = Stroke::new(1.0, t.border);
        v.slider_trailing_fill  = true;

        v.widgets.noninteractive.bg_fill   = t.surface;
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, t.border);
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, t.text_dim);
        v.widgets.inactive.bg_fill         = t.surface;
        v.widgets.inactive.bg_stroke       = Stroke::new(1.0, t.border);
        v.widgets.inactive.fg_stroke       = Stroke::new(1.0, t.text);
        v.widgets.hovered.bg_fill          = Color32::from_rgb(30, 30, 30);
        v.widgets.hovered.bg_stroke        = Stroke::new(1.0, t.accent);
        v.widgets.hovered.fg_stroke        = Stroke::new(1.0, t.accent);
        v.widgets.active.bg_fill           = Color32::from_rgb(38, 18, 4);
        v.widgets.active.bg_stroke         = Stroke::new(1.0, t.accent);
        v.widgets.active.fg_stroke         = Stroke::new(1.0, t.accent);
        v.widgets.open.bg_fill             = t.surface;
        v.selection.bg_fill                = t.accent_dim;
        v.selection.stroke                 = Stroke::new(1.0, t.accent);

        ctx.set_visuals(v);
    }
}
