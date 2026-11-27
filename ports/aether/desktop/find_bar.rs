/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use egui::{Key, TextEdit, WidgetInfo, WidgetType};

use crate::desktop::icons::{ToolbarIcon, ToolbarIconCache};

const BAR_HEIGHT: f32 = 28.0;
const FIELD_WIDTH: f32 = 220.0;

/// What the user did in the find bar on this frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FindBarCommand {
    /// The query text changed.
    QueryChanged,
    /// Next match (Enter, or the down arrow).
    Next,
    /// Previous match (Shift+Enter, or the up arrow).
    Previous,
    /// The bar was closed. The query is kept for the next open.
    Closed,
    /// The highlight-all checkbox was toggled.
    HighlightChanged,
}

/// Chrome find bar: query field, `active/total`, previous, next, close.
#[derive(Default)]
pub(crate) struct FindBar {
    open: bool,
    query: String,
    /// 1-based index of the active match. `0` when there is no match.
    active: usize,
    total: usize,
    focus_requested: bool,
    /// When set, every visible match is marked, not only the active one.
    highlight_all: bool,
    /// Bumped on every search so a late result cannot overwrite a newer one.
    generation: u64,
    /// Set when a closed bar is opened again with a query still in the field.
    pending_search: bool,
}

impl FindBar {
    pub(crate) fn open(&mut self) {
        let was_closed = !self.open;
        self.open = true;
        self.focus_requested = true;
        if was_closed && !self.query.is_empty() {
            self.pending_search = true;
        }
    }

    /// Drops the match count after the document changes. A following paint searches again
    /// when the bar is open and the query is not empty.
    pub(crate) fn invalidate_document(&mut self) {
        self.active = 0;
        self.total = 0;
        self.generation = self.generation.wrapping_add(1);
        self.pending_search = self.open && !self.query.is_empty();
    }

    pub(crate) fn is_open(&self) -> bool {
        self.open
    }

    pub(crate) fn query(&self) -> &str {
        &self.query
    }

    pub(crate) fn active(&self) -> usize {
        self.active
    }

    pub(crate) fn total(&self) -> usize {
        self.total
    }

    pub(crate) fn highlight_all(&self) -> bool {
        self.highlight_all
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.wrapping_add(1);
        self.generation
    }

    /// Updates the counter the bar displays. `active` is 1-based and `0` when `total` is `0`.
    pub(crate) fn set_matches(&mut self, active: usize, total: usize) {
        self.total = total;
        self.active = if total == 0 { 0 } else { active.clamp(1, total) };
    }

    /// Paints the bar along the bottom of the window when it is open.
    ///
    /// The field, the counter and the chevrons sit on the left. The close button sits on the right.
    /// The returned rect is the bar, in egui points.
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        icons: &mut ToolbarIconCache,
        field_id: egui::Id,
    ) -> (Option<egui::Rect>, Option<FindBarCommand>) {
        if !self.open {
            return (None, None);
        }

        let mut command = None;
        let fill = ui.visuals().window_fill;
        let inner = egui::Panel::bottom("find_bar")
            .frame(
                egui::Frame::default()
                    .fill(fill)
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show_inside(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(BAR_HEIGHT);
                    ui.spacing_mut().item_spacing.x = 4.0;

                    let field = ui.add_sized(
                        egui::vec2(FIELD_WIDTH, BAR_HEIGHT),
                        TextEdit::singleline(&mut self.query)
                            .id(field_id)
                            .hint_text("Find")
                            .vertical_align(egui::Align::Center),
                    );
                    field.widget_info(|| {
                        let mut info = WidgetInfo::new(WidgetType::TextEdit);
                        info.label = Some("Find".into());
                        info
                    });

                    if field.changed() {
                        self.active = 0;
                        self.total = 0;
                        command = Some(FindBarCommand::QueryChanged);
                    }
                    if std::mem::take(&mut self.focus_requested) {
                        field.request_focus();
                        select_all(ui.ctx(), field_id, &self.query);
                    }
                    let (escape, enter, shift) = ui.input(|input| {
                        (
                            input.key_pressed(Key::Escape),
                            input.key_pressed(Key::Enter),
                            input.modifiers.shift,
                        )
                    });
                    // This egui version has no public setter for `TextEdit`'s escape filter,
                    // and Escape clears the field's focus before `has_focus` is read.
                    // `lost_focus` is still true on that frame, so Escape closes the bar.
                    if escape && (field.has_focus() || field.lost_focus()) {
                        command = Some(FindBarCommand::Closed);
                    }
                    if field.lost_focus() && enter {
                        command = Some(if shift {
                            FindBarCommand::Previous
                        } else {
                            FindBarCommand::Next
                        });
                        field.request_focus();
                    }
                    if command.is_none() && std::mem::take(&mut self.pending_search) {
                        self.active = 0;
                        self.total = 0;
                        command = Some(FindBarCommand::QueryChanged);
                    }

                    let label = match_label(self.active, self.total, self.query.is_empty());
                    let count = ui.add(egui::Label::new(label).selectable(false));
                    if self.total == 0 && !self.query.is_empty() {
                        count.clone().on_hover_text("No matches");
                    }

                    if icon_button(ui, icons, ToolbarIcon::ChevronUp, "Previous match").clicked() {
                        command = Some(FindBarCommand::Previous);
                    }
                    if icon_button(ui, icons, ToolbarIcon::ChevronDown, "Next match").clicked() {
                        command = Some(FindBarCommand::Next);
                    }

                    let mut highlight_all = self.highlight_all;
                    let highlight = ui.checkbox(&mut highlight_all, "Highlight all");
                    highlight.widget_info(|| {
                        let mut info = WidgetInfo::new(WidgetType::Checkbox);
                        info.label = Some("Highlight all".into());
                        info.selected = Some(highlight_all);
                        info
                    });
                    if highlight.changed() {
                        self.highlight_all = highlight_all;
                        command = Some(FindBarCommand::HighlightChanged);
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if icon_button(ui, icons, ToolbarIcon::Close, "Close find").clicked() {
                            command = Some(FindBarCommand::Closed);
                        }
                    });
                });
            });

        if command == Some(FindBarCommand::Closed) {
            self.open = false;
        }
        (Some(inner.response.rect), command)
    }
}

