/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! The bookmarks bar below the location bar, and its folder menus.

mod drag;
mod state;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::rc::Rc;

use aether_places::{BookmarkEdit, BookmarkNode};
use egui::containers::menu::{MenuButton, SubMenuButton};
use egui::load::SizedTexture;
use egui::{
    Align, Button, Color32, CornerRadius, Image, Label, Layout, Popup, Rect, RichText, ScrollArea,
    Sense, Stroke, TextStyle, TextureOptions, UiBuilder, Vec2, vec2,
};
use url::Url;

use self::drag::{BarItem, Drag, DropList, DropSpot};
pub(crate) use self::state::{BarVisibility, BarVisibilitySetting, BookmarksBarState};
use crate::desktop::gui::truncate_with_ellipsis;
use crate::desktop::icons::{TOOLBAR_BUTTON_CORNER_RADIUS, ToolbarIcon, ToolbarIconCache};

const ITEM_HEIGHT: f32 = 24.0;

const ICON_SIZE: f32 = 16.0;

const ITEM_SPACING: f32 = 2.0;

/// Longest title of a bar item, in characters.
const BAR_TITLE_LENGTH: usize = 24;

/// Longest title of a menu row, in characters.
const MENU_TITLE_LENGTH: usize = 48;

const OVERFLOW_BUTTON_WIDTH: f32 = 24.0;

const MENU_MIN_WIDTH: f32 = 220.0;

const MENU_MAX_HEIGHT: f32 = 480.0;

const EMPTY_HINT: &str = "Select the star in the address bar to add bookmarks here.";

/// What the user asked from the bookmarks bar.
#[derive(Debug, PartialEq)]
pub(crate) enum BookmarksBarAction {
    Open(Url),
    OpenInNewTab(Url),
    /// Opens the editor of a bookmark or a folder in the bookmarks page.
    Edit(i64),
    /// Changes the bookmarks without asking more.
    Apply(BookmarkEdit),
    /// Asks in the bookmarks page to delete a folder and its content.
    ConfirmDelete(i64),
    /// Opens the new folder editor of the bookmarks page.
    NewFolder,
    /// Opens the bookmarks page, on a folder when one is given.
    Manage(Option<i64>),
    SetVisibility(BarVisibility),
}

/// The widgets of the bookmarks bar of a window.
#[derive(Default)]
pub(crate) struct BookmarksBar {
    shown_tree: Option<Rc<Vec<BookmarkNode>>>,
    /// The decoded page icons of the shown tree, by item.
    textures: HashMap<i64, DecodedIcon>,
    /// The item being dragged, from the bar or from one of its menus.
    drag: Option<Drag>,
    /// Where the dragged item could land in each list drawn under the pointer this frame.
    drop_spots: Vec<DropSpot>,
}

