/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Inspector tab — DOM tree + element details.

use egui::{self, Color32, RichText};

use super::super::panel::{DevToolsPanel, DevToolsState, find_node_by_path};
use super::super::types::{DomNode, NodeStyleData};
use super::utils::truncate_chars;

pub(crate) fn show_inspector_tab(ui: &mut egui::Ui, panel: &mut DevToolsPanel) {
    ui.horizontal(|ui| {
        if ui.button("Refresh DOM").clicked() {
            panel.request_dom_tree();
        }
        ui.checkbox(&mut panel.state.auto_refresh_dom, "Auto-refresh on load");
    });

    ui.separator();

    if panel.state.dom_tree.is_empty() {
        ui.label(
            RichText::new("No DOM data. Click \"Refresh DOM\" or wait for page load.")
                .color(Color32::GRAY),
        );
        return;
    }

    let mut selected = panel.state.selected_node_path.clone();
    let mut request_children: Option<String> = None;
    let mut request_layout: Option<String> = None;

    ui.columns(2, |cols| {
        egui::ScrollArea::both()
            .id_salt("dom_tree_scroll")
            .auto_shrink([false, false])
            .show(&mut cols[0], |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                for (i, node) in panel.state.dom_tree.iter().enumerate() {
                    render_dom_node(
                        ui,
                        node,
                        &[i],
                        &mut selected,
                        &mut request_children,
                        &mut request_layout,
                        ui.visuals().dark_mode,
                    );
                }
            });

        // Split borrows at the field level so `dom_tree` (immutable) and
        // `selected_node_styles` (mutable) can coexist.
        let DevToolsState {
            dom_tree,
            selected_node_styles,
            ..
        } = &mut panel.state;
        if let Some(node) = find_node_by_path(dom_tree, &selected) {
            let mut pending = PendingCssEdit {
                attr_mods: Vec::new(),
                rule_mods: Vec::new(),
            };
            let node_id = node.unique_id.clone();
            egui::ScrollArea::both()
                .id_salt("dom_details_scroll")
                .auto_shrink([false, false])
                .show(&mut cols[1], |ui| {
                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
                    pending = render_node_details(ui, node, selected_node_styles);
                });
            // Execute write-back mutations after the borrow scope ends.
            for m in pending.attr_mods {
                panel.modify_attribute(&node_id, vec![m]);
            }
            for m in pending.rule_mods {
                panel.modify_rule(&node_id, vec![m]);
            }
        }
    });

    panel.state.selected_node_path = selected;
    if let Some(id) = request_children {
        panel.request_dom_children(&id);
    }
    if let Some(ref id) = request_layout {
        panel.state.selected_node_styles = None;
        panel.request_dom_layout(id);
        panel.request_node_styles(id);
        panel.highlight_dom_node(id);
    }
    if panel.state.selected_node_path.is_empty() && request_layout.is_none() {
        panel.clear_highlight();
    }
}

/// Pending write-back mutations collected during rendering.
struct PendingCssEdit {
    attr_mods: Vec<devtools_traits::AttrModification>,
    rule_mods: Vec<devtools_traits::RuleModification>,
}

