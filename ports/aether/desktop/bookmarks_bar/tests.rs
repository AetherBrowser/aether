/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Tests of the bookmarks bar, drawn in egui and used with a simulated pointer.

use std::collections::HashMap;
use std::rc::Rc;

use aether_places::{BookmarkEdit, BookmarkNode};
use egui::{Modifiers, PointerButton, Pos2, pos2};
use url::Url;

use super::{BarVisibility, BookmarksBar, BookmarksBarAction, visible_items};
use crate::desktop::icons::ToolbarIconCache;

fn bookmark(id: i64, title: &str) -> BookmarkNode {
    BookmarkNode {
        id,
        url: Some(Url::parse(&format!("https://{title}.example/")).unwrap()),
        title: Some(title.to_owned()),
        icon: None,
        children: Vec::new(),
    }
}

fn folder(id: i64, title: &str, children: Vec<BookmarkNode>) -> BookmarkNode {
    BookmarkNode {
        id,
        url: None,
        title: Some(title.to_owned()),
        icon: None,
        children,
    }
}

fn pointer(pos: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers,
    }
}

/// Draws a bar in a 800x600 screen, and finds its widgets by their label.
struct Harness {
    ctx: egui::Context,
    bar: BookmarksBar,
    icons: ToolbarIconCache,
    tree: Rc<Vec<BookmarkNode>>,
    modifiers: Modifiers,
    action: Option<BookmarksBarAction>,
    widgets: HashMap<String, egui::Rect>,
    /// The time of the next frame, in seconds, a sixtieth of a second after the last one.
    time: f64,
}

impl Harness {
    fn new(tree: Vec<BookmarkNode>) -> Self {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut harness = Self {
            ctx,
            bar: BookmarksBar::default(),
            icons: ToolbarIconCache::default(),
            tree: Rc::new(tree),
            modifiers: Modifiers::NONE,
            action: None,
            widgets: HashMap::new(),
            time: 0.0,
        };
        harness.frame(vec![]);
        harness
    }

    fn frame(&mut self, events: Vec<egui::Event>) {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            modifiers: self.modifiers,
            time: Some(self.time),
            ..Default::default()
        };
        self.time += 1.0 / 60.0;
        let output = self.ctx.run_ui(input, |ui| {
            let action = self.bar.show(
                ui,
                &self.tree,
                BarVisibility::Always,
                LOCATION,
                &mut self.icons,
            );
            self.action = self.action.take().or(action);
        });
        let nodes = output
            .platform_output
            .accesskit_update
            .map(|update| update.nodes)
            .unwrap_or_default();
        for (_, node) in nodes {
            if let (Some(label), Some(bounds)) = (node.label(), node.bounds()) {
                let rect = egui::Rect::from_min_max(
                    pos2(bounds.x0 as f32, bounds.y0 as f32),
                    pos2(bounds.x1 as f32, bounds.y1 as f32),
                );
                self.widgets.insert(label.to_owned(), rect);
            }
        }
    }

    /// A press and a release at `pos`, in separate frames.
    fn click(&mut self, pos: Pos2, button: PointerButton) {
        let modifiers = self.modifiers;
        self.frame(vec![egui::Event::PointerMoved(pos)]);
        self.frame(vec![pointer(pos, button, true, modifiers)]);
        self.frame(vec![pointer(pos, button, false, modifiers)]);
    }

    /// A press at `from`, moves in several frames to `to`, and a release there.
    fn drag(&mut self, from: Pos2, to: Pos2) {
        self.press(from);
        self.move_pointer(from, to);
        self.release(to);
    }

    fn press(&mut self, at: Pos2) {
        let modifiers = self.modifiers;
        self.frame(vec![egui::Event::PointerMoved(at)]);
        self.frame(vec![pointer(at, PointerButton::Primary, true, modifiers)]);
    }

    fn move_pointer(&mut self, from: Pos2, to: Pos2) {
        for step in 1..=5 {
            self.frame(vec![egui::Event::PointerMoved(
                from.lerp(to, step as f32 / 5.0),
            )]);
        }
    }

    fn release(&mut self, at: Pos2) {
        let modifiers = self.modifiers;
        self.frame(vec![pointer(at, PointerButton::Primary, false, modifiers)]);
    }

    /// Frames without input during `seconds`, the pointer staying where it is.
    fn wait(&mut self, seconds: f64) {
        let end = self.time + seconds;
        while self.time < end {
            self.frame(vec![]);
        }
    }

    /// The center of the widget labelled `label`, once a menu that shows it is drawn.
    fn position_of(&mut self, label: &str) -> Pos2 {
        for _ in 0..3 {
            self.frame(vec![]);
            if let Some(rect) = self.widgets.get(label) {
                return rect.center();
            }
        }
        panic!("No widget labelled {label}: {:?}", self.widgets.keys());
    }

    fn is_menu_open(&mut self) -> bool {
        self.frame(vec![]);
        egui::Popup::is_any_open(&self.ctx)
    }
}