impl BookmarksBar {
    /// Draws the bar, and returns what the user asked from it. A bookmark dragged onto the
    /// `location` bar opens.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        tree: &Rc<Vec<BookmarkNode>>,
        visibility: BarVisibility,
        location: Rect,
        icons: &mut ToolbarIconCache,
    ) -> Option<BookmarksBarAction> {
        if !self
            .shown_tree
            .as_ref()
            .is_some_and(|shown| Rc::ptr_eq(shown, tree))
        {
            // The icons that did not change stay decoded.
            let icons = icons_by_id(tree);
            self.textures
                .retain(|id, decoded| icons.get(id) == Some(&decoded.png.as_slice()));
            self.shown_tree = Some(tree.clone());
        }
        let mut action = None;
        self.drop_spots.clear();
        // The sense of the row is below its items, so it only gets the clicks between them.
        let rect = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), ITEM_HEIGHT));
        let row = UiBuilder::new()
            .max_rect(rect)
            .layout(Layout::left_to_right(Align::Center))
            .sense(Sense::click());
        let response = ui
            .scope_builder(row, |ui| {
                ui.set_min_size(rect.size());
                ui.spacing_mut().item_spacing.x = ITEM_SPACING;
                remove_hover_frame(ui);
                if tree.is_empty() {
                    ui.add(Label::new(RichText::new(EMPTY_HINT).weak()).selectable(false));
                    return;
                }
                let widths: Vec<_> = tree.iter().map(|node| item_width(ui, node)).collect();
                let visible = visible_items(
                    &widths,
                    ui.available_width(),
                    OVERFLOW_BUTTON_WIDTH,
                    ITEM_SPACING,
                );
                let nodes = &tree[..visible];
                let items: Vec<_> = nodes
                    .iter()
                    .map(|node| {
                        let response = self.bar_item(ui, node, visibility, icons, &mut action);
                        BarItem::new(node, response)
                    })
                    .collect();
                let list = DropList {
                    nodes,
                    items: &items,
                    parent: None,
                    first_index: 0,
                    vertical: false,
                    gap: ITEM_SPACING,
                };
                self.track_drag(ui, &list);
                let dragged = self.drag.as_ref().map(|drag| drag.id);
                let mut menu_openers: Vec<_> = items
                    .iter()
                    .filter(|item| item.is_folder && Some(item.id) != dragged)
                    .map(|item| (item.rect, Popup::default_response_id(&item.response)))
                    .collect();
                if visible < tree.len() {
                    ui.add_space((ui.available_width() - OVERFLOW_BUTTON_WIDTH).max(0.0));
                    let button =
                        match icons.sized_image(ui, ToolbarIcon::DoubleArrowRight, ICON_SIZE) {
                            Some(image) => Button::new(image).image_tint_follows_text_color(true),
                            None => Button::new("»"),
                        };
                    let button = button
                        .frame_when_inactive(false)
                        .corner_radius(CornerRadius::same(TOOLBAR_BUTTON_CORNER_RADIUS))
                        .min_size(vec2(OVERFLOW_BUTTON_WIDTH, ITEM_HEIGHT));
                    let (overflow, _) = MenuButton::from_button(button).ui(ui, |ui| {
                        self.menu_contents(ui, &tree[visible..], None, visible, icons, &mut action)
                    });
                    menu_openers.push((overflow.rect, Popup::default_response_id(&overflow)));
                }
                self.open_menu_under_drag(ui, &menu_openers);
            })
            .response;
        context_menu(&response, |ui| {
            bar_options(ui, None, visibility, &mut action)
        });
        if let Some(drop) = self.finish_drag(ui, location) {
            action = Some(drop);
        }
        action
    }

    fn bar_item(
        &mut self,
        ui: &mut egui::Ui,
        node: &BookmarkNode,
        visibility: BarVisibility,
        icons: &mut ToolbarIconCache,
        action: &mut Option<BookmarksBarAction>,
    ) -> egui::Response {
        let button = self
            .item_button(ui, node, icons, BAR_TITLE_LENGTH)
            .sense(Sense::click_and_drag());
        let id = node.id;
        let Some(url) = &node.url else {
            let (response, _) = MenuButton::from_button(button).ui(ui, |ui| {
                self.menu_contents(ui, &node.children, Some(id), 0, icons, action)
            });
            context_menu(&response, |ui| {
                if ui.button("Rename…").clicked() {
                    *action = Some(BookmarksBarAction::Edit(id));
                }
                if ui.button("Delete…").clicked() {
                    *action = Some(BookmarksBarAction::ConfirmDelete(id));
                }
                ui.separator();
                bar_options(ui, Some(id), visibility, action);
            });
            return response;
        };
        let response = ui.add(button).on_hover_text(url.as_str());
        record_click(ui, &response, url, action);
        context_menu(&response, |ui| {
            if ui.button("Open in New Tab").clicked() {
                *action = Some(BookmarksBarAction::OpenInNewTab(url.clone()));
            }
            if ui.button("Edit…").clicked() {
                *action = Some(BookmarksBarAction::Edit(id));
            }
            if ui.button("Delete").clicked() {
                *action = Some(BookmarksBarAction::Apply(BookmarkEdit::Delete { id }));
            }
            ui.separator();
            bar_options(ui, None, visibility, action);
        });
        response
    }

    /// The rows of a folder menu: `nodes` are the items of `parent` from `first_index`.
    fn menu_contents(
        &mut self,
        ui: &mut egui::Ui,
        nodes: &[BookmarkNode],
        parent: Option<i64>,
        first_index: usize,
        icons: &mut ToolbarIconCache,
        action: &mut Option<BookmarksBarAction>,
    ) {
        ui.set_min_width(MENU_MIN_WIDTH);
        remove_hover_frame(ui);
        if nodes.is_empty() {
            let empty = ui.add_enabled(false, Button::new("(Empty)"));
            if let Some(folder) = parent {
                self.track_drag_over_empty_folder(ui, folder, empty.rect);
            }
            return;
        }
        ScrollArea::vertical()
            .max_height(MENU_MAX_HEIGHT)
            .show(ui, |ui| {
                let items: Vec<_> = nodes
                    .iter()
                    .map(|node| {
                        let button = self
                            .item_button(ui, node, icons, MENU_TITLE_LENGTH)
                            .sense(Sense::click_and_drag());
                        let response = match &node.url {
                            Some(url) => {
                                let response = ui.add(button);
                                record_click(ui, &response, url, action);
                                response
                            },
                            None => {
                                let button = button.right_text(SubMenuButton::RIGHT_ARROW);
                                let (response, _) =
                                    SubMenuButton::from_button(button).ui(ui, |ui| {
                                        self.menu_contents(
                                            ui,
                                            &node.children,
                                            Some(node.id),
                                            0,
                                            icons,
                                            action,
                                        )
                                    });
                                response
                            },
                        };
                        BarItem::new(node, response)
                    })
                    .collect();
                let list = DropList {
                    nodes,
                    items: &items,
                    parent,
                    first_index,
                    vertical: true,
                    gap: ui.spacing().item_spacing.y,
                };
                self.track_drag(ui, &list);
            });
    }

    fn item_button(
        &mut self,
        ui: &egui::Ui,
        node: &BookmarkNode,
        icons: &mut ToolbarIconCache,
        title_length: usize,
    ) -> Button<'static> {
        let label = truncate_with_ellipsis(&display_name(node), title_length);
        let favicon = self.favicon(ui.ctx(), node);
        let tinted = favicon.is_none();
        let default_icon = if node.url.is_some() {
            ToolbarIcon::Globe
        } else {
            ToolbarIcon::Folder
        };
        let button = match favicon.or_else(|| icons.sized_image(ui, default_icon, ICON_SIZE)) {
            Some(image) => Button::new((image, label)),
            None => Button::new(label),
        };
        button
            .image_tint_follows_text_color(tinted)
            .frame_when_inactive(false)
            .corner_radius(CornerRadius::same(TOOLBAR_BUTTON_CORNER_RADIUS))
            .min_size(vec2(0.0, ITEM_HEIGHT))
    }

    fn favicon(&mut self, ctx: &egui::Context, node: &BookmarkNode) -> Option<Image<'static>> {
        let png = node.icon.as_ref()?;
        let texture = self
            .textures
            .entry(node.id)
            .or_insert_with(|| DecodedIcon {
                png: png.clone(),
                texture: decode_icon(ctx, node.id, png),
            })
            .texture
            .as_ref()?;
        Some(
            Image::from_texture(SizedTexture::new(texture.id(), Vec2::splat(ICON_SIZE)))
                .fit_to_exact_size(Vec2::splat(ICON_SIZE)),
        )
    }
}

