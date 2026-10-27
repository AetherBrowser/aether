/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Network tab — request list + expandable details.

use egui::{self, Color32, RichText};

use super::super::panel::DevToolsPanel;
use super::utils::{contains_ignore_ascii_case, format_bytes};

pub(crate) fn show_network_tab(ui: &mut egui::Ui, panel: &mut DevToolsPanel) {
    ui.horizontal(|ui| {
        if ui.button("Clear").clicked() {
            panel.state.network.clear();
            panel.state.selected_network = None;
        }
        let label = if panel.state.network_recording {
            "⏸ Pause"
        } else {
            "⏺ Record"
        };
        if ui.button(label).clicked() {
            panel.state.network_recording = !panel.state.network_recording;
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut panel.state.network_search)
                .hint_text("Filter URL…")
                .desired_width(180.0),
        );
    });

    ui.separator();

    let search_lower = panel.state.network_search.to_lowercase();
    let filtered: Vec<usize> = panel
        .state
        .network
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            search_lower.is_empty() || contains_ignore_ascii_case(&e.url, &search_lower)
        })
        .map(|(i, _)| i)
        .collect();

    egui::ScrollArea::both()
        .id_salt("network_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            ui.horizontal(|ui| {
                ui.label(RichText::new("  ").monospace());
                ui.label(RichText::new("Status").monospace().strong());
                ui.label(RichText::new("Method").monospace().strong());
                ui.label(RichText::new("URL").monospace().strong());
                ui.label(RichText::new("Size").monospace().strong());
                ui.label(RichText::new("Time").monospace().strong());
            });

            for &idx in &filtered {
                let entry = &panel.state.network[idx];
                let status_color = match entry.status {
                    Some(s) if (200..300).contains(&s) => Color32::from_rgb(0x4C, 0xAF, 0x50),
                    Some(s) if (300..400).contains(&s) => Color32::from_rgb(0xFF, 0xC1, 0x07),
                    Some(s) if s >= 400 => Color32::from_rgb(0xF4, 0x43, 0x36),
                    Some(_) => Color32::from_rgb(0x4C, 0xAF, 0x50),
                    None if entry.failed => Color32::from_rgb(0xF4, 0x43, 0x36),
                    None => Color32::GRAY,
                };
                let is_expanded = panel.state.selected_network == Some(idx);
                let tri = if is_expanded { "▾" } else { "▸" };

                let row_resp = ui.horizontal(|ui| {
                    ui.label(RichText::new(tri).monospace().strong());
                    ui.label(
                        RichText::new(
                            entry
                                .status
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| {
                                    if entry.failed { "ERR" } else { "—" }.into()
                                }),
                        )
                        .monospace()
                        .color(status_color),
                    );
                    ui.label(RichText::new(&entry.method).monospace());
                    ui.label(RichText::new(&entry.url).monospace());
                    ui.label(RichText::new(format_bytes(entry.body_size)).monospace());
                    ui.label(
                        RichText::new(format!("{}ms", entry.duration_ms as u64))
                            .monospace(),
                    );
                });
                if row_resp.response.clicked() {
                    panel.state.selected_network = if is_expanded {
                        None
                    } else {
                        Some(idx)
                    };
                }

                if is_expanded {
                    egui::Frame::NONE
                        .inner_margin(egui::Margin { left: 16, right: 16, top: 6, bottom: 6 })
                        .show(ui, |ui| {
                            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);

                            // General info.
                            egui::Grid::new(format!("net_detail_{idx}"))
                                .striped(true)
                                .show(ui, |ui| {
                                    ui.label("Request ID");
                                    ui.label(
                                        RichText::new(&entry.request_id).monospace(),
                                    );
                                    ui.end_row();
                                    ui.label("URL");
                                    ui.label(RichText::new(&entry.url).monospace());
                                    ui.end_row();
                                    ui.label("Method");
                                    ui.label(RichText::new(&entry.method).monospace());
                                    ui.end_row();
                                    ui.label("Status");
                                    ui.label(
                                        RichText::new(format!(
                                            "{} {}",
                                            entry
                                                .status
                                                .map(|s| s.to_string())
                                                .unwrap_or_default(),
                                            entry.status_text.as_deref().unwrap_or("")
                                        ))
                                        .monospace(),
                                    );
                                    ui.end_row();
                                    if let Some(dest) = &entry.destination {
                                        ui.label("Destination");
                                        ui.label(RichText::new(dest).monospace());
                                        ui.end_row();
                                    }
                                    if let Some(mime) = &entry.mime_type {
                                        ui.label("MIME type");
                                        ui.label(RichText::new(mime).monospace());
                                        ui.end_row();
                                    }
                                    ui.label("XHR");
                                    ui.label(if entry.is_xhr { "Yes" } else { "No" });
                                    ui.end_row();
                                    ui.label("From cache");
                                    ui.label(
                                        if entry.from_cache { "Yes" } else { "No" },
                                    );
                                    ui.end_row();
                                    ui.label("Duration");
                                    ui.label(format!("{:.1} ms", entry.duration_ms));
                                    ui.end_row();
                                    ui.label("Size");
                                    ui.label(format_bytes(entry.body_size));
                                    ui.end_row();
                                });

                            // Headers section.
                            if !entry.request_headers.is_empty() {
                                egui::CollapsingHeader::new(
                                    RichText::new("Request Headers").strong(),
                                )
                                .default_open(true)
                                .id_salt(format!("net_req_{idx}"))
                                .show(ui, |ui| {
                                    egui::Grid::new(format!("net_req_grid_{idx}"))
                                        .striped(true)
                                        .show(ui, |ui| {
                                            for (k, v) in &entry.request_headers {
                                                ui.label(
                                                    RichText::new(k).monospace().strong(),
                                                );
                                                ui.label(RichText::new(v).monospace());
                                                ui.end_row();
                                            }
                                        });
                                });
                            }
                            if !entry.response_headers.is_empty() {
                                egui::CollapsingHeader::new(
                                    RichText::new("Response Headers").strong(),
                                )
                                .default_open(true)
                                .id_salt(format!("net_resp_{idx}"))
                                .show(ui, |ui| {
                                    egui::Grid::new(format!("net_resp_grid_{idx}"))
                                        .striped(true)
                                        .show(ui, |ui| {
                                            for (k, v) in &entry.response_headers {
                                                ui.label(
                                                    RichText::new(k).monospace().strong(),
                                                );
                                                ui.label(RichText::new(v).monospace());
                                                ui.end_row();
                                            }
                                        });
                                });
                            }

                            // Payload (request body).
                            if let Some(body) = &entry.request_body {
                                egui::CollapsingHeader::new(
                                    RichText::new("Request Body").strong(),
                                )
                                .default_open(false)
                                .id_salt(format!("net_payload_{idx}"))
                                .show(ui, |ui| {
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(body).monospace(),
                                        )
                                        .wrap(),
                                    );
                                });
                            }

                            // Response body preview.
                            if let Some(body) = &entry.response_body {
                                egui::CollapsingHeader::new(
                                    RichText::new(format!(
                                        "Response Body ({})",
                                        if entry.response_is_base64 {
                                            "base64"
                                        } else {
                                            "text"
                                        }
                                    ))
                                    .strong(),
                                )
                                .default_open(false)
                                .id_salt(format!("net_resp_body_{idx}"))
                                .show(ui, |ui| {
                                    let preview = super::utils::truncate_chars(body, 2000);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(preview).monospace(),
                                        )
                                        .wrap(),
                                    );
                                });
                            }

                            // Cookies.
                            if !entry.request_cookies.is_empty() {
                                egui::CollapsingHeader::new(
                                    RichText::new("Request Cookies").strong(),
                                )
                                .default_open(false)
                                .id_salt(format!("net_req_cookies_{idx}"))
                                .show(ui, |ui| {
                                    egui::Grid::new(format!("net_req_cookies_grid_{idx}"))
                                        .striped(true)
                                        .show(ui, |ui| {
                                            for c in &entry.request_cookies {
                                                ui.label(
                                                    RichText::new(&c.name)
                                                        .monospace()
                                                        .strong(),
                                                );
                                                ui.label(
                                                    RichText::new(&c.value).monospace(),
                                                );
                                                ui.end_row();
                                            }
                                        });
                                });
                            }
                            if !entry.response_cookies.is_empty() {
                                egui::CollapsingHeader::new(
                                    RichText::new("Response Cookies (Set-Cookie)").strong(),
                                )
                                .default_open(false)
                                .id_salt(format!("net_resp_cookies_{idx}"))
                                .show(ui, |ui| {
                                    egui::Grid::new(format!("net_resp_cookies_grid_{idx}"))
                                        .striped(true)
                                        .show(ui, |ui| {
                                            for c in &entry.response_cookies {
                                                ui.label(
                                                    RichText::new(&c.name)
                                                        .monospace()
                                                        .strong(),
                                                );
                                                ui.label(
                                                    RichText::new(&c.value).monospace(),
                                                );
                                                if c.http_only {
                                                    ui.label(
                                                        RichText::new("HttpOnly")
                                                            .color(Color32::from_rgb(
                                                                0xFF, 0x98, 0x00,
                                                            )),
                                                    );
                                                }
                                                if c.secure {
                                                    ui.label(
                                                        RichText::new("Secure")
                                                            .color(Color32::from_rgb(
                                                                0x21, 0x96, 0xF3,
                                                            )),
                                                    );
                                                }
                                                ui.end_row();
                                            }
                                        });
                                });
                            }

                            // Timing waterfall.
                            egui::CollapsingHeader::new(
                                RichText::new("Timing Waterfall").strong(),
                            )
                            .default_open(true)
                            .id_salt(format!("net_timings_{idx}"))
                            .show(ui, |ui| {
                                render_timing_waterfall(ui, &entry.timings, entry.duration_ms);
                            });
                        });
                }
            }
        });
}