fn click(
    tree: Vec<BookmarkNode>,
    pos: Pos2,
    button: PointerButton,
    modifiers: Modifiers,
) -> Option<BookmarksBarAction> {
    let mut harness = Harness::new(tree);
    harness.modifiers = modifiers;
    harness.click(pos, button);
    harness.action
}

const FIRST_ITEM: Pos2 = pos2(30.0, 12.0);

/// Where the tests put the address bar, away from the bookmarks bar and its menus.
const LOCATION: egui::Rect = egui::Rect::from_min_max(pos2(100.0, 400.0), pos2(500.0, 428.0));

/// Between the items and the right end of the bar.
const EMPTY_PART: Pos2 = pos2(400.0, 12.0);

#[test]
fn a_click_opens_the_bookmark_in_the_current_tab() {
    assert_eq!(
        click(
            vec![bookmark(1, "servo")],
            FIRST_ITEM,
            PointerButton::Primary,
            Modifiers::NONE
        ),
        Some(BookmarksBarAction::Open(
            Url::parse("https://servo.example/").unwrap()
        ))
    );
}

#[test]
fn a_middle_click_or_a_command_click_opens_a_new_tab() {
    let new_tab = Some(BookmarksBarAction::OpenInNewTab(
        Url::parse("https://servo.example/").unwrap(),
    ));

    assert_eq!(
        click(
            vec![bookmark(1, "servo")],
            FIRST_ITEM,
            PointerButton::Middle,
            Modifiers::NONE
        ),
        new_tab
    );
    assert_eq!(
        click(
            vec![bookmark(1, "servo")],
            FIRST_ITEM,
            PointerButton::Primary,
            Modifiers::COMMAND
        ),
        new_tab
    );
}

#[test]
fn clicking_a_folder_asks_for_nothing_and_opens_its_menu() {
    let mut harness = Harness::new(vec![folder(1, "Rust", vec![bookmark(2, "inside")])]);

    harness.click(FIRST_ITEM, PointerButton::Primary);

    assert_eq!(harness.action, None);
    assert!(harness.is_menu_open());
}

#[test]
fn a_bookmark_of_a_folder_menu_can_be_opened() {
    let mut harness = Harness::new(vec![folder(1, "Rust", vec![bookmark(2, "inside")])]);
    harness.click(FIRST_ITEM, PointerButton::Primary);

    let inside = harness.position_of("inside");
    harness.click(inside, PointerButton::Primary);

    assert_eq!(
        harness.action,
        Some(BookmarksBarAction::Open(
            Url::parse("https://inside.example/").unwrap()
        ))
    );
}

#[test]
fn an_empty_bar_does_nothing_when_clicked() {
    assert_eq!(
        click(
            Vec::new(),
            FIRST_ITEM,
            PointerButton::Primary,
            Modifiers::NONE
        ),
        None
    );
}

#[test]
fn the_visibility_is_chosen_from_the_menu_of_the_bar() {
    let mut harness = Harness::new(vec![bookmark(1, "servo")]);
    harness.click(EMPTY_PART, PointerButton::Secondary);

    let submenu = harness.position_of("Bookmarks Bar ⏵");
    harness.frame(vec![egui::Event::PointerMoved(submenu)]);
    let never = harness.position_of("Never Show");
    harness.click(never, PointerButton::Primary);

    assert_eq!(
        harness.action,
        Some(BookmarksBarAction::SetVisibility(BarVisibility::Never))
    );
    assert!(!harness.is_menu_open());
}

