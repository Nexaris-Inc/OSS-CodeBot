use eframe::egui::{self, Response, Stroke, Ui, Widget, WidgetText};

/// Keep egui's checkbox interaction and accessibility, drawn as a Material ring with
/// a filled primary dot when selected.
pub struct Checkbox<'a> {
    checked: &'a mut bool,
    text: WidgetText,
}

impl<'a> Checkbox<'a> {
    pub fn new(checked: &'a mut bool, text: impl Into<WidgetText>) -> Self {
        Self {
            checked,
            text: text.into(),
        }
    }
}

impl Widget for Checkbox<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let Self { checked, text } = self;
        let text = text.color(ui.visuals().text_color());
        let response =
            without_native_indicator(ui, |ui| ui.add(egui::Checkbox::new(&mut *checked, text)));
        paint_indicator(ui, &response, *checked);
        response
    }
}

/// egui's radio button semantics with the same Material indicator as [`Checkbox`].
pub struct Radio<'a, T> {
    current: &'a mut T,
    value: T,
    text: WidgetText,
}

impl<'a, T: PartialEq> Radio<'a, T> {
    pub fn new(current: &'a mut T, value: T, text: impl Into<WidgetText>) -> Self {
        Self {
            current,
            value,
            text: text.into(),
        }
    }
}

impl<T: PartialEq> Widget for Radio<'_, T> {
    fn ui(self, ui: &mut Ui) -> Response {
        let Self {
            current,
            value,
            text,
        } = self;
        let text = text.color(ui.visuals().text_color());
        let selected = *current == value;
        let mut response =
            without_native_indicator(ui, |ui| ui.add(egui::RadioButton::new(selected, text)));
        let selected = if response.clicked() && !selected {
            *current = value;
            response.mark_changed();
            true
        } else {
            selected
        };
        paint_indicator(ui, &response, selected);
        response
    }
}

/// Only replace the indicator painting. egui still handles the label layout,
/// hit area, focus, keyboard, and checked state.
fn without_native_indicator(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> Response) -> Response {
    ui.scope(|ui| {
        let widgets = &mut ui.visuals_mut().widgets;
        for visual in [
            &mut widgets.noninteractive,
            &mut widgets.inactive,
            &mut widgets.hovered,
            &mut widgets.active,
            &mut widgets.open,
        ] {
            visual.bg_fill = egui::Color32::TRANSPARENT;
            visual.weak_bg_fill = egui::Color32::TRANSPARENT;
            visual.bg_stroke = Stroke::NONE;
            visual.fg_stroke = Stroke::NONE;
        }
        add(ui)
    })
    .inner
}

fn paint_indicator(ui: &Ui, response: &Response, checked: bool) {
    if !ui.is_rect_visible(response.rect) {
        return;
    }
    let (_, icon) = ui.spacing().icon_rectangles(response.rect);
    let center = icon.center();
    let radius = icon.width() / 2.0;
    let painter = ui.painter();
    // Material state layer around the control.
    let layer = if !response.enabled() {
        0.0
    } else if response.is_pointer_button_down_on() {
        0.12
    } else if response.hovered() || response.has_focus() {
        0.08
    } else {
        0.0
    };
    if layer > 0.0 {
        let tint = if checked { crate::ACCENT } else { crate::TEXT };
        painter.circle_filled(center, radius + 7.0, tint.gamma_multiply(layer));
    }
    if response.has_focus() {
        painter.circle_stroke(center, radius + 7.0, Stroke::new(2.0_f32, crate::ACCENT));
    }
    let color = if !response.enabled() {
        crate::DISABLED_CONTENT
    } else if checked {
        crate::ACCENT
    } else {
        crate::OUTLINE
    };
    painter.circle_stroke(center, radius - 1.0, Stroke::new(2.0_f32, color));
    if checked {
        painter.circle_filled(center, radius * 0.45, color);
    }
}