/// A page icon decoded for the bar, with the PNG it comes from.
struct DecodedIcon {
    png: Vec<u8>,
    texture: Option<egui::TextureHandle>,
}

/// The PNG icons of `nodes` and of everything inside them, by item.
fn icons_by_id(nodes: &[BookmarkNode]) -> HashMap<i64, &[u8]> {
    let mut icons = HashMap::new();
    for node in nodes {
        if let Some(icon) = &node.icon {
            icons.insert(node.id, icon.as_slice());
        }
        icons.extend(icons_by_id(&node.children));
    }
    icons
}

/// The end of every context menu of the bar, about the bar itself.
fn bar_options(
    ui: &mut egui::Ui,
    folder: Option<i64>,
    visibility: BarVisibility,
    action: &mut Option<BookmarksBarAction>,
) {
    if ui.button("New Folder…").clicked() {
        *action = Some(BookmarksBarAction::NewFolder);
    }
    ui.menu_button("Bookmarks Bar", |ui| {
        for option in BarVisibility::ALL {
            if ui.radio(option == visibility, option.label()).clicked() {
                *action = Some(BookmarksBarAction::SetVisibility(option));
            }
        }
    });
    if ui.button("Manage Bookmarks").clicked() {
        *action = Some(BookmarksBarAction::Manage(folder));
    }
}