#[test]
fn an_empty_bar_has_the_menu_of_the_bar() {
    let mut harness = Harness::new(Vec::new());
    harness.click(FIRST_ITEM, PointerButton::Secondary);

    let manage = harness.position_of("Manage Bookmarks");
    harness.click(manage, PointerButton::Primary);

    assert_eq!(harness.action, Some(BookmarksBarAction::Manage(None)));
}

#[test]
fn the_menu_of_a_folder_deletes_it_after_asking() {
    let mut harness = Harness::new(vec![folder(7, "Rust", Vec::new())]);
    harness.click(FIRST_ITEM, PointerButton::Secondary);

    let delete = harness.position_of("Delete…");
    harness.click(delete, PointerButton::Primary);

    assert_eq!(harness.action, Some(BookmarksBarAction::ConfirmDelete(7)));
}

#[test]
fn the_menu_of_a_folder_manages_that_folder() {
    let mut harness = Harness::new(vec![folder(7, "Rust", Vec::new())]);
    harness.click(FIRST_ITEM, PointerButton::Secondary);

    let manage = harness.position_of("Manage Bookmarks");
    harness.click(manage, PointerButton::Primary);

    assert_eq!(harness.action, Some(BookmarksBarAction::Manage(Some(7))));
}

#[test]
fn the_overflow_button_is_at_the_right_end_of_the_bar() {
    let tree = (1..=8)
        .map(|id| bookmark(id, "a-long-bookmark-title"))
        .collect();
    let mut harness = Harness::new(tree);

    harness.click(pos2(788.0, 12.0), PointerButton::Primary);

    assert!(harness.is_menu_open());
}

fn moved(id: i64, parent: Option<i64>, index: usize) -> Option<BookmarksBarAction> {
    Some(BookmarksBarAction::Apply(BookmarkEdit::Move {
        id,
        parent,
        index,
    }))
}

fn three_bookmarks() -> Vec<BookmarkNode> {
    vec![
        bookmark(1, "alpha"),
        bookmark(2, "beta"),
        bookmark(3, "gamma"),
    ]
}

#[test]
fn a_bookmark_is_dragged_along_the_bar() {
    let mut harness = Harness::new(three_bookmarks());
    let alpha = harness.position_of("alpha");
    let gamma = harness.widgets["gamma"];

    harness.drag(alpha, gamma.right_center() + egui::vec2(-2.0, 0.0));

    assert_eq!(harness.action, moved(1, None, 2));
}

#[test]
fn a_bookmark_is_dragged_to_the_start_of_the_bar() {
    let mut harness = Harness::new(three_bookmarks());
    let gamma = harness.position_of("gamma");
    let alpha = harness.widgets["alpha"];

    harness.drag(gamma, alpha.left_center() + egui::vec2(2.0, 0.0));

    assert_eq!(harness.action, moved(3, None, 0));
}

#[test]
fn a_bookmark_dropped_on_a_folder_goes_inside() {
    let mut harness = Harness::new(vec![bookmark(1, "alpha"), folder(2, "Rust", Vec::new())]);
    let alpha = harness.position_of("alpha");
    let rust = harness.position_of("Rust");

    harness.drag(alpha, rust);

    assert_eq!(harness.action, moved(1, Some(2), usize::MAX));
}

#[test]
fn a_drop_where_the_item_already_is_changes_nothing() {
    let mut harness = Harness::new(three_bookmarks());
    let beta = harness.widgets["beta"];

    harness.drag(beta.center(), beta.left_center() + egui::vec2(4.0, 0.0));

    assert_eq!(harness.action, None);
}

#[test]
fn a_drop_outside_the_bar_changes_nothing() {
    let mut harness = Harness::new(three_bookmarks());
    let alpha = harness.position_of("alpha");

    harness.drag(alpha, pos2(400.0, 300.0));

    assert_eq!(harness.action, None);
}

fn folder_of_three() -> Vec<BookmarkNode> {
    vec![folder(
        10,
        "Rust",
        vec![
            bookmark(1, "alpha"),
            bookmark(2, "beta"),
            folder(3, "Async", Vec::new()),
        ],
    )]
}