fn render_node_details(
    ui: &mut egui::Ui,
    node: &DomNode,
    styles: &mut Option<NodeStyleData>,
) -> PendingCssEdit {
    let mut pending = PendingCssEdit {
        attr_mods: Vec::new(),
        rule_mods: Vec::new(),
    };

    ui.heading(format!("<{}>", node.node_name));
    for attr in &node.attrs {
        if attr.name == "id" {
            ui.label(format!("id: {}", attr.value));
        }
        if attr.name == "class" {
            ui.label(format!("class: {}", attr.value));
        }
    }
    ui.separator();
    ui.label(RichText::new("Attributes").strong());
    egui::Grid::new("attrs_grid").striped(true).show(ui, |ui| {
        for attr in &node.attrs {
            ui.label(RichText::new(&attr.name).monospace().strong());
            ui.label(RichText::new(&attr.value).monospace());
            ui.end_row();
        }
    });

    if let Some(bm) = &node.box_model {
        ui.separator();
        ui.label(RichText::new("Box model").strong());
        egui::Grid::new("box_grid").striped(true).show(ui, |ui| {
            for (label, value) in [
                ("width", format!("{:.1}", bm.width)),
                ("height", format!("{:.1}", bm.height)),
                ("display", bm.display.clone()),
                ("position", bm.position.clone()),
                ("box-sizing", bm.box_sizing.clone()),
                (
                    "margin",
                    format!(
                        "{} {} {} {}",
                        bm.margin_top, bm.margin_right, bm.margin_bottom, bm.margin_left
                    ),
                ),
                (
                    "border",
                    format!(
                        "{} {} {} {}",
                        bm.border_top_width,
                        bm.border_right_width,
                        bm.border_bottom_width,
                        bm.border_left_width
                    ),
                ),
                (
                    "padding",
                    format!(
                        "{} {} {} {}",
                        bm.padding_top, bm.padding_right, bm.padding_bottom, bm.padding_left
                    ),
                ),
            ] {
                ui.label(label);
                ui.label(value);
                ui.end_row();
            }
        });
    }

    if let Some((top, right, bottom, left)) = &node.auto_margins {
        ui.separator();
        ui.label(RichText::new("Auto Margins").strong());
        ui.horizontal(|ui| {
            let mk = |auto: bool| {
                if auto {
                    RichText::new("auto").color(Color32::from_rgb(0x21, 0x96, 0xF3))
                } else {
                    RichText::new("0").color(Color32::GRAY)
                }
            };
            ui.label(mk(*top));
            ui.horizontal(|ui| {
                ui.label(mk(*left));
                ui.label(" ");
                ui.label(mk(*right));
            });
            ui.label(mk(*bottom));
        });
    }

    if let Some(text) = &node.node_value {
        ui.separator();
        ui.label(RichText::new("Value").strong());
        ui.label(RichText::new(text).monospace());
    }
    if let Some(display) = &node.display {
        ui.label(format!("display: {display}"));
    }
    if !node.is_displayed {
        ui.label(RichText::new("hidden").color(Color32::GRAY).italics());
    }
    if let Some(mode) = &node.shadow_root_mode {
        ui.label(format!("shadow-root: {mode}"));
    }
    if let Some(dt) = &node.doctype {
        ui.label(
            RichText::new(format!("<!DOCTYPE {dt}>"))
                .monospace()
                .color(Color32::GRAY),
        );
    }

    if let Some(styles) = styles.as_mut() {
        render_styles_section(ui, styles, &mut pending);
    }
    pending
}

