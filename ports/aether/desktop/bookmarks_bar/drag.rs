/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Dragging the items of the bookmarks bar and of its menus, to move them or to open them from
//! the location bar.

use std::collections::HashSet;
use std::time::Duration;

use aether_places::{BookmarkEdit, BookmarkNode};
use egui::{
    Area, CornerRadius, CursorIcon, Frame, Id, Label, LayerId, Order, PointerButton, Popup, Pos2,
    Rangef, Rect, Stroke, StrokeKind, TextWrapMode, pos2, vec2,
};
use url::Url;

use super::{BAR_TITLE_LENGTH, BookmarksBar, BookmarksBarAction, ITEM_HEIGHT, display_name};
use crate::desktop::gui::{LOCATION_CORNER_RADIUS, truncate_with_ellipsis};
use crate::desktop::icons::TOOLBAR_BUTTON_CORNER_RADIUS;

/// The width of the line that shows where a dragged item would land.
const DROP_MARKER_WIDTH: f32 = 2.0;

/// How long, in seconds, a dragged item stays over a folder of the bar before its menu opens.
const DRAG_OPEN_DELAY: f64 = 0.4;

impl BookmarksBar {
    /// Whether an item of the bar or of its menus is being dragged.
    pub(crate) fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Forgets the drag when the bar is no longer drawn, which would never finish it, so that the
    /// page gets the pointer back.
    pub(crate) fn cancel_drag(&mut self) {
        self.drag = None;
    }

    /// Starts a drag from `list`, and records where the dragged item would land in it.
    pub(super) fn track_drag(&mut self, ui: &egui::Ui, list: &DropList) {
        if let Some(position) = list
            .items
            .iter()
            .position(|item| item.response.drag_started_by(PointerButton::Primary))
        {
            let node = &list.nodes[position];
            self.drag = Some(Drag {
                id: node.id,
                url: node.url.clone(),
                name: display_name(node),
                parent: list.parent,
                index: list.first_index + position,
                itself_and_inside: ids_inside(node),
                hovered_menu: None,
            });
        }
        let Some(drag) = &self.drag else {
            return;
        };
        let Some(pointer) = ui.input(|input| input.pointer.latest_pos()) else {
            return;
        };
        if let Some((edit, marker)) = drop_spot(list, drag, pointer) {
            self.drop_spots.push(DropSpot {
                layer: ui.layer_id(),
                clip: ui.clip_rect(),
                edit,
                marker,
            });
        }
    }

    /// Records the row of the menu of an empty folder as a place for the dragged item.
    pub(super) fn track_drag_over_empty_folder(&mut self, ui: &egui::Ui, folder: i64, row: Rect) {
        let Some(drag) = &self.drag else {
            return;
        };
        let over_row = ui
            .input(|input| input.pointer.latest_pos())
            .is_some_and(|pointer| row.contains(pointer));
        if over_row && !drag.itself_and_inside.contains(&folder) {
            self.drop_spots.push(DropSpot {
                layer: ui.layer_id(),
                clip: ui.clip_rect(),
                edit: BookmarkEdit::Move {
                    id: drag.id,
                    parent: Some(folder),
                    index: 0,
                },
                marker: Marker::Outline(row),
            });
        }
    }

    /// Opens the menu of a folder of the bar, or the overflow menu, once the dragged item stays
    /// over it, as Firefox does.
    pub(super) fn open_menu_under_drag(&mut self, ui: &egui::Ui, menu_openers: &[(Rect, Id)]) {
        let Some(drag) = &mut self.drag else {
            return;
        };
        let pointer = ui.input(|input| input.pointer.latest_pos());
        let Some(&(_, popup)) = menu_openers
            .iter()
            .find(|(rect, _)| pointer.is_some_and(|pointer| rect.contains(pointer)))
        else {
            drag.hovered_menu = None;
            return;
        };
        let now = ui.input(|input| input.time);
        let since = match drag.hovered_menu {
            Some((hovered, since)) if hovered == popup => since,
            _ => {
                drag.hovered_menu = Some((popup, now));
                now
            },
        };
        let waited = now - since;
        if waited < DRAG_OPEN_DELAY {
            ui.ctx()
                .request_repaint_after(Duration::from_secs_f64(DRAG_OPEN_DELAY - waited));
        } else if !Popup::is_id_open(ui.ctx(), popup) {
            Popup::open_id(ui.ctx(), popup);
        }
    }