#[test]
fn a_bookmark_is_dragged_down_a_folder_menu() {
    let mut harness = Harness::new(folder_of_three());
    harness.click(FIRST_ITEM, PointerButton::Primary);
    let alpha = harness.position_of("alpha");
    let beta = harness.widgets["beta"];

    harness.drag(alpha, beta.center_bottom() - egui::vec2(0.0, 2.0));

    assert_eq!(harness.action, moved(1, Some(10), 1));
}

#[test]
fn a_bookmark_dropped_on_a_folder_of_a_menu_goes_inside() {
    let mut harness = Harness::new(folder_of_three());
    harness.click(FIRST_ITEM, PointerButton::Primary);
    let alpha = harness.position_of("alpha");
    let subfolder = harness.position_of("Async ⏵");

    harness.drag(alpha, subfolder);

    assert_eq!(harness.action, moved(1, Some(3), usize::MAX));
}

#[test]
fn a_drop_far_from_the_bar_and_its_menus_changes_nothing() {
    let mut harness = Harness::new(folder_of_three());
    harness.click(FIRST_ITEM, PointerButton::Primary);
    let beta = harness.position_of("beta");

    harness.drag(beta, pos2(700.0, 500.0));

    assert_eq!(harness.action, None);
}

#[test]
fn a_bookmark_of_a_menu_is_dragged_onto_the_bar() {
    let mut harness = Harness::new(folder_of_three());
    harness.click(FIRST_ITEM, PointerButton::Primary);
    let beta = harness.position_of("beta");

    harness.drag(beta, pos2(beta.x, 12.0));

    assert_eq!(harness.action, moved(2, None, 1));
}

#[test]
fn a_bookmark_is_dragged_into_a_submenu_that_opens_under_it() {
    let tree = vec![folder(
        10,
        "Rust",
        vec![
            bookmark(1, "alpha"),
            folder(3, "Async", vec![bookmark(4, "gamma"), bookmark(5, "delta")]),
        ],
    )];
    let mut harness = Harness::new(tree);
    harness.click(FIRST_ITEM, PointerButton::Primary);
    let alpha = harness.position_of("alpha");
    let submenu = harness.position_of("Async ⏵");

    harness.press(alpha);
    harness.move_pointer(alpha, submenu);
    let gamma = harness.position_of("gamma");
    harness.move_pointer(submenu, gamma);
    let below_gamma = harness.widgets["gamma"].center_bottom() - egui::vec2(0.0, 2.0);
    harness.move_pointer(gamma, below_gamma);
    harness.release(below_gamma);

    assert_eq!(harness.action, moved(1, Some(3), 1));
}

#[test]
fn a_folder_of_the_bar_opens_its_menu_under_a_dragged_bookmark() {
    let tree = vec![
        bookmark(1, "alpha"),
        folder(10, "Rust", vec![bookmark(2, "beta"), bookmark(3, "gamma")]),
    ];
    let mut harness = Harness::new(tree);
    let alpha = harness.position_of("alpha");
    let rust = harness.position_of("Rust");

    harness.press(alpha);
    harness.move_pointer(alpha, rust);
    assert!(!harness.widgets.contains_key("beta"));
    harness.wait(0.5);
    let beta = harness.position_of("beta");
    let below_beta = harness.widgets["beta"].center_bottom() - egui::vec2(0.0, 2.0);
    harness.move_pointer(rust, beta);
    harness.move_pointer(beta, below_beta);
    harness.release(below_beta);

    assert_eq!(harness.action, moved(1, Some(10), 1));
    assert!(!harness.is_menu_open());
}

#[test]
fn a_bookmark_dropped_on_the_address_bar_opens() {
    let mut harness = Harness::new(three_bookmarks());
    let beta = harness.position_of("beta");

    harness.drag(beta, LOCATION.center());

    assert_eq!(
        harness.action,
        Some(BookmarksBarAction::Open(
            Url::parse("https://beta.example/").unwrap()
        ))
    );
}

