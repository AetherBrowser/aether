/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! DevTools panel UI entry point and tab rendering.

pub(crate) mod utils;

mod application;
mod console;
mod inspector;
mod network;
mod performance;

use egui::{self, RichText};

use super::panel::DevToolsPanel;
use super::types::DevtoolsTab;

/// What the user did in the DevTools panel this frame.
#[derive(Clone, Copy)]
pub struct DevtoolsPanelResult {
    pub close_clicked: bool,
    pub close_button_rect: egui::Rect,
}

/// Render the full DevTools panel content (header + tab bar + active tab).
///
/// **No outer `ScrollArea`** — each tab manages its own scroll regions.
pub fn show_devtools_panel(
    ui: &mut egui::Ui,
    panel: &mut DevToolsPanel,
) -> DevtoolsPanelResult {
    let mut close_clicked = false;
    let mut close_button_rect = egui::Rect::NOTHING;
    let old_tab = panel.state.active_tab;
    let mut tab = panel.state.active_tab;
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    ui.horizontal(|ui| {
        egui::ScrollArea::horizontal()
            .id_salt("devtools_tabs_bar_scroll")
            .auto_shrink([false, true])
            .max_width((ui.available_width() - 36.0).max(0.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("DevTools").strong());
                    if !panel.state.page_title.is_empty() {
                        ui.label(
                            RichText::new(format!("— {}", panel.state.page_title))
                                .color(egui::Color32::GRAY),
                        );
                    }
                    ui.separator();
                    ui.selectable_value(&mut tab, DevtoolsTab::Console, "Console");
                    ui.selectable_value(&mut tab, DevtoolsTab::Inspector, "Inspector");
                    ui.selectable_value(&mut tab, DevtoolsTab::Network, "Network");
                    ui.selectable_value(&mut tab, DevtoolsTab::Application, "Application");
                    ui.selectable_value(&mut tab, DevtoolsTab::Performance, "Performance");
                });
            });
        // Close button always visible outside the scroll area.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let resp = ui
                .button(RichText::new("✕").strong())
                .on_hover_text("Close DevTools (F12)");
            close_clicked = resp.clicked();
            close_button_rect = resp.rect;
        });
    });
    panel.state.active_tab = tab;

    // Auto-fetch data when switching tabs; clear highlight when leaving Inspector.
    if tab != old_tab {
        if old_tab == DevtoolsTab::Inspector {
            panel.clear_highlight();
        }
        match tab {
            DevtoolsTab::Inspector if panel.state.dom_tree.is_empty() => {
                panel.request_dom_tree();
            },
            DevtoolsTab::Application => {
                panel.refresh_storage();
            },
            DevtoolsTab::Performance if panel.state.page_timing.is_none() => {
                panel.request_page_timing();
            },
            _ => {},
        }
    }

    ui.separator();
    // Each tab manages its own scroll regions.
    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
    match tab {
        DevtoolsTab::Console => console::show_console_tab(ui, panel),
        DevtoolsTab::Inspector => inspector::show_inspector_tab(ui, panel),
        DevtoolsTab::Network => network::show_network_tab(ui, panel),
        DevtoolsTab::Application => application::show_application_tab(ui, panel),
        DevtoolsTab::Performance => performance::show_performance_tab(ui, panel),
    }

    DevtoolsPanelResult {
        close_clicked,
        close_button_rect,
    }
}
