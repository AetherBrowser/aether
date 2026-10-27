/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Application tab — localStorage, sessionStorage, cookies.

use egui::{self, Color32, RichText};

use super::super::panel::DevToolsPanel;
use super::super::types::StorageKind;

pub(crate) fn show_application_tab(ui: &mut egui::Ui, panel: &mut DevToolsPanel) {
    egui::ScrollArea::horizontal()
        .id_salt("app_toolbar_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut kind = panel.state.storage_kind;
                let mut changed = false;
                changed |= ui
                    .selectable_value(&mut kind, StorageKind::LocalStorage, "localStorage")
                    .clicked();
                changed |= ui
                    .selectable_value(
                        &mut kind,
                        StorageKind::SessionStorage,
                        "sessionStorage",
                    )
                    .clicked();
                changed |= ui
                    .selectable_value(&mut kind, StorageKind::Cookies, "Cookies")
                    .clicked();
                if changed {
                    panel.state.storage_kind = kind;
                }
                if ui.button("Refresh Storage").clicked() {
                    panel.refresh_storage();
                }
            });
        });

    ui.separator();

    let avail = ui.available_size();
    egui::ScrollArea::both()
        .id_salt("application_tab_scroll")
        .auto_shrink([false, false])
        .max_width(avail.x)
        .max_height(avail.y)
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

            match panel.state.storage_kind {
                StorageKind::LocalStorage | StorageKind::SessionStorage => {
                    let items = if panel.state.storage_kind == StorageKind::LocalStorage {
                        &panel.state.local_storage
                    } else {
                        &panel.state.session_storage
                    };
                    if items.is_empty() {
                        ui.label(
                            RichText::new("No storage data. Click \"Refresh Storage\".")
                                .color(Color32::GRAY),
                        );
                        return;
                    }
                    for item in items {
                        let is_expanded = panel.state.selected_storage_key.as_deref() ==
                            Some(item.key.as_str());
                        let tri = if is_expanded { "▾" } else { "▸" };
                        let row_resp = ui.horizontal(|ui| {
                            ui.label(RichText::new(tri).monospace().strong());
                            ui.label(RichText::new(&item.key).monospace().strong());
                            ui.label(RichText::new(&item.value).monospace());
                        });
                        if row_resp.response.clicked() {
                            panel.state.selected_storage_key = if is_expanded {
                                None
                            } else {
                                Some(item.key.clone())
                            };
                        }
                        if is_expanded {
                            egui::Frame::NONE
                                .inner_margin(egui::Margin {
                                    left: 16,
                                    right: 16,
                                    top: 4,
                                    bottom: 4,
                                })
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&item.value).monospace(),
                                        )
                                        .wrap(),
                                    );
                                    if ui.button("Copy Value").clicked() {
                                        ui.ctx().copy_text(item.value.clone());
                                    }
                                });
                        }
                    }
                },
                StorageKind::Cookies => {
                    if panel.state.cookies.is_empty() {
                        ui.label(
                            RichText::new("No cookie data from engine.")
                                .color(Color32::GRAY),
                        );
                        return;
                    }
                    ui.label(
                        RichText::new(format!(
                            "{} cookies",
                            panel.state.cookies.len()
                        ))
                        .strong(),
                    );
                    for c in &panel.state.cookies {
                        let is_expanded = panel.state.selected_storage_key.as_deref() ==
                            Some(c.name.as_str());
                        let tri = if is_expanded { "▾" } else { "▸" };
                        let row_resp = ui.horizontal(|ui| {
                            ui.label(RichText::new(tri).monospace().strong());
                            ui.label(RichText::new(&c.name).monospace().strong());
                            ui.label(RichText::new(&c.value).monospace());
                            ui.label(
                                RichText::new(&c.domain).monospace().color(Color32::GRAY),
                            );
                        });
                        if row_resp.response.clicked() {
                            panel.state.selected_storage_key = if is_expanded {
                                None
                            } else {
                                Some(c.name.clone())
                            };
                        }
                        if is_expanded {
                            egui::Frame::NONE
                                .inner_margin(egui::Margin {
                                    left: 16,
                                    right: 16,
                                    top: 4,
                                    bottom: 4,
                                })
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(&c.value).monospace(),
                                        )
                                        .wrap(),
                                    );
                                    ui.separator();
                                    egui::Grid::new(format!("cookie_detail_{}", c.name))
                                        .striped(true)
                                        .show(ui, |ui| {
                                            ui.label("Domain");
                                            ui.label(
                                                RichText::new(&c.domain).monospace(),
                                            );
                                            ui.end_row();
                                            ui.label("Path");
                                            ui.label(
                                                RichText::new(&c.path).monospace(),
                                            );
                                            ui.end_row();
                                            ui.label("HttpOnly");
                                            ui.label(if c.http_only { "✓" } else { "No" });
                                            ui.end_row();
                                            ui.label("Secure");
                                            ui.label(if c.secure { "✓" } else { "No" });
                                            ui.end_row();
                                            ui.label("SameSite");
                                            ui.label(
                                                RichText::new(&c.same_site).monospace(),
                                            );
                                            ui.end_row();
                                            ui.label("Expires");
                                            ui.label(
                                                RichText::new(
                                                    c.expires.as_deref().unwrap_or("—"),
                                                )
                                                .monospace(),
                                            );
                                            ui.end_row();
                                        });
                                    if ui.button("Copy Value").clicked() {
                                        ui.ctx().copy_text(c.value.clone());
                                    }
                                });
                        }
                    }
                },
            }
        });
}