    /// Shows the dragged item and where it would land, and returns what its drop asks for.
    pub(super) fn finish_drag(
        &mut self,
        ui: &egui::Ui,
        location: Rect,
    ) -> Option<BookmarksBarAction> {
        let ctx = ui.ctx();
        let (pointer, released, down) = ctx.input(|input| {
            (
                input.pointer.latest_pos(),
                input.pointer.any_released(),
                input.pointer.any_down(),
            )
        });
        let top_layer = pointer.and_then(|pointer| ctx.layer_id_at(pointer));
        // As in other browsers, a bookmark dropped on the address bar opens.
        let over_location = self.drag.as_ref()?.url.is_some() &&
            top_layer.is_none_or(|layer| layer == ui.layer_id()) &&
            pointer.is_some_and(|pointer| location.contains(pointer));
        // Otherwise, the list of the topmost menu under the pointer, or else the bar.
        let layer = top_layer.unwrap_or(ui.layer_id());
        let spot = self
            .drop_spots
            .iter()
            .position(|spot| spot.layer == layer)
            .filter(|_| !over_location);
        if released || !down {
            let drag = self.drag.take()?;
            let action = if over_location {
                drag.url.map(BookmarksBarAction::Open)
            } else {
                spot.map(|spot| BookmarksBarAction::Apply(self.drop_spots.swap_remove(spot).edit))
            };
            if action.is_some() {
                Popup::close_all(ctx);
            }
            return action;
        }
        ctx.set_cursor_icon(CursorIcon::Grabbing);
        if let (Some(drag), Some(pointer)) = (&self.drag, pointer) {
            show_drag_label(ctx, &drag.name, pointer);
        }
        let stroke = Stroke::new(DROP_MARKER_WIDTH, ui.visuals().selection.bg_fill);
        if over_location {
            ctx.layer_painter(ui.layer_id()).rect_stroke(
                location,
                CornerRadius::same(LOCATION_CORNER_RADIUS),
                stroke,
                StrokeKind::Inside,
            );
        } else if let Some(spot) = spot {
            self.drop_spots[spot].paint(ctx, stroke);
        }
        None
    }
}

/// An item drawn in the bar or in a folder menu.
pub(super) struct BarItem {
    pub(super) id: i64,
    pub(super) rect: Rect,
    pub(super) is_folder: bool,
    pub(super) response: egui::Response,
}

impl BarItem {
    pub(super) fn new(node: &BookmarkNode, response: egui::Response) -> Self {
        Self {
            id: node.id,
            rect: response.rect,
            is_folder: node.url.is_none(),
            response,
        }
    }
}

/// Items of one folder, along the bar or down a menu, where a dragged item can land.
pub(super) struct DropList<'a> {
    pub(super) nodes: &'a [BookmarkNode],
    pub(super) items: &'a [BarItem],
    /// The folder of the items, or `None` for the bar.
    pub(super) parent: Option<i64>,
    /// The index of the first item in its folder, which the overflow menu does not start with.
    pub(super) first_index: usize,
    /// Whether the items go down, as in a menu, rather than along the bar.
    pub(super) vertical: bool,
    /// The space between two items, where the line of an insertion is drawn.
    pub(super) gap: f32,
}

impl DropList<'_> {
    /// The coordinate of `point` along the items.
    fn along(&self, point: Pos2) -> f32 {
        if self.vertical { point.y } else { point.x }
    }

    /// The range of `rect` along the items.
    fn range_along(&self, rect: Rect) -> Rangef {
        if self.vertical {
            rect.y_range()
        } else {
            rect.x_range()
        }
    }

    /// The range of `rect` across the items, and the coordinate of `point` across them.
    fn across(&self, rect: Rect, point: Pos2) -> (Rangef, f32) {
        if self.vertical {
            (rect.x_range(), point.x)
        } else {
            (rect.y_range(), point.y)
        }
    }

    /// The line where an item would be inserted before the item at `index`, or at the end.
    fn insertion_line(&self, index: usize) -> [Pos2; 2] {
        let position = match self.items.get(index) {
            Some(next) => self.range_along(next.rect).min - self.gap / 2.0,
            None => self.range_along(self.items[self.items.len() - 1].rect).max + self.gap / 2.0,
        };
        let (across, _) = self.across(self.items[0].rect, Pos2::ZERO);
        if self.vertical {
            [pos2(across.min, position), pos2(across.max, position)]
        } else {
            [pos2(position, across.min), pos2(position, across.max)]
        }
    }
}

/// An item being dragged.
pub(super) struct Drag {
    pub(super) id: i64,
    /// The address of a bookmark, or `None` for a folder.
    url: Option<Url>,
    name: String,
    /// The folder of the item and its index there, to know when a drop does not move it.
    parent: Option<i64>,
    index: usize,
    /// The item and everything inside it, which cannot receive it.
    itself_and_inside: HashSet<i64>,
    /// The menu of the bar the pointer is over, and since when, to open it.
    hovered_menu: Option<(Id, f64)>,
}