/// The match to select after moving forward or backward. Wraps, and asks for the first
/// match when the total is not known yet.
pub(crate) fn step_match(active: usize, total: usize, forward: bool) -> usize {
    if total == 0 {
        return 1;
    }
    if forward {
        if active == 0 || active >= total {
            1
        } else {
            active + 1
        }
    } else if active <= 1 {
        total
    } else {
        active - 1
    }
}

/// Script that counts `query` in the visible page text and selects the 1-based `index` match.
///
/// `generation` is this search's token. The script records it on the page and does not paint
/// if a newer search has already taken that token. The current match is the page selection.
/// With "Highlight all", every other match is painted under the glyphs by the engine.
/// The result is `"active/total"`.
pub(crate) fn find_script(
    query: &str,
    index: usize,
    highlight_all: bool,
    generation: u64,
) -> String {
    let query = serde_json::to_string(query).unwrap_or_else(|_| "\"\"".into());
    let highlight_all = if highlight_all { "true" } else { "false" };
    format!(
        r#"(function() {{
  const query = {query};
  const want = {index};
  const highlightAll = {highlight_all};
  const generation = {generation};
  if (typeof window.__aetherFindGen === "number" && window.__aetherFindGen > generation) return "0/0";
  window.__aetherFindGen = generation;
  function stale() {{ return window.__aetherFindGen !== generation; }}
  const selection = window.getSelection && window.getSelection();
  function clearPaint() {{
    if (document.aetherSetFindHighlights) document.aetherSetFindHighlights([]);
    if (window.__aetherFindScroll) {{
      window.removeEventListener("scroll", window.__aetherFindScroll, true);
      window.removeEventListener("resize", window.__aetherFindScroll);
      window.__aetherFindScroll = null;
    }}
  }}
  if (!query || !document.body) {{
    if (!stale()) {{
      clearPaint();
      if (selection) selection.removeAllRanges();
    }}
    return "0/0";
  }}
  if (stale()) return "0/0";
  clearPaint();
  const needle = query.toLowerCase();
  const styleCache = new Map();
  function computed(el) {{
    if (styleCache.has(el)) return styleCache.get(el);
    let display = "";
    let visibility = "";
    let opacity = 1;
    let content = "";
    if (window.getComputedStyle) {{
      const style = window.getComputedStyle(el);
      display = style.getPropertyValue("display");
      visibility = style.getPropertyValue("visibility");
      const parsed = parseFloat(style.getPropertyValue("opacity"));
      opacity = isNaN(parsed) ? 1 : parsed;
      content = style.getPropertyValue("content-visibility");
    }}
    const info = {{ display: display, visibility: visibility, opacity: opacity, content: content }};
    styleCache.set(el, info);
    return info;
  }}
  function isBlockBox(el) {{
    if (!el || el === document.body) return true;
    const display = computed(el).display;
    return display !== "inline" && display !== "contents" && display !== "ruby";
  }}
  function hiddenByStyle(el) {{
    if (el.hidden) return true;
    const info = computed(el);
    return info.display === "none" || info.content === "hidden" || info.opacity === 0;
  }}
  function onScreen(el) {{
    if (!el) return false;
    for (let node = el; node; node = node.parentElement) {{
      const name = node.localName;
      if (name === "script" || name === "style" || name === "noscript" || name === "template" || name === "textarea") return false;
      if (hiddenByStyle(node)) return false;
    }}
    const visibility = computed(el).visibility;
    return visibility !== "hidden" && visibility !== "collapse";
  }}
  const textNodes = [];
  const textWalker = document.createTreeWalker(document.body, 4, null);
  let textNode;
  while ((textNode = textWalker.nextNode())) {{
    if (textNode.textContent && onScreen(textNode.parentElement)) textNodes.push(textNode);
  }}
  function blockOwner(el) {{
    for (let node = el; node; node = node.parentElement) {{
      if (isBlockBox(node)) return node;
    }}
    return document.body;
  }}
  const elementWalker = document.createTreeWalker(document.body, 1, null);
  function gapBreaks(from, to) {{
    elementWalker.currentNode = from;
    let next = elementWalker.nextNode();
    while (next) {{
      if (to.compareDocumentPosition(next) & 4) return false;
      if (hiddenByStyle(next)) {{
        next = elementWalker.nextNode();
        continue;
      }}
      if (next.localName === "br" || next.localName === "wbr" || isBlockBox(next)) return true;
      next = elementWalker.nextNode();
    }}
    return false;
  }}
  const runs = [];
  let run = null;
  for (let i = 0; i < textNodes.length; i++) {{
    const node = textNodes[i];
    const owner = blockOwner(node.parentElement);
    if (!run || run.owner !== owner || gapBreaks(run.last, node)) {{
      run = {{ owner: owner, parts: [], last: node }};
      runs.push(run);
    }}
    run.parts.push(node);
    run.last = node;
  }}
  function foldPart(text) {{
    let lower = "";
    const map = [];
    for (let i = 0; i < text.length; ) {{
      const ch = String.fromCodePoint(text.codePointAt(i));
      const folded = ch.toLowerCase();
      const origin = i;
      i += ch.length;
      lower += folded;
      for (let k = 0; k < folded.length; k++) map.push(origin);
    }}
    map.push(text.length);
    return {{ lower: lower, map: map }};
  }}
  function scrollMatch(range) {{
    let node = range.startContainer;
    if (!node) return;
    if (node.nodeType !== 1) node = node.parentElement;
    if (!node) return;
    if (node.scrollIntoView) node.scrollIntoView(true);
    const rect = node.getBoundingClientRect ? node.getBoundingClientRect() : null;
    if (!rect || (rect.top >= 0 && rect.bottom <= window.innerHeight)) return;
    const scroller = document.scrollingElement || document.documentElement;
    if (scroller) scroller.scrollTop += rect.top - 80;
  }}
  function locate(segs, combinedLength, index) {{
    if (!segs.length) return null;
    if (index >= combinedLength) {{
      const last = segs[segs.length - 1];
      return {{ node: last.node, offset: last.map[last.map.length - 1] }};
    }}
    let lo = 0;
    let hi = segs.length - 1;
    while (lo <= hi) {{
      const mid = (lo + hi) >> 1;
      const seg = segs[mid];
      if (index < seg.start) hi = mid - 1;
      else if (index >= seg.start + seg.length) lo = mid + 1;
      else return {{ node: seg.node, offset: seg.map[index - seg.start] }};
    }}
    return null;
  }}
  const hits = [];
  let total = 0;
  let chosen = null;
  let chosenIndex = 0;
  for (let r = 0; r < runs.length; r++) {{
    const parts = runs[r].parts;
    const segs = [];
    let combined = "";
    for (let p = 0; p < parts.length; p++) {{
      const text = parts[p].textContent || "";
      if (!text.length) continue;
      const folded = foldPart(text);
      segs.push({{ node: parts[p], start: combined.length, length: folded.lower.length, map: folded.map }});
      combined += folded.lower;
    }}
    let from = 0;
    while (from < combined.length) {{
      const at = combined.indexOf(needle, from);
      if (at < 0) break;
      const start = locate(segs, combined.length, at);
      const end = locate(segs, combined.length, at + needle.length);
      from = at + needle.length;
      if (!start || !end) continue;
      total += 1;
      const hit = {{
        startNode: start.node,
        startOffset: start.offset,
        endNode: end.node,
        endOffset: end.offset,
        index: total
      }};
      hits.push(hit);
      if (want < 1 ? total === 1 : total <= want) {{
        chosen = hit;
        chosenIndex = total;
      }}
    }}
  }}
  if (!stale()) {{
    try {{
      if (highlightAll && document.aetherSetFindHighlights && document.createRange) {{
        const ranges = [];
        for (let i = 0; i < hits.length; i++) {{
          const hit = hits[i];
          const range = document.createRange();
          range.setStart(hit.startNode, hit.startOffset);
          range.setEnd(hit.endNode, hit.endOffset);
          ranges.push(range);
        }}
        document.aetherSetFindHighlights(ranges);
      }}
      if (selection && document.createRange && chosen) {{
        const range = document.createRange();
        range.setStart(chosen.startNode, chosen.startOffset);
        range.setEnd(chosen.endNode, chosen.endOffset);
        selection.removeAllRanges();
        selection.addRange(range);
        scrollMatch(range);
      }}
    }} catch (error) {{}}
  }}
  return chosenIndex + "/" + total;
}})()"#
    )
}

