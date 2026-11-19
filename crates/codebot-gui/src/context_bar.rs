//! Context usage: a segmented bar showing what the next request is made of,
//! measured in characters against the configured `max_context_chars` budget.
use eframe::egui::{self, Color32, RichText};

/// Segment colors: Material tonal accents that stay distinguishable on white.
pub const SKILLS: Color32 = Color32::from_rgb(125, 82, 96);
pub const TOOLS: Color32 = Color32::from_rgb(91, 95, 151);
pub const HISTORY: Color32 = Color32::from_rgb(0, 95, 184);
pub const ATTACHMENTS: Color32 = Color32::from_rgb(46, 125, 91);
pub const INPUT: Color32 = Color32::from_rgb(178, 106, 0);

pub struct Segment {
    pub label: &'static str,
    pub chars: usize,
    pub color: Color32,
    /// Shown in the legend, e.g. why a segment is not sent.
    pub note: Option<String>,
}
impl Segment {
    pub fn new(label: &'static str, chars: usize, color: Color32) -> Self {
        Self {
            label,
            chars,
            color,
            note: None,
        }
    }
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }
}

fn used(segments: &[Segment]) -> usize {
    segments
        .iter()
        .filter(|s| s.note.is_none())
        .map(|s| s.chars)
        .sum()
}

/// Human-readable character count: 950, 8.2k, 64k.
pub fn short(chars: usize) -> String {
    if chars < 1000 {
        chars.to_string()
    } else if chars < 10_000 {
        format!("{:.1}k", chars as f32 / 1000.0)
    } else {
        format!("{}k", (chars + 500) / 1000)
    }
}

fn paint_bar(
    ui: &mut egui::Ui,
    segments: &[Segment],
    budget: usize,
    size: egui::Vec2,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::hover());
    let painter = ui.painter();
    let radius = size.y / 2.0;
    painter.rect_filled(rect, radius, crate::RAISED);
    let budget = budget.max(1) as f32;
    let mut x = rect.left();
    let clip = painter.with_clip_rect(rect);
    // Paint each segment as a rounded bar starting at the left edge, longest
    // first, so the pill shape is preserved without gaps between segments.
    let mut ends = Vec::new();
    for segment in segments.iter().filter(|s| s.note.is_none() && s.chars > 0) {
        x += (segment.chars as f32 / budget) * rect.width();
        ends.push((x.min(rect.right()), segment.color));
    }
    for (end, color) in ends.into_iter().rev() {
        let bar = egui::Rect::from_min_max(
            rect.min,
            egui::pos2(end.max(rect.left() + size.y), rect.bottom()),
        );
        clip.rect_filled(bar, radius, color);
    }
    response
}

/// Detailed view: wide bar and a legend with each category's share.
pub fn detail(ui: &mut egui::Ui, title: &str, segments: &[Segment], budget: usize) {
    let total = used(segments);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(title)
                .size(14.0)
                .family(egui::FontFamily::Name("ui-bold".into())),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!(
                    "{} / {}文字（{}）",
                    short(total),
                    short(budget),
                    percent(total, budget)
                ))
                .size(12.0)
                .color(crate::MUTED),
            );
        });
    });
    let width = ui.available_width();
    paint_bar(ui, segments, budget, egui::vec2(width, 10.0));
    ui.add_space(4.0);
    egui::Grid::new(ui.next_auto_id())
        .num_columns(3)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for segment in segments {
                ui.horizontal(|ui| {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                    let color = if segment.note.is_some() {
                        crate::DISABLED_CONTENT
                    } else {
                        segment.color
                    };
                    ui.painter().circle_filled(dot.center(), 5.0, color);
                    ui.label(RichText::new(segment.label).size(13.0));
                });
                ui.label(
                    RichText::new(format!("{}文字", short(segment.chars)))
                        .size(13.0)
                        .color(crate::MUTED),
                );
                ui.label(
                    RichText::new(match &segment.note {
                        Some(note) => note.clone(),
                        None => format!(
                            "{:.1}%",
                            segment.chars as f32 * 100.0 / budget.max(1) as f32
                        ),
                    })
                    .size(12.0)
                    .color(crate::MUTED),
                );
                ui.end_row();
            }
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), egui::Sense::hover());
                ui.painter().circle_stroke(
                    dot.center(),
                    4.5,
                    egui::Stroke::new(1.0_f32, crate::OUTLINE),
                );
                ui.label(RichText::new("空き").size(13.0));
            });
            ui.label(
                RichText::new(format!("{}文字", short(budget.saturating_sub(total))))
                    .size(13.0)
                    .color(crate::MUTED),
            );
            ui.label("");
            ui.end_row();
        });
    ui.label(
        RichText::new("文字数はアプリの送信上限（設定ファイルのmax_context_chars）に対する目安です。モデルのトークン数とは一致しません。")
            .size(11.0)
            .color(crate::MUTED),
    );
}

fn percent(total: usize, budget: usize) -> String {
    match total * 100 / budget.max(1) {
        0 if total > 0 => "<1%".into(),
        value => format!("{}%", value.min(999)),
    }
}

/// Compact meter for the composer footer; hovering shows [`detail`].
pub fn compact(ui: &mut egui::Ui, segments: &[Segment], budget: usize) -> egui::Response {
    let total = used(segments);
    let response = ui
        .horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            let over = total > budget;
            ui.label(
                RichText::new(percent(total, budget))
                    .size(11.0)
                    .color(if over { crate::ERROR } else { crate::MUTED }),
            );
            paint_bar(ui, segments, budget, egui::vec2(72.0, 6.0));
        })
        .response
        .interact(egui::Sense::hover());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(
            egui::WidgetType::ProgressIndicator,
            true,
            format!("コンテキスト使用量 {}/{}文字", total, budget),
        )
    });
    response.on_hover_ui(|ui| {
        ui.set_width(320.0);
        detail(ui, "コンテキストの内訳", segments, budget);
    })
}