/// Render the Styles / Computed / Event Listeners sections.
/// Mutates `styles` directly so `TextEdit` state persists across frames.
/// Collects write-back mutations in `pending` (executed after rendering).
fn render_styles_section(
    ui: &mut egui::Ui,
    styles: &mut NodeStyleData,
    pending: &mut PendingCssEdit,
) {
    // Inline styles
    if !styles.inline_styles.is_empty() {
        ui.separator();
        ui.label(RichText::new("Inline Styles").strong());
        let mut inline_commit = false;
        egui::Grid::new("inline_styles_grid")
            .striped(true)
            .num_columns(3)
            .show(ui, |ui| {
                for prop in &mut styles.inline_styles {
                    ui.label(
                        RichText::new(&prop.name)
                            .monospace()
                            .color(Color32::from_rgb(0x7B, 0x1F, 0xA2)),
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut prop.value)
                            .font(egui::TextStyle::Monospace)
                            .desired_width(150.0),
                    );
                    if resp.lost_focus() &&
                        ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        inline_commit = true;
                    }
                    let prio = if prop.priority.is_empty() {
                        String::new()
                    } else {
                        format!(" {}", prop.priority)
                    };
                    ui.label(RichText::new(prio).monospace().color(Color32::GRAY));
                    ui.end_row();
                }
            });
        if inline_commit {
            // Reconstruct the full style attribute from ALL inline properties.
            let full_style = styles
                .inline_styles
                .iter()
                .map(|p| {
                    if p.priority.is_empty() {
                        format!("{}: {}", p.name, p.value)
                    } else {
                        format!("{}: {} {}", p.name, p.value, p.priority)
                    }
                })
                .collect::<Vec<_>>()
                .join("; ");
            pending.attr_mods.push(devtools_traits::AttrModification {
                attribute_name: "style".into(),
                new_value: Some(full_style),
            });
        }
    }

    // Matched CSS Rules
    if !styles.matched_rules.is_empty() {
        ui.separator();
        ui.label(RichText::new("Matched CSS Rules").strong());
        let mut rule_commit: Option<(usize, u32, String, String)> = None;
        for (ri, rule) in styles.matched_rules.iter_mut().enumerate() {
            let source_label = if rule.is_inline {
                "inline".to_string()
            } else {
                rule.href.as_deref().unwrap_or("stylesheet").to_string()
            };
            egui::CollapsingHeader::new(
                RichText::new(format!("{}  ({})", rule.selector, source_label))
                    .monospace()
                    .strong(),
            )
            .default_open(true)
            .id_salt(format!("css_rule_{}_{}", rule.stylesheet_index, rule.block_id))
            .show(ui, |ui| {
                egui::Grid::new(format!(
                    "css_rule_grid_{}_{}",
                    rule.stylesheet_index, rule.block_id
                ))
                .striped(true)
                .num_columns(3)
                .show(ui, |ui| {
                    for (pi, prop) in rule.declarations.iter_mut().enumerate() {
                        let name_color = if prop.is_used {
                            Color32::from_rgb(0x7B, 0x1F, 0xA2)
                        } else {
                            Color32::GRAY
                        };
                        ui.label(RichText::new(&prop.name).monospace().color(name_color));
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut prop.value)
                                .font(egui::TextStyle::Monospace)
                                .desired_width(150.0),
                        );
                        if resp.lost_focus() &&
                            ui.input(|i| i.key_pressed(egui::Key::Enter))
                        {
                            rule_commit =
                                Some((ri, pi as u32, prop.name.clone(), prop.value.clone()));
                        }
                        let prio = if prop.priority.is_empty() {
                            String::new()
                        } else {
                            format!(" {}", prop.priority)
                        };
                        ui.label(RichText::new(prio).monospace().color(Color32::GRAY));
                        ui.end_row();
                    }
                });
            });
        }
        if let Some((_ri, pi, name, value)) = rule_commit {
            pending.rule_mods.push(devtools_traits::RuleModification {
                type_: "setRuleText".into(),
                index: pi,
                name,
                value,
                priority: String::new(),
            });
        }
    }

    // Computed Styles (searchable)
    if !styles.computed_styles.is_empty() {
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Computed Styles").strong());
            ui.label(
                RichText::new(format!("({})", styles.computed_styles.len()))
                    .color(Color32::GRAY),
            );
        });
        egui::CollapsingHeader::new(RichText::new("Show all computed properties"))
            .default_open(false)
            .id_salt("computed_styles_collapsing")
            .show(ui, |ui| {
                egui::Grid::new("computed_styles_grid")
                    .striped(true)
                    .num_columns(2)
                    .show(ui, |ui| {
                        for prop in &styles.computed_styles {
                            ui.label(
                                RichText::new(&prop.name)
                                    .monospace()
                                    .color(Color32::from_rgb(0x7B, 0x1F, 0xA2)),
                            );
                            ui.label(
                                RichText::new(&prop.value)
                                    .monospace()
                                    .color(Color32::from_rgb(0x2E, 0x7D, 0x32)),
                            );
                            ui.end_row();
                        }
                    });
            });
    }

    // Event Listeners
    if !styles.event_listeners.is_empty() {
        ui.separator();
        ui.label(RichText::new("Event Listeners").strong());
        egui::Grid::new("event_listeners_grid")
            .striped(true)
            .num_columns(2)
            .show(ui, |ui| {
                for listener in &styles.event_listeners {
                    ui.label(
                        RichText::new(&listener.event_type)
                            .monospace()
                            .color(Color32::from_rgb(0xFF, 0x98, 0x00)),
                    );
                    let phase = if listener.capturing {
                        RichText::new("capturing").color(Color32::from_rgb(0xE9, 0x1E, 0x63))
                    } else {
                        RichText::new("bubbling").color(Color32::from_rgb(0x21, 0x96, 0xF3))
                    };
                    ui.label(phase);
                    ui.end_row();
                }
            });
    }
}

