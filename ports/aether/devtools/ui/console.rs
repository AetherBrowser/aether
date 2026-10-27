/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Console tab — log list with filters, expandable rows, JS eval bar.

use egui::{self, Color32, RichText};

use super::super::panel::DevToolsPanel;
use super::super::types::LogLevel;
use super::utils::contains_ignore_ascii_case;

pub(crate) fn show_console_tab(ui: &mut egui::Ui, panel: &mut DevToolsPanel) {
    ui.horizontal_wrapped(|ui| {
        if ui.button("Clear").clicked() {
            panel.state.logs.clear();
            panel.state.expanded_logs.clear();
        }
        ui.separator();
        let mut filter = panel.state.log_filter_level;
        let mut changed = false;
        changed |= ui.selectable_value(&mut filter, None, "All").clicked();
        changed |= ui
            .selectable_value(&mut filter, Some(LogLevel::Error), "Errors")
            .clicked();
        changed |= ui
            .selectable_value(&mut filter, Some(LogLevel::Warn), "Warnings")
            .clicked();
        changed |= ui
            .selectable_value(&mut filter, Some(LogLevel::Info), "Info")
            .clicked();
        changed |= ui
            .selectable_value(&mut filter, Some(LogLevel::Debug), "Debug")
            .clicked();
        if changed {
            panel.state.log_filter_level = filter;
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut panel.state.log_search)
                .hint_text("Filter…")
                .desired_width(110.0),
        );
        ui.checkbox(&mut panel.state.auto_scroll, "Auto-scroll");
    });

    // Filtered index list (no deep clone of entries).
    let filter_level = panel.state.log_filter_level;
    let search_lower = panel.state.log_search.to_lowercase();
    let filtered: Vec<usize> = panel
        .state
        .logs
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            if let Some(min_level) = filter_level {
                if e.level > min_level {
                    return false;
                }
            }
            if !search_lower.is_empty() {
                if !contains_ignore_ascii_case(&e.message, &search_lower) &&
                    !contains_ignore_ascii_case(&e.target, &search_lower)
                {
                    return false;
                }
            }
            true
        })
        .map(|(i, _)| i)
        .collect();

    // Log list with its own scroll area.
    let auto_scroll = panel.state.auto_scroll;
    egui::ScrollArea::both()
        .id_salt("console_logs_scroll")
        .auto_shrink([false, false])
        .stick_to_bottom(auto_scroll)
        .max_height((ui.available_height() - 32.0).max(40.0))
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            for &idx in &filtered {
                let entry = &panel.state.logs[idx];
                let is_expanded = panel.state.expanded_logs.contains(&idx);
                let is_dark = ui.visuals().dark_mode;
                let (bg_fill, icon, msg_color) = match entry.level {
                    LogLevel::Error => (
                        if is_dark {
                            Color32::from_rgba_unmultiplied(0x5C, 0x1A, 0x1A, 180)
                        } else {
                            Color32::from_rgba_unmultiplied(0xFF, 0xEB, 0xEE, 180)
                        },
                        "⛔",
                        if is_dark {
                            Color32::from_rgb(0xEF, 0x9A, 0x9A)
                        } else {
                            Color32::from_rgb(0xC6, 0x28, 0x28)
                        },
                    ),
                    LogLevel::Warn => (
                        if is_dark {
                            Color32::from_rgba_unmultiplied(0x5C, 0x4A, 0x1A, 180)
                        } else {
                            Color32::from_rgba_unmultiplied(0xFF, 0xF9, 0xC4, 180)
                        },
                        "⚠",
                        if is_dark {
                            Color32::from_rgb(0xFF, 0xE0, 0x82)
                        } else {
                            Color32::from_rgb(0xE6, 0x5C, 0x00)
                        },
                    ),
                    LogLevel::Info => (
                        if idx % 2 == 0 {
                            Color32::TRANSPARENT
                        } else if is_dark {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 8)
                        } else {
                            Color32::from_rgba_unmultiplied(0, 0, 0, 8)
                        },
                        "ℹ",
                        if is_dark {
                            Color32::from_rgb(0xBB, 0xDE, 0xFB)
                        } else {
                            Color32::from_rgb(0x15, 0x65, 0xC0)
                        },
                    ),
                    LogLevel::Debug | LogLevel::Trace => (
                        if idx % 2 == 0 {
                            Color32::TRANSPARENT
                        } else if is_dark {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 8)
                        } else {
                            Color32::from_rgba_unmultiplied(0, 0, 0, 8)
                        },
                        "·",
                        Color32::GRAY,
                    ),
                };

                // Reserve shape slot for background — painted behind text.
                let bg_shape_idx = ui.painter().add(egui::Shape::Noop);
                let row_resp = ui.horizontal(|ui| {
                    // Disclosure triangle.
                    let tri = if is_expanded { "▾" } else { "▸" };
                    let tri_resp = ui.add(
                        egui::Label::new(
                            RichText::new(tri).monospace().strong(),
                        )
                        .sense(egui::Sense::click()),
                    );
                    if tri_resp.clicked() {
                        if is_expanded {
                            panel.state.expanded_logs.remove(&idx);
                        } else {
                            panel.state.expanded_logs.insert(idx);
                        }
                    }
                    ui.label(
                        RichText::new(icon).monospace().color(entry.level.color()),
                    );
                    ui.label(
                        RichText::new(&entry.timestamp)
                            .monospace()
                            .color(Color32::GRAY)
                            .size(11.0),
                    );
                    ui.label(
                        RichText::new(&entry.message)
                            .monospace()
                            .color(msg_color),
                    );
                    ui.with_layout(
                        egui::Layout::right_to_left(egui::Align::Center),
                        |ui| {
                            ui.label(
                                RichText::new(&entry.target)
                                    .monospace()
                                    .color(Color32::from_rgb(0x5C, 0x6B, 0xC0))
                                    .size(11.0),
                            );
                        },
                    );
                });
                // Clicking the row body also toggles expansion.
                if row_resp.response.clicked() {
                    if is_expanded {
                        panel.state.expanded_logs.remove(&idx);
                    } else {
                        panel.state.expanded_logs.insert(idx);
                    }
                }
                // Set background shape behind the text.
                let row_rect = row_resp.response.rect;
                if bg_fill != Color32::TRANSPARENT {
                    ui.painter().set(
                        bg_shape_idx,
                        egui::Shape::rect_filled(row_rect.expand(1.0), 0.0, bg_fill),
                    );
                }
                ui.painter().hline(
                    row_rect.x_range(),
                    row_rect.bottom(),
                    egui::Stroke::new(1.0_f32, ui.visuals().faint_bg_color),
                );

                // Expanded detail block.
                if is_expanded {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin { left: 16, right: 16, top: 4, bottom: 4 })
                        .fill(if is_dark {
                            Color32::from_rgba_unmultiplied(0, 0, 0, 30)
                        } else {
                            Color32::from_rgba_unmultiplied(0, 0, 0, 8)
                        })
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(&entry.message).monospace(),
                                )
                                .wrap(),
                            );
                            ui.horizontal(|ui| {
                                ui.label(
                                    RichText::new(format!("Target: {}", entry.target))
                                        .monospace()
                                        .color(Color32::from_rgb(0x5C, 0x6B, 0xC0))
                                        .size(11.0),
                                );
                                ui.separator();
                                ui.label(
                                    RichText::new(&entry.timestamp)
                                        .monospace()
                                        .color(Color32::GRAY)
                                        .size(11.0),
                                );
                                if ui.button("Copy").clicked() {
                                    ui.ctx().copy_text(entry.message.clone());
                                }
                            });
                            if let Some(st) = &entry.stacktrace {
                                ui.separator();
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(st).monospace().color(Color32::GRAY),
                                    )
                                    .wrap(),
                                );
                            }
                        });
                }
            }
        });

    ui.separator();
    // JS eval bar.
    ui.horizontal(|ui| {
        ui.label(RichText::new("❯").monospace().strong());
        let resp = ui.add(
            egui::TextEdit::singleline(&mut panel.state.js_input)
                .hint_text("Evaluate JavaScript…")
                .font(egui::TextStyle::Monospace)
                .desired_width(ui.available_width()),
        );
        if resp.lost_focus() &&
            ui.input(|i| i.key_pressed(egui::Key::Enter)) &&
            !panel.state.js_input.trim().is_empty()
        {
            let code = panel.state.js_input.trim().to_string();
            panel.evaluate_js(&code);
            panel.state.js_input.clear();
            resp.request_focus();
        }
    });
}
