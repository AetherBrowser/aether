/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Application hamburger menu. Add new entries in [`AppMenu::contents`].

use egui::{Button, CornerRadius, Id, Popup, RectAlign, Sense, Stroke, TextWrapMode, Vec2, vec2};
use euclid::Point2D;
use servo::DeviceIndependentPixel;

/// Matches other chrome menus (see `dialog.rs`).
const APP_MENU_MIN_WIDTH: f32 = 350.0;

/// Empty panel height so the menu block is visible before items are added.
const APP_MENU_MIN_HEIGHT: f32 = 50.0;

/// Gap between the toolbar button and the panel.
const APP_MENU_OFFSET: f32 = 4.0;

const APP_MENU_ID: &str = "app_menu";

/// Application menu opened from the toolbar hamburger button.
pub(crate) struct AppMenu {
    open: bool,
    rect: egui::Rect,
}

impl Default for AppMenu {
    fn default() -> Self {
        Self {
            open: false,
            rect: egui::Rect::NOTHING,
        }
    }
}

impl AppMenu {
    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn close(&mut self) {
        self.open = false;
        self.rect = egui::Rect::NOTHING;
    }

    pub(crate) fn close_ui(&mut self, ctx: &egui::Context) {
        self.close();
        Popup::close_id(ctx, Id::new(APP_MENU_ID));
    }

    pub(crate) fn contains_pointer(&self, position: Point2D<f32, DeviceIndependentPixel>) -> bool {
        self.open && self.rect.contains(egui::pos2(position.x, position.y))
    }

    /// Toggle the panel from the toolbar button and draw it when open.
    pub(crate) fn update(&mut self, button: &egui::Response) {
        let inner = Popup::menu(button)
            .id(Id::new(APP_MENU_ID))
            .align(RectAlign::BOTTOM_END)
            .gap(APP_MENU_OFFSET)
            .width(APP_MENU_MIN_WIDTH)
            .show(|ui| {
                ui.set_min_size(vec2(APP_MENU_MIN_WIDTH, APP_MENU_MIN_HEIGHT));
                Self::contents(ui);
            });

        match inner {
            Some(response) => {
                self.open = true;
                self.rect = response.response.rect;
            },
            None => self.close(),
        }
    }

    /// Menu body. Insert new items with [`Self::item`].
    fn contents(ui: &mut egui::Ui) {
        ui.set_min_width(APP_MENU_MIN_WIDTH);
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.style_mut().visuals.widgets.inactive.weak_bg_fill = ui.visuals().panel_fill;
        ui.style_mut().visuals.widgets.inactive.bg_fill = ui.visuals().panel_fill;
    }

    /// A full-width row ready to host a menu action.
    #[expect(dead_code)]
    fn item(ui: &mut egui::Ui, label: &str) -> egui::Response {
        let button = Button::new(label)
            .corner_radius(CornerRadius::ZERO)
            .stroke(Stroke::NONE)
            .wrap_mode(TextWrapMode::Extend)
            .min_size(Vec2 {
                x: APP_MENU_MIN_WIDTH,
                y: 0.0,
            })
            .sense(Sense::click());
        ui.add(button)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use euclid::Point2D;

    #[test]
    fn default_menu_is_closed() {
        let menu = AppMenu::default();
        assert!(!menu.is_open());
        assert!(!menu.contains_pointer(Point2D::new(10.0, 10.0)));
    }

    #[test]
    fn close_clears_open_state() {
        let mut menu = AppMenu::default();
        menu.open = true;
        menu.close();
        assert!(!menu.is_open());
        assert!(!menu.contains_pointer(Point2D::new(0.0, 0.0)));
    }
}