/// Render a horizontal timing waterfall bar for a network request.
fn render_timing_waterfall(
    ui: &mut egui::Ui,
    timings: &super::super::types::NetworkTimings,
    total_ms: f64,
) {
    let total = if total_ms > 0.0 { total_ms } else { 1.0 };
    let bar_width = ui.available_width().min(300.0);
    let bar_height = 18.0;

    let phases: [(&str, f64, Color32); 3] = [
        ("Connect", timings.connect_ms, Color32::from_rgb(0x00, 0x96, 0x88)),
        ("Send", timings.send_ms, Color32::from_rgb(0xFF, 0x98, 0x00)),
        ("Receive", timings.receive_ms, Color32::from_rgb(0x4C, 0xAF, 0x50)),
    ];

    // Draw the stacked bar.
    let (response, painter) = ui.allocate_painter(
        egui::vec2(bar_width, bar_height),
        egui::Sense::hover(),
    );
    let rect = response.rect;
    let mut x_offset = 0.0f32;
    for (_, ms, color) in &phases {
        let w = (*ms as f32 / total as f32) * rect.width();
        if w > 0.0 {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left() + x_offset, rect.top()),
                    egui::vec2(w.max(1.0), bar_height),
                ),
                2.0,
                *color,
            );
            x_offset += w;
        }
    }

    // Labels.
    for (name, ms, color) in &phases {
        if *ms > 0.0 {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("●")
                        .color(*color)
                        .size(10.0),
                );
                ui.label(
                    RichText::new(format!("{name}: {ms:.1} ms"))
                        .monospace()
                        .size(11.0),
                );
            });
        }
    }
    ui.label(
        RichText::new(format!("Total: {total:.1} ms"))
            .monospace()
            .strong()
            .size(11.0),
    );
}