/// A folder also has a dropdown menu, which would share the default popup id of its response.
fn context_menu(response: &egui::Response, add_contents: impl FnOnce(&mut egui::Ui)) {
    Popup::context_menu(response)
        .id(response.id.with("bookmark_context_menu"))
        .show(add_contents);
}

fn decode_icon(ctx: &egui::Context, id: i64, png: &[u8]) -> Option<egui::TextureHandle> {
    let image = image::load_from_memory(png).ok()?.into_rgba8();
    let size = [image.width() as usize, image.height() as usize];
    Some(ctx.load_texture(
        format!("bookmark-icon-{id}"),
        egui::ColorImage::from_rgba_unmultiplied(size, image.as_raw()),
        TextureOptions::LINEAR,
    ))
}

fn display_name(node: &BookmarkNode) -> String {
    node.title
        .clone()
        .or_else(|| node.url.as_ref().map(Url::to_string))
        .unwrap_or_default()
}

fn record_click(
    ui: &egui::Ui,
    response: &egui::Response,
    url: &Url,
    action: &mut Option<BookmarksBarAction>,
) {
    if response.middle_clicked() || (response.clicked() && ui.input(|i| i.modifiers.command)) {
        *action = Some(BookmarksBarAction::OpenInNewTab(url.clone()));
    } else if response.clicked() {
        *action = Some(BookmarksBarAction::Open(url.clone()));
    }
}

/// The items only show a square on hover, like the toolbar buttons.
fn remove_hover_frame(ui: &mut egui::Ui) {
    let widgets = &mut ui.visuals_mut().widgets;
    for visuals in [&mut widgets.hovered, &mut widgets.active] {
        visuals.bg_stroke = Stroke::NONE;
        visuals.expansion = 0.0;
    }
}

/// The width of a bar item, computed before drawing it to know which items fit.
fn item_width(ui: &egui::Ui, node: &BookmarkNode) -> f32 {
    let label = truncate_with_ellipsis(&display_name(node), BAR_TITLE_LENGTH);
    let font = TextStyle::Button.resolve(ui.style());
    let text_width = ui
        .painter()
        .layout_no_wrap(label, font, Color32::PLACEHOLDER)
        .size()
        .x;
    text_width + ICON_SIZE + ui.spacing().icon_spacing + 2.0 * ui.spacing().button_padding.x
}

/// How many items fit in `available`, keeping room for the overflow button if some do not.
fn visible_items(widths: &[f32], available: f32, overflow_width: f32, spacing: f32) -> usize {
    let total: f32 = widths.iter().map(|width| width + spacing).sum();
    if total <= available {
        return widths.len();
    }
    let mut used = overflow_width + spacing;
    widths
        .iter()
        .take_while(|width| {
            used += **width + spacing;
            used <= available
        })
        .count()
}