fn render_dom_node(
    ui: &mut egui::Ui,
    node: &DomNode,
    path: &[usize],
    selected_path: &mut Vec<usize>,
    request_children: &mut Option<String>,
    request_layout: &mut Option<String>,
    dark_mode: bool,
) {
    let is_selected = *selected_path == path;

    if node.num_children == 0 && !node.is_shadow_host {
        let resp = ui.selectable_label(is_selected, render_dom_label_rich(node, dark_mode));
        if resp.clicked() {
            *selected_path = path.to_vec();
            *request_layout = Some(node.unique_id.clone());
        }
        return;
    }

    let default_open = path.len() <= 2;
    let id = ui.make_persistent_id(format!("dom_node_{}", node.unique_id));
    let collapsing = egui::collapsing_header::CollapsingState::load_with_default_open(
        ui.ctx(),
        id,
        default_open,
    );
    let header = collapsing.show_header(ui, |ui| {
        ui.selectable_label(is_selected, render_dom_label_rich(node, dark_mode))
    });
    let was_open = header.is_open();
    let (_toggle, header_inner, _body) = header.body(|ui| {
        if !node.children_loaded {
            *request_children = Some(node.unique_id.clone());
            ui.label(
                RichText::new("Loading children…")
                    .color(Color32::GRAY)
                    .italics(),
            );
        } else {
            for (i, child) in node.children.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(i);
                render_dom_node(
                    ui,
                    child,
                    &child_path,
                    selected_path,
                    request_children,
                    request_layout,
                    dark_mode,
                );
            }
        }
    });
    if header_inner.inner.clicked() {
        *selected_path = path.to_vec();
        *request_layout = Some(node.unique_id.clone());
    }
    if was_open && !node.children_loaded && request_children.is_none() {
        *request_children = Some(node.unique_id.clone());
    }
}

fn render_dom_label_rich(node: &DomNode, dark_mode: bool) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat};
    let (tag_color, attr_name_color, attr_value_color, text_color) = if dark_mode {
        (
            Color32::from_rgb(0xBB, 0x86, 0xFC),
            Color32::from_rgb(0xFF, 0xAB, 0x70),
            Color32::from_rgb(0x81, 0xC7, 0x84),
            Color32::from_rgb(0xE0, 0xE0, 0xE0),
        )
    } else {
        (
            Color32::from_rgb(0x7B, 0x1F, 0xA2),
            Color32::from_rgb(0xE6, 0x5C, 0x00),
            Color32::from_rgb(0x2E, 0x7D, 0x32),
            Color32::from_rgb(0x42, 0x42, 0x42),
        )
    };
    let font = egui::FontId::monospace(12.0);

    let mut job = LayoutJob::default();
    match node.node_type {
        3 => {
            let preview = truncate_chars(node.node_value.as_deref().unwrap_or(""), 40);
            job.append(
                &format!("\"{preview}\""),
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: text_color,
                    ..Default::default()
                },
            );
        },
        8 | 9 | 10 => {
            let text = match node.node_type {
                8 => "<!-- -->".to_string(),
                9 => "#document".to_string(),
                _ => format!("<!DOCTYPE {}>", node.node_name),
            };
            job.append(
                &text,
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: Color32::GRAY,
                    ..Default::default()
                },
            );
        },
        _ => {
            job.append(
                "<",
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: tag_color,
                    ..Default::default()
                },
            );
            job.append(
                &node.node_name.to_lowercase(),
                0.0,
                TextFormat {
                    font_id: font.clone(),
                    color: tag_color,
                    ..Default::default()
                },
            );
            if let Some(a) = node.attrs.iter().find(|a| a.name == "id") {
                job.append(
                    "#",
                    0.0,
                    TextFormat {
                        font_id: font.clone(),
                        color: attr_name_color,
                        ..Default::default()
                    },
                );
                job.append(
                    &a.value,
                    0.0,
                    TextFormat {
                        font_id: font.clone(),
                        color: attr_value_color,
                        ..Default::default()
                    },
                );
            }
            if let Some(a) = node.attrs.iter().find(|a| a.name == "class") {
                if let Some(first) = a.value.split_whitespace().next() {
                    job.append(
                        ".",
                        0.0,
                        TextFormat {
                            font_id: font.clone(),
                            color: attr_name_color,
                            ..Default::default()
                        },
                    );
                    job.append(
                        first,
                        0.0,
                        TextFormat {
                            font_id: font.clone(),
                            color: attr_value_color,
                            ..Default::default()
                        },
                    );
                }
            }
            job.append(
                ">",
                0.0,
                TextFormat {
                    font_id: font,
                    color: tag_color,
                    ..Default::default()
                },
            );
        },
    }
    // Event-listener badge.
    if node.has_event_listeners {
        job.append(
            " ev",
            0.0,
            TextFormat {
                font_id: egui::FontId::monospace(10.0),
                color: Color32::from_rgb(0xFF, 0x98, 0x00),
                ..Default::default()
            },
        );
    }
    job
}