#[test]
fn a_folder_dropped_on_the_address_bar_does_nothing() {
    let mut harness = Harness::new(folder_of_three());

    harness.drag(FIRST_ITEM, LOCATION.center());

    assert_eq!(harness.action, None);
}

fn png(width: u32) -> Vec<u8> {
    let mut png = Vec::new();
    image::RgbaImage::new(width, width)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

#[test]
fn a_new_tree_only_decodes_the_icons_that_changed() {
    let with_icon = |icon: Vec<u8>| BookmarkNode {
        icon: Some(icon),
        ..bookmark(1, "alpha")
    };
    let mut harness = Harness::new(vec![with_icon(png(2)), bookmark(2, "beta")]);
    let texture = |harness: &Harness| harness.bar.textures[&1].texture.as_ref().unwrap().id();
    let first = texture(&harness);

    harness.tree = Rc::new(vec![with_icon(png(2)), bookmark(3, "gamma")]);
    harness.frame(vec![]);
    assert_eq!(texture(&harness), first);

    harness.tree = Rc::new(vec![with_icon(png(4))]);
    harness.frame(vec![]);
    assert_ne!(texture(&harness), first);
}

#[test]
fn only_the_primary_button_drags_an_item() {
    for button in [PointerButton::Middle, PointerButton::Secondary] {
        let mut harness = Harness::new(three_bookmarks());
        let alpha = harness.position_of("alpha");
        let after_gamma = harness.widgets["gamma"].right_center() + egui::vec2(-2.0, 0.0);

        harness.frame(vec![egui::Event::PointerMoved(alpha)]);
        harness.frame(vec![pointer(alpha, button, true, Modifiers::NONE)]);
        harness.move_pointer(alpha, after_gamma);
        harness.frame(vec![pointer(after_gamma, button, false, Modifiers::NONE)]);

        assert_eq!(harness.action, None, "{button:?}");
    }
}

#[test]
fn a_folder_cannot_be_dropped_into_itself() {
    let tree = vec![folder(
        10,
        "Rust",
        vec![bookmark(1, "alpha"), folder(3, "Async", Vec::new())],
    )];
    let mut harness = Harness::new(tree);
    harness.click(FIRST_ITEM, PointerButton::Primary);
    let alpha = harness.position_of("alpha");
    let submenu = harness.position_of("Async ⏵");

    harness.press(submenu);
    harness.move_pointer(submenu, alpha);
    harness.move_pointer(alpha, submenu);
    let empty = harness.position_of("(Empty)");
    harness.move_pointer(submenu, empty);
    harness.release(empty);

    assert_eq!(harness.action, None);
}

#[test]
fn the_overflow_menu_moves_its_items_after_those_of_the_bar() {
    let label = |id: i64| format!("bookmark-number-{id}-of-the-bar");
    let mut harness = Harness::new((1..=8).map(|id| bookmark(id, &label(id))).collect());
    harness.click(pos2(788.0, 12.0), PointerButton::Primary);
    harness.frame(vec![]);
    // The bar shortens these titles, so only the rows of the menu show them whole.
    let in_menu: Vec<i64> = (1..=8)
        .filter(|id| harness.widgets.contains_key(&label(*id)))
        .collect();
    assert!(
        in_menu.len() >= 2 && in_menu.last() == Some(&8),
        "{in_menu:?}"
    );
    let (first, second) = (in_menu[0], in_menu[1]);
    let from = harness.position_of(&label(first));
    let to = harness.widgets[&label(second)].center_bottom() - egui::vec2(0.0, 2.0);

    harness.drag(from, to);

    assert_eq!(harness.action, moved(first, None, second as usize - 1));
}

#[test]
fn all_items_are_shown_when_they_fit() {
    assert_eq!(visible_items(&[100.0, 100.0], 204.0, 24.0, 2.0), 2);
}

#[test]
fn items_that_do_not_fit_leave_room_for_the_overflow_button() {
    assert_eq!(visible_items(&[100.0, 100.0, 100.0], 230.0, 24.0, 2.0), 2);
    assert_eq!(visible_items(&[100.0, 100.0, 100.0], 228.0, 24.0, 2.0), 1);
    assert_eq!(visible_items(&[100.0], 50.0, 24.0, 2.0), 0);
}
