//! Material Design 3 color roles for the light and dark schemes, both built
//! around the #005FB8 brand primary. Read a role with its accessor, e.g. `ACCENT()`.
use eframe::egui::{self, Color32};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

static DARK: AtomicBool = AtomicBool::new(false);

pub fn is_dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

/// Theme setting. The default stays Light, independent of the OS preference.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeChoice {
    #[default]
    Light,
    Dark,
    System,
}

macro_rules! roles {
    ($($(#[$doc:meta])* $name:ident = $light:expr, $dark:expr;)*) => {$(
        $(#[$doc])*
        #[allow(non_snake_case)]
        pub fn $name() -> Color32 {
            if is_dark() { $dark } else { $light }
        }
    )*};
}
const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}
roles! {
    /// surface-container: navigation drawer and window background.
    SHELL = rgb(236, 238, 244), rgb(29, 32, 36);
    /// The reading pane (surface-container-lowest / surface).
    SURFACE = Color32::WHITE, rgb(17, 20, 24);
    /// surface-container-high: composer and filled text fields.
    RAISED = rgb(230, 232, 238), rgb(40, 42, 47);
    /// outline-variant: dividers and outlined cards.
    BORDER = rgb(195, 199, 207), rgb(67, 71, 78);
    /// on-surface.
    TEXT = rgb(25, 28, 32), rgb(225, 226, 233);
    /// on-surface-variant.
    MUTED = rgb(67, 71, 78), rgb(195, 198, 207);
    /// outline: unselected selection controls.
    OUTLINE = rgb(115, 119, 127), rgb(141, 145, 153);
    /// primary.
    ACCENT = rgb(0, 95, 184), rgb(164, 201, 255);
    /// on-primary: icons on primary buttons.
    ON_PRIMARY = Color32::WHITE, rgb(0, 49, 93);
    /// primary with an 8% on-primary hover state layer.
    ACCENT_HOVER = rgb(20, 108, 190), rgb(151, 189, 242);
    /// primary-container: user messages and the FAB.
    PRIMARY_CONTAINER = rgb(209, 228, 255), rgb(0, 71, 134);
    ON_PRIMARY_CONTAINER = rgb(0, 29, 54), rgb(212, 227, 255);
    /// secondary-container: navigation indicator, selected rows, tonal buttons.
    SELECTED = rgb(215, 227, 247), rgb(59, 72, 88);
    ON_SECONDARY_CONTAINER = rgb(16, 28, 43), rgb(215, 227, 248);
    /// Tonal button state layers (8% hover, 12% press).
    SELECTED_HOVER = rgb(199, 211, 231), rgb(72, 85, 101);
    SELECTED_PRESS = rgb(191, 203, 223), rgb(79, 92, 108);
    /// Filled field state layers.
    RAISED_HOVER = rgb(220, 222, 229), rgb(52, 54, 59);
    RAISED_PRESS = rgb(214, 217, 224), rgb(58, 60, 65);
    /// on-surface at 12% (container) and 38% (content) for disabled controls.
    DISABLED = rgb(225, 226, 230), rgb(46, 49, 53);
    DISABLED_CONTENT = rgb(160, 162, 166), rgb(117, 119, 123);
    /// error.
    ERROR = rgb(186, 26, 26), rgb(255, 180, 171);
    /// Skeleton placeholder blocks while startup data loads.
    SKELETON = rgb(218, 222, 230), rgb(44, 48, 54);
    /// Added and removed lines in diffs.
    DIFF_ADD = rgb(22, 113, 61), rgb(125, 214, 155);
    DIFF_DEL = rgb(180, 35, 24), rgb(255, 180, 171);
    /// Context-meter segments and chip dots.
    SKILLS = rgb(125, 82, 96), rgb(211, 155, 171);
    TOOLS = rgb(91, 95, 151), rgb(163, 167, 230);
    HISTORY = rgb(0, 95, 184), rgb(127, 182, 255);
    ATTACHMENTS = rgb(46, 125, 91), rgb(111, 199, 159);
    INPUT = rgb(178, 106, 0), rgb(240, 169, 74);
}

/// Whether `choice` resolves to dark for the current OS preference.
pub fn wants_dark(ctx: &egui::Context, choice: ThemeChoice) -> bool {
    match choice {
        ThemeChoice::Light => false,
        ThemeChoice::Dark => true,
        ThemeChoice::System => ctx.system_theme() == Some(egui::Theme::Dark),
    }
}

/// Apply the scheme to egui's visuals and the role accessors.
pub fn apply(ctx: &egui::Context, dark: bool) {
    DARK.store(dark, Ordering::Relaxed);
    let theme = if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    };
    ctx.set_theme(theme);
    let mut style = (*ctx.style_of(theme)).clone();
    style.visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let v = &mut style.visuals;
    // Per-role text colors instead of an override, so hints and disabled
    // text keep their weaker emphasis.
    v.override_text_color = None;
    v.panel_fill = SHELL();
    v.extreme_bg_color = RAISED();
    v.faint_bg_color = SHELL();
    v.code_bg_color = SHELL();
    v.window_fill = SURFACE();
    v.window_stroke = egui::Stroke::NONE;
    v.error_fg_color = ERROR();
    v.warn_fg_color = INPUT();
    v.hyperlink_color = ACCENT();
    v.selection.bg_fill = SELECTED();
    // Also the focused text-field indicator and selected-label text.
    v.selection.stroke = egui::Stroke::new(2.0_f32, ACCENT());
    // Menus and popups: elevation level 2, medium shape.
    v.menu_corner_radius = egui::CornerRadius::same(12);
    v.window_corner_radius = egui::CornerRadius::same(28);
    let elevation = egui::Shadow {
        offset: [0, 2],
        blur: 8,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 110 } else { 40 }),
    };
    v.popup_shadow = elevation;
    v.window_shadow = elevation;
    // Dividers use outline-variant.
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER());
    v.widgets.noninteractive.fg_stroke.color = TEXT();
    v.widgets.noninteractive.weak_bg_fill = SURFACE();
    v.widgets.noninteractive.bg_fill = SURFACE();
    // Filled tonal buttons (secondary container) with 8%/12% state layers.
    v.widgets.inactive.weak_bg_fill = SELECTED();
    v.widgets.inactive.bg_fill = RAISED();
    v.widgets.hovered.weak_bg_fill = SELECTED_HOVER();
    v.widgets.hovered.bg_fill = RAISED_HOVER();
    v.widgets.active.weak_bg_fill = SELECTED_PRESS();
    v.widgets.active.bg_fill = RAISED_PRESS();
    v.widgets.open.weak_bg_fill = SELECTED_PRESS();
    v.widgets.open.bg_fill = RAISED_PRESS();
    for widget in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        // 18 = half the 36px control height: pill buttons and single-line
        // fields, while multi-line fields keep moderately rounded corners.
        widget.corner_radius = egui::CornerRadius::same(18);
        widget.bg_stroke = egui::Stroke::NONE;
        widget.fg_stroke.color = ON_SECONDARY_CONTAINER();
        widget.expansion = 0.0;
    }
    style.spacing.button_padding = egui::vec2(16.0, 8.0);
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.interact_size.y = 36.0;
    style.spacing.icon_width = 18.0;
    style.spacing.icon_width_inner = 8.0;
    style.spacing.icon_spacing = 10.0;
    style.spacing.menu_margin = egui::Margin::same(8);
    // Material type scale: headline small, body, label large, label medium.
    for (text_style, font) in [
        (egui::TextStyle::Heading, egui::FontId::proportional(22.0)),
        (egui::TextStyle::Body, egui::FontId::proportional(15.0)),
        (egui::TextStyle::Button, egui::FontId::proportional(14.0)),
        (egui::TextStyle::Small, egui::FontId::proportional(12.0)),
        (egui::TextStyle::Monospace, egui::FontId::monospace(14.0)),
    ] {
        style.text_styles.insert(text_style, font);
    }
    ctx.set_style_of(theme, style);
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roles_switch_with_the_scheme_and_keep_text_contrast() {
        let ctx = egui::Context::default();
        for dark in [false, true] {
            apply(&ctx, dark);
            assert_eq!(is_dark(), dark);
            assert_eq!(ctx.style().visuals.dark_mode, dark);
            // Text on the pane and on the drawer keeps at least 7:1 contrast.
            for background in [SURFACE(), SHELL()] {
                assert!(contrast(TEXT(), background) >= 7.0, "dark={dark}");
                assert!(contrast(MUTED(), background) >= 4.5, "dark={dark}");
            }
            assert!(contrast(ON_PRIMARY(), ACCENT()) >= 4.5, "dark={dark}");
            assert!(contrast(ON_PRIMARY_CONTAINER(), PRIMARY_CONTAINER()) >= 7.0);
        }
        apply(&ctx, false);
    }
    fn contrast(a: Color32, b: Color32) -> f32 {
        let luminance = |c: Color32| {
            let channel = |v: u8| {
                let v = v as f32 / 255.0;
                if v <= 0.03928 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
        };
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }
}