/// Where the dragged item would land in a list, and how to show it.
pub(super) struct DropSpot {
    layer: LayerId,
    clip: Rect,
    edit: BookmarkEdit,
    marker: Marker,
}

enum Marker {
    /// The folder that would receive the item.
    Outline(Rect),
    /// The line where the item would be inserted.
    Line([Pos2; 2]),
}

impl DropSpot {
    fn paint(&self, ctx: &egui::Context, stroke: Stroke) {
        let painter = ctx.layer_painter(self.layer).with_clip_rect(self.clip);
        match self.marker {
            Marker::Outline(rect) => {
                painter.rect_stroke(
                    rect,
                    CornerRadius::same(TOOLBAR_BUTTON_CORNER_RADIUS),
                    stroke,
                    StrokeKind::Inside,
                );
            },
            Marker::Line(points) => {
                painter.line_segment(points, stroke);
            },
        }
    }
}

/// The ids of `node` and of everything inside it.
fn ids_inside(node: &BookmarkNode) -> HashSet<i64> {
    let mut ids = HashSet::from([node.id]);
    for child in &node.children {
        ids.extend(ids_inside(child));
    }
    ids
}

/// Where the dragged item would land in `list` under `pointer`, if dropping it there moves it.
fn drop_spot(list: &DropList, drag: &Drag, pointer: Pos2) -> Option<(BookmarkEdit, Marker)> {
    let items = list.items;
    // The bar shows no item when the first one does not fit.
    let (first, last) = (items.first()?, items.last()?);
    let all = first.rect.union(last.rect);
    let (across, pointer_across) = list.across(all, pointer);
    if !across.expand(ITEM_HEIGHT / 2.0).contains(pointer_across) {
        return None;
    }
    let along = list.along(pointer);
    // Along the bar, the space after the last item is its end, but a menu stops at its rows.
    if list.vertical &&
        !list
            .range_along(all)
            .expand(ITEM_HEIGHT / 2.0)
            .contains(along)
    {
        return None;
    }
    if list
        .parent
        .is_some_and(|parent| drag.itself_and_inside.contains(&parent))
    {
        return None;
    }
    let folder = items.iter().find(|item| {
        let range = list.range_along(item.rect);
        let quarter = range.span() / 4.0;
        item.is_folder &&
            !drag.itself_and_inside.contains(&item.id) &&
            Rangef::new(range.min + quarter, range.max - quarter).contains(along)
    });
    if let Some(folder) = folder {
        let edit = BookmarkEdit::Move {
            id: drag.id,
            parent: Some(folder.id),
            index: usize::MAX,
        };
        return Some((edit, Marker::Outline(folder.rect)));
    }
    let index = items
        .iter()
        .filter(|item| list.range_along(item.rect).center() < along)
        .count();
    let position = list.first_index + index;
    let same_folder = list.parent == drag.parent;
    if same_folder && (position == drag.index || position == drag.index + 1) {
        return None;
    }
    let edit = BookmarkEdit::Move {
        id: drag.id,
        parent: list.parent,
        index: if same_folder && position > drag.index {
            position - 1
        } else {
            position
        },
    };
    Some((edit, Marker::Line(list.insertion_line(index))))
}

/// The name of the dragged item, next to the pointer.
fn show_drag_label(ctx: &egui::Context, name: &str, pointer: Pos2) {
    let name = truncate_with_ellipsis(name, BAR_TITLE_LENGTH);
    Area::new(Id::new("bookmark_drag_label"))
        .order(Order::Tooltip)
        .fixed_pos(pointer + vec2(12.0, 12.0))
        .interactable(false)
        .show(ctx, |ui| {
            Frame::popup(ui.style()).show(ui, |ui| {
                ui.add(Label::new(name).wrap_mode(TextWrapMode::Extend))
            });
        });
}

#[cfg(test)]
mod tests {
    use egui::pos2;

    use super::{Drag, DropList, drop_spot};

    #[test]
    fn a_bar_without_room_for_its_first_item_receives_no_drop() {
        // When the first item does not fit, all the items are in the overflow menu.
        let list = DropList {
            nodes: &[],
            items: &[],
            parent: None,
            first_index: 0,
            vertical: false,
            gap: 2.0,
        };
        let drag = Drag {
            id: 1,
            url: None,
            name: "alpha".to_owned(),
            parent: None,
            index: 0,
            itself_and_inside: [1].into(),
            hovered_menu: None,
        };

        assert!(drop_spot(&list, &drag, pos2(10.0, 10.0)).is_none());
    }
}
