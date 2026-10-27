/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Performance tab — frame time graph + page timing waterfall.

use egui::{self, Color32, RichText};

use super::super::panel::DevToolsPanel;
use super::super::types::PageTiming;

pub(crate) fn show_performance_tab(ui: &mut egui::Ui, panel: &mut DevToolsPanel) {
    egui::ScrollArea::both()
        .id_salt("perf_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            ui.label(RichText::new("Frame time (ms)").strong());
            if panel.state.frame_times.is_empty() {
                ui.label(
                    RichText::new("Collecting frame timing…").color(Color32::GRAY),
                );
            } else {
                let n = panel.state.frame_times.len();
                let sum: f64 = panel.state.frame_times.iter().sum();
                let avg = sum / n as f64;
                let fps = if avg > 0.0 { 1000.0 / avg } else { 0.0 };
                ui.label(format!(
                    "Avg: {avg:.2} ms  ·  FPS: {fps:.1}  ·  Samples: {n}"
                ));

                let graph_height = 100.0;
                let (response, painter) = ui.allocate_painter(
                    egui::vec2(ui.available_width(), graph_height),
                    egui::Sense::hover(),
                );
                let rect = response.rect;
                let max_val = panel
                    .state
                    .frame_times
                    .iter()
                    .cloned()
                    .fold(50.0f64, f64::max);

                let y60 = rect.bottom() - (16.67 / max_val) as f32 * rect.height();
                if y60 >= rect.top() {
                    draw_dashed_line(
                        &painter,
                        rect.left(),
                        y60,
                        rect.right(),
                        y60,
                        Color32::from_rgb(0x4C, 0xAF, 0x50),
                    );
                    painter.text(
                        egui::pos2(rect.right() - 60.0, y60 - 12.0),
                        egui::Align2::LEFT_CENTER,
                        "60 FPS (16.7ms)",
                        egui::FontId::monospace(9.0),
                        Color32::from_rgb(0x4C, 0xAF, 0x50),
                    );
                }
                let y30 = rect.bottom() - (33.33 / max_val) as f32 * rect.height();
                if y30 >= rect.top() {
                    draw_dashed_line(
                        &painter,
                        rect.left(),
                        y30,
                        rect.right(),
                        y30,
                        Color32::from_rgb(0xFF, 0xC1, 0x07),
                    );
                    painter.text(
                        egui::pos2(rect.right() - 60.0, y30 - 12.0),
                        egui::Align2::LEFT_CENTER,
                        "30 FPS (33.3ms)",
                        egui::FontId::monospace(9.0),
                        Color32::from_rgb(0xFF, 0xC1, 0x07),
                    );
                }

                if n > 1 {
                    let bar_width = (rect.width() / (n - 1) as f32).min(4.0).max(1.0);
                    for (i, &v) in panel.state.frame_times.iter().enumerate() {
                        let x = rect.left() + (i as f32 / (n - 1) as f32) * rect.width();
                        let h = (v as f32 / max_val as f32) * rect.height();
                        let color = if v <= 16.67 {
                            Color32::from_rgb(0x4C, 0xAF, 0x50)
                        } else if v <= 33.33 {
                            Color32::from_rgb(0xFF, 0xC1, 0x07)
                        } else {
                            Color32::from_rgb(0xF4, 0x43, 0x36)
                        };
                        painter.rect_filled(
                            egui::Rect::from_min_max(
                                egui::pos2(x - bar_width / 2.0, rect.bottom() - h),
                                egui::pos2(x + bar_width / 2.0, rect.bottom()),
                            ),
                            0.0,
                            color,
                        );
                    }
                }
            }

            ui.separator();

            ui.horizontal(|ui| {
                ui.label(RichText::new("Page Load & Navigation Timing").strong());
                if ui.button("Measure Page Timing").clicked() {
                    panel.request_page_timing();
                }
            });
            if let Some(timing) = &panel.state.page_timing {
                render_page_timing(ui, timing);
            } else {
                ui.label(
                    RichText::new("Click \"Measure Page Timing\" to evaluate window.performance.timing.")
                        .color(Color32::GRAY),
                );
            }

            ui.separator();
            ui.label(RichText::new("Counters").strong());
            egui::Grid::new("perf_grid")
                .striped(true)
                .num_columns(2)
                .show(ui, |ui| {
                    ui.label("Console entries");
                    ui.label(panel.state.logs.len().to_string());
                    ui.end_row();
                    ui.label("Network entries");
                    ui.label(panel.state.network.len().to_string());
                    ui.end_row();
                    ui.label("DOM nodes");
                    ui.label(count_dom_nodes(&panel.state.dom_tree).to_string());
                    ui.end_row();
                });
        });
}

fn draw_dashed_line(
    painter: &egui::Painter,
    x0: f32,
    y: f32,
    x1: f32,
    _y: f32,
    color: Color32,
) {
    let mut x = x0;
    while x < x1 {
        let end = (x + 6.0).min(x1);
        painter.line_segment(
            [egui::pos2(x, y), egui::pos2(end, y)],
            egui::Stroke::new(1.0_f32, color),
        );
        x += 12.0;
    }
}

fn render_page_timing(ui: &mut egui::Ui, timing: &PageTiming) {
    let items: [(&str, f64, Color32); 6] = [
        ("DNS Lookup", timing.dns_ms, Color32::from_rgb(0x21, 0x96, 0xF3)),
        ("TCP Connect", timing.connect_ms, Color32::from_rgb(0x00, 0x96, 0x88)),
        ("TTFB", timing.ttfb_ms, Color32::from_rgb(0xFF, 0x98, 0x00)),
        ("DOM Interactive", timing.dom_interactive_ms, Color32::from_rgb(0x9C, 0x27, 0xB0)),
        ("DOMContentLoaded", timing.dom_content_loaded_ms, Color32::from_rgb(0xE9, 0x1E, 0x63)),
        ("Total Load", timing.load_ms, Color32::from_rgb(0x4C, 0xAF, 0x50)),
    ];
    let max_val = items.iter().map(|(_, v, _)| *v).fold(1.0f64, f64::max);
    let bar_area = ui.available_width() * 0.6;

    egui::Grid::new("page_timing_grid").striped(true).num_columns(4).show(ui, |ui| {
        ui.label(RichText::new("Phase").strong());
        ui.label(RichText::new("Time").strong());
        ui.label(RichText::new("Waterfall").strong());
        ui.end_row();
        for (name, ms, color) in &items {
            ui.label(RichText::new(*name).monospace());
            ui.label(RichText::new(format!("{ms:.1} ms")).monospace());
            let (response, painter) = ui.allocate_painter(
                egui::vec2(bar_area, 16.0),
                egui::Sense::hover(),
            );
            let w = (*ms as f32 / max_val as f32) * response.rect.width();
            painter.rect_filled(
                egui::Rect::from_min_size(
                    response.rect.min,
                    egui::vec2(w.max(2.0), 16.0),
                ),
                2.0,
                *color,
            );
            ui.end_row();
        }
    });
}

fn count_dom_nodes(nodes: &[super::super::types::DomNode]) -> usize {
    nodes.iter().map(|n| 1 + count_dom_nodes(&n.children)).sum()
}