pub(crate) fn parse_find_count(text: &str) -> Option<(usize, usize)> {
    let (active, total) = text.split_once('/')?;
    Some((active.parse().ok()?, total.parse().ok()?))
}

fn match_label(active: usize, total: usize, query_empty: bool) -> String {
    if query_empty {
        String::new()
    } else {
        format!("{active}/{total}")
    }
}

fn icon_button(
    ui: &mut egui::Ui,
    icons: &mut ToolbarIconCache,
    icon: ToolbarIcon,
    label: &str,
) -> egui::Response {
    let response = icons.button(ui, icon).on_hover_text(label);
    response.widget_info(|| {
        let mut info = WidgetInfo::new(WidgetType::Button);
        info.label = Some(label.into());
        info
    });
    response
}

fn select_all(ctx: &egui::Context, id: egui::Id, query: &str) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, id) {
        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
            egui::text::CCursor::new(0),
            egui::text::CCursor::new(query.len()),
        )));
        state.store(ctx, id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_wraps_around_the_matches() {
        assert_eq!(step_match(0, 0, true), 1);
        assert_eq!(step_match(1, 3, true), 2);
        assert_eq!(step_match(3, 3, true), 1);
        assert_eq!(step_match(1, 3, false), 3);
        assert_eq!(step_match(2, 3, false), 1);
    }

    #[test]
    fn the_script_result_is_active_over_total() {
        assert_eq!(parse_find_count("2/5"), Some((2, 5)));
        assert_eq!(parse_find_count("0/0"), Some((0, 0)));
        assert_eq!(parse_find_count("nope"), None);
    }

    #[test]
    fn the_counter_stays_blank_until_there_is_a_query() {
        assert_eq!(match_label(0, 0, true), "");
        assert_eq!(match_label(2, 5, false), "2/5");
        assert_eq!(match_label(0, 0, false), "0/0");
    }

    #[test]
    fn set_matches_clamps_the_active_index() {
        let mut bar = FindBar::default();
        bar.set_matches(9, 3);
        assert_eq!((bar.active, bar.total), (3, 3));
        bar.set_matches(1, 0);
        assert_eq!((bar.active, bar.total), (0, 0));
    }

    #[test]
    fn close_hides_the_bar_and_keeps_the_query() {
        let ctx = egui::Context::default();
        let mut icons = ToolbarIconCache::default();
        let mut bar = FindBar::default();
        bar.query = "servo".into();
        bar.open();
        let mut closed = false;
        let _ = ctx.run_ui(input(vec![]), |ui| {
            let (_, command) = bar.show(ui, &mut icons, egui::Id::new("find_bar_query"));
            let _ = ui.label("");
            closed = command == Some(FindBarCommand::Closed);
        });
        assert!(bar.is_open());
        assert!(!closed);

        let close = button_center(&ctx, "Close find");
        let _ = ctx.run_ui(input(click(close)), |ui| {
            let (_, command) = bar.show(ui, &mut icons, egui::Id::new("find_bar_query"));
            closed = command == Some(FindBarCommand::Closed);
        });
        assert!(closed);
        assert!(!bar.is_open());
        assert_eq!(bar.query(), "servo");
    }

    fn button_center(ctx: &egui::Context, label: &str) -> egui::Pos2 {
        ctx.memory(|memory| {
            memory
                .widgets()
                .iter()
                .find(|(_, widget)| widget.info.label.as_deref() == Some(label))
                .map(|(_, widget)| widget.interact_rect.center())
                .unwrap_or_else(|| panic!("no widget labeled {label}"))
        })
    }

    fn input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            ..Default::default()
        }
    }

    fn click(pos: egui::Pos2) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            },
            egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            },
        ]
    }
}
