/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

use std::collections::HashMap;
#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos"
))]
use std::fs;
#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos"
))]
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use accesskit::Affine;
use dpi::PhysicalSize;
use egui::text::{CCursor, CCursorRange};
use egui::text_edit::TextEditState;
use egui::{
    Button, FontDefinitions, Id, Key, Label, LayerId, Modifiers, Order, PaintCallback, Panel,
    WidgetInfo, WidgetType, pos2,
};
#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos"
))]
use egui::{FontData, FontFamily};
use egui_glow::{CallbackFn, EguiGlow};
use egui_winit::EventResponse;
use euclid::{Length, Point2D, Rect, Scale, Size2D};
#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos"
))]
use log::info;
use log::warn;
use servo::{
    DeviceIndependentPixel, DevicePixel, Image, LoadStatus, OffscreenRenderingContext, PixelFormat,
    RenderingContext, WebView, WebViewId,
};
use url::Url;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::window::Window;

use crate::desktop::event_loop::AppEvent;
use crate::desktop::headed_window;
use crate::desktop::icons::{ToolbarIcon, ToolbarIconCache};
use crate::desktop::menu::{AppMenu, AppMenuAction};
use crate::running_app_state::{RunningAppState, UserInterfaceCommand};
use crate::window::{
    ServoShellWindow, TopLevelWebViewCreationRequest, history_url, new_tab_url, processes_url,
    settings_url,
};

/// The user interface of a headed servoshell. Currently this is implemented via
/// egui.
pub struct Gui {
    rendering_context: Rc<OffscreenRenderingContext>,
    context: EguiGlow,
    toolbar_height: Length<f32, DeviceIndependentPixel>,

    location: String,

    /// Whether the location has been edited by the user without clicking Go.
    location_dirty: bool,

    /// The [`LoadStatus`] of the active `WebView`.
    load_status: LoadStatus,

    /// The text to display in the status bar on the bottom of the window.
    status_text: Option<String>,

    /// Whether or not the current `WebView` can navigate backward.
    can_go_back: bool,

    /// Whether or not the current `WebView` can navigate forward.
    can_go_forward: bool,

    /// Handle to the GPU texture of the favicon.
    ///
    /// These need to be cached across egui draw calls.
    favicon_textures: HashMap<WebViewId, (egui::TextureHandle, egui::load::SizedTexture)>,

    /// Rasterized SVG toolbar icons, cached across frames.
    toolbar_icons: ToolbarIconCache,

    /// Hamburger menu opened from the toolbar.
    app_menu: AppMenu,

    /// Whether the docked developer tools panel is open beside the page.
    devtools_open: bool,

    /// DevTools state and event receiver (owned by the UI thread).
    devtools_panel: crate::devtools::DevToolsPanel,

    /// Timestamp of the previous frame, used for frame-time telemetry.
    last_frame_time: std::time::Instant,

    /// Screen rect of the docked developer tools panel, in device-independent pixels.
    ///
    /// Empty when the panel is closed. Pointer events inside this rect stay in egui
    /// so the close button works and the page does not receive them.
    devtools_rect: egui::Rect,

    /// AccessKit tree updates pending the next egui tick.
    /// This allows us to ensure that graft nodes are sent before the subtrees they graft.
    pending_accesskit_updates: Vec<accesskit::TreeUpdate>,
}

fn truncate_with_ellipsis(input: &str, max_length: usize) -> String {
    if input.chars().count() > max_length {
        let truncated: String = input.chars().take(max_length.saturating_sub(1)).collect();
        format!("{}…", truncated)
    } else {
        input.to_string()
    }
}

#[cfg(any(
    target_os = "windows",
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos"
))]
fn load_cjk_fonts(font_candidates: &[(&str, &str)]) -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let mut loaded_font_names = Vec::new();

    for (path_str, font_name) in font_candidates.iter() {
        let font_path = Path::new(path_str);
        if font_path.exists() {
            match fs::read(font_path) {
                Ok(bytes) => {
                    if !fonts.font_data.contains_key(*font_name) {
                        fonts
                            .font_data
                            .insert(font_name.to_string(), Arc::new(FontData::from_owned(bytes)));
                        loaded_font_names.push(font_name.to_string());
                        info!("Loaded font: {}", font_name);
                    }
                },
                Err(error) => {
                    info!("Failed to read font {}: {}", font_name, error);
                },
            }
        }
    }

    if !loaded_font_names.is_empty() {
        let proportional = fonts.families.get_mut(&FontFamily::Proportional).unwrap();
        for font_name in loaded_font_names.iter() {
            proportional.insert(0, font_name.clone());
        }
    }

    fonts
}

#[cfg(target_os = "windows")]
fn configure_fonts() -> FontDefinitions {
    load_cjk_fonts(&[
        (r"C:\Windows\Fonts\malgun.ttf", "Malgun Gothic"), // Korean
        (r"C:\Windows\Fonts\msyh.ttc", "Microsoft YaHei"), // Chinese + Japanese
    ])
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn configure_fonts() -> FontDefinitions {
    load_cjk_fonts(&[
        (
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "Noto Sans CJK",
        ), // Ubuntu/Debian
        (
            "/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc",
            "Noto Sans CJK",
        ), // Fedora/Arch
        // FreeBSD splits the Noto CJK fonts into regional subsets
        (
            "/usr/local/share/fonts/noto/NotoSansCJKhk-Regular.otf",
            "Noto Sans CJK HK",
        ),
        (
            "/usr/local/share/fonts/noto/NotoSansCJKjp-Regular.otf",
            "Noto Sans CJK JP",
        ),
        (
            "/usr/local/share/fonts/noto/NotoSansCJKkr-Regular.otf",
            "Noto Sans CJK KR",
        ),
        (
            "/usr/local/share/fonts/noto/NotoSansCJKsc-Regular.otf",
            "Noto Sans CJK SC",
        ),
        (
            "/usr/local/share/fonts/noto/NotoSansCJKtc-Regular.otf",
            "Noto Sans CJK TC",
        ),
        (
            "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
            "WenQuanYi Micro Hei",
        ), // common fallback
        (
            "/usr/local/share/fonts/wqy/wqy-microhei.ttc",
            "WenQuanYi Micro Hei",
        ), // FreeBSD
    ])
}

#[cfg(target_os = "macos")]
fn configure_fonts() -> FontDefinitions {
    load_cjk_fonts(&[
        (
            "/System/Library/Fonts/AppleSDGothicNeo.ttc",
            "Apple SD Gothic Neo",
        ), // Korean
        ("/System/Library/Fonts/STHeiti Medium.ttc", "STHeiti Medium"), // Chinese + Japanese
    ])
}

/// Right-hand developer tools dock produced by [`Gui::show_devtools_dock`].
struct DevtoolsDock {
    close_clicked: bool,
    panel_rect: egui::Rect,
    /// Close button bounds. Read by tests that click it.
    #[cfg_attr(not(test), allow(dead_code))]
    close_button_rect: egui::Rect,
}

/// Toolbar, open application menu, or the docked developer tools panel.
fn pointer_hits_browser_chrome(
    toolbar_height: f32,
    menu_contains_pointer: bool,
    devtools_rect: egui::Rect,
    position: Point2D<f32, DeviceIndependentPixel>,
) -> bool {
    position.y < toolbar_height ||
        menu_contains_pointer ||
        devtools_rect
            .expand(8.0)
            .contains(egui::pos2(position.x, position.y))
}

impl Drop for Gui {
    fn drop(&mut self) {
        self.rendering_context
            .make_current()
            .expect("Could not make window RenderingContext current");
        self.context.destroy();
    }
}

impl Gui {
    pub(crate) fn new(
        winit_window: &Window,
        event_loop: &ActiveEventLoop,
        event_loop_proxy: EventLoopProxy<AppEvent>,
        rendering_context: Rc<OffscreenRenderingContext>,
        initial_url: Url,
    ) -> Self {
        rendering_context
            .make_current()
            .expect("Could not make window RenderingContext current");
        let mut context = EguiGlow::new(
            event_loop,
            rendering_context.glow_gl_api(),
            None,
            None,
            false,
        );

        let font_definitions = configure_fonts();
        context.egui_ctx.set_fonts(font_definitions);

        context
            .egui_winit
            .init_accesskit(event_loop, winit_window, event_loop_proxy);
        winit_window.set_visible(true);

        context.egui_ctx.options_mut(|options| {
            // Disable the builtin egui handlers for the Ctrl+Plus, Ctrl+Minus and Ctrl+0
            // shortcuts as they don't work well with servoshell's `device-pixel-ratio` CLI argument.
            options.zoom_with_keyboard = false;

            // On platforms where winit fails to obtain a system theme, fall back to a light theme
            // since it is the more common default.
            options.fallback_theme = egui::Theme::Light;
        });

        Self {
            rendering_context,
            context,
            toolbar_height: Default::default(),
            location: initial_url.to_string(),
            location_dirty: false,
            load_status: LoadStatus::Complete,
            status_text: None,
            can_go_back: false,
            can_go_forward: false,
            favicon_textures: Default::default(),
            toolbar_icons: Default::default(),
            app_menu: Default::default(),
            devtools_open: false,
            devtools_panel: crate::devtools::DevToolsPanel::new(),
            last_frame_time: std::time::Instant::now(),
            devtools_rect: egui::Rect::NOTHING,
            pending_accesskit_updates: vec![],
        }
    }

    pub(crate) fn has_keyboard_focus(&self) -> bool {
        self.context
            .egui_ctx
            .memory(|memory| memory.focused().is_some())
    }

    pub(crate) fn surrender_focus(&self) {
        self.context.egui_ctx.memory_mut(|memory| {
            if let Some(focused) = memory.focused() {
                memory.surrender_focus(focused);
            }
        });
    }

    pub(crate) fn on_window_event(
        &mut self,
        winit_window: &Window,
        event: &WindowEvent,
    ) -> EventResponse {
        self.context.on_window_event(winit_window, event)
    }

    /// The height of the top toolbar of this user inteface ie the distance from the top of the
    /// window to the position of the `WebView`.
    pub(crate) fn toolbar_height(&self) -> Length<f32, DeviceIndependentPixel> {
        self.toolbar_height
    }

    /// Return true iff the given position is over the egui toolbar, the open app menu,
    /// or the docked developer tools panel.
    pub(crate) fn is_in_egui_toolbar_rect(
        &self,
        position: Point2D<f32, DeviceIndependentPixel>,
    ) -> bool {
        // While actively dragging the DevTools resize splitter, keep pointer
        // capture even if the cursor drifts outside the panel rect.
        if self.devtools_open && self.context.egui_ctx.egui_is_using_pointer() {
            return true;
        }
        pointer_hits_browser_chrome(
            self.toolbar_height.get(),
            self.app_menu.contains_pointer(position),
            self.devtools_rect,
            position,
        )
    }

    pub(crate) fn is_app_menu_open(&self) -> bool {
        self.app_menu.is_open()
    }

    /// Toggle the docked developer tools panel open or closed.
    pub(crate) fn toggle_devtools(&mut self) {
        let open = !self.devtools_open;
        self.set_devtools_open(open);
    }

    /// Open or close the DevTools panel with proper state reset.
    pub(crate) fn set_devtools_open(&mut self, open: bool) {
        self.devtools_open = open;
        if open {
            self.last_frame_time = std::time::Instant::now();
            self.devtools_panel.request_dom_tree();
            self.devtools_panel.refresh_storage();
            self.devtools_panel.request_page_timing();
        } else {
            self.devtools_panel.clear_highlight();
            self.devtools_rect = egui::Rect::NOTHING;
            self.surrender_focus();
        }
    }

    /// Whether the pointer is over the open application menu, which overlays the page.
    pub(crate) fn app_menu_contains_pointer(
        &self,
        position: Point2D<f32, DeviceIndependentPixel>,
    ) -> bool {
        self.app_menu.contains_pointer(position)
    }

    /// Draws a browser tab, checking for clicks and queues appropriate [`UserInterfaceCommand`]s.
    /// Using a custom widget here would've been nice, but it doesn't seem as though egui
    /// supports that, so we arrange multiple Widgets in a way that they look connected.
    fn browser_tab(
        ui: &mut egui::Ui,
        toolbar_icons: &mut ToolbarIconCache,
        window: &ServoShellWindow,
        webview: WebView,
        favicon_texture: Option<egui::load::SizedTexture>,
    ) {
        let label = match (webview.page_title(), webview.url()) {
            (Some(title), _) if !title.is_empty() => title,
            (_, Some(url)) => url.to_string(),
            _ => "New Tab".into(),
        };

        let inactive_bg_color = ui.visuals().window_fill;
        let active_bg_color = ui.visuals().widgets.active.weak_bg_fill;
        let active = window.active_webview().map(|webview| webview.id()) == Some(webview.id());

        // Setup a tab frame that will contain the favicon, title and close button
        let mut tab_frame = egui::Frame::NONE.corner_radius(4).begin(ui);
        {
            tab_frame.content_ui.add_space(5.0);

            let visuals = tab_frame.content_ui.visuals_mut();
            // Remove the stroke so we don't see the border between the close button and the label
            visuals.widgets.active.bg_stroke.width = 0.0;
            visuals.widgets.hovered.bg_stroke.width = 0.0;
            // Now we make sure the fill color is always the same, irrespective of state, that way
            // we can make sure that both the label and close button have the same background color
            visuals.widgets.noninteractive.weak_bg_fill = inactive_bg_color;
            visuals.widgets.inactive.weak_bg_fill = inactive_bg_color;
            visuals.widgets.hovered.weak_bg_fill = active_bg_color;
            visuals.widgets.active.weak_bg_fill = active_bg_color;
            visuals.selection.bg_fill = active_bg_color;
            visuals.selection.stroke.color = visuals.widgets.active.fg_stroke.color;
            visuals.widgets.hovered.fg_stroke.color = visuals.widgets.active.fg_stroke.color;

            // Expansion would also show that they are 2 separate widgets
            visuals.widgets.active.expansion = 0.0;
            visuals.widgets.hovered.expansion = 0.0;

            if let Some(favicon) = favicon_texture {
                tab_frame.content_ui.add(
                    egui::Image::from_texture(favicon)
                        .fit_to_exact_size(egui::vec2(16.0, 16.0))
                        .bg_fill(egui::Color32::TRANSPARENT),
                );
            }

            let tab = tab_frame
                .content_ui
                .add(Button::selectable(
                    active,
                    truncate_with_ellipsis(&label, 20),
                ))
                .on_hover_ui(|ui| {
                    ui.label(&label);
                });

            let close_button = tab_frame.content_ui.add(
                toolbar_icons
                    .image_button(&tab_frame.content_ui, ToolbarIcon::Close)
                    .fill(egui::Color32::TRANSPARENT),
            );
            close_button.widget_info(|| {
                let mut info = WidgetInfo::new(WidgetType::Button);
                info.label = Some("Close".into());
                info
            });
            if close_button.clicked() || close_button.middle_clicked() || tab.middle_clicked() {
                window
                    .queue_user_interface_command(UserInterfaceCommand::CloseWebView(webview.id()));
            } else if !active && tab.clicked() {
                window.activate_webview(webview.id());
            }
        }

        let response = tab_frame.allocate_space(ui);
        let fill_color = if active || response.hovered() {
            active_bg_color
        } else {
            inactive_bg_color
        };
        tab_frame.frame.fill = fill_color;
        tab_frame.end(ui);
    }

    /// Render DevTools as a floating overlay anchored to the Right or Bottom
    /// edge of the viewport area. Does NOT consume layout space — the WebView
    /// always keeps 100% of the area below the toolbar.
    /// Bottom-docked DevTools panel using `egui::Area` on `Order::Foreground`
    /// so it paints AFTER the WebView PaintCallback on `LayerId::background()`.
    /// Anchored to the bottom of the viewport, resizable via top border.
    fn show_devtools_overlay(
        ctx: &egui::Context,
        viewport: egui::Rect,
        devtools: &mut crate::devtools::DevToolsPanel,
    ) -> DevtoolsDock {
        let mut result = DevtoolsDock {
            close_clicked: false,
            panel_rect: egui::Rect::NOTHING,
            close_button_rect: egui::Rect::NOTHING,
        };

        let bounds = if viewport.is_positive() { viewport } else { ctx.viewport_rect() };
        let max_h = (bounds.height() * 0.75).max(120.0);
        let min_h = 120.0_f32.min(max_h);
        let h = devtools.state.bottom_dock_height.clamp(min_h, max_h);

        let panel_rect = egui::Rect::from_min_max(
            egui::pos2(bounds.min.x, bounds.max.y - h),
            bounds.max,
        );

        let panel_bg = ctx.global_style().visuals.panel_fill;
        let border_color = ctx.global_style().visuals.widgets.noninteractive.bg_stroke.color;

        egui::Area::new(egui::Id::new("devtools_overlay"))
            .fixed_pos(panel_rect.min)
            .order(egui::Order::Foreground)
            .interactable(true)
            .show(ctx, |ui| {
                // Force content into the panel's screen-space rect so widget
                // coordinates match `panel_rect` (fixes run_ui test positioning).
                ui.scope_builder(egui::UiBuilder::new().max_rect(panel_rect), |ui| {
                    ui.set_min_size(panel_rect.size());
                    ui.set_max_size(panel_rect.size());

                    let frame = egui::Frame::default()
                        .fill(panel_bg)
                        .inner_margin(4.0)
                        .stroke(egui::Stroke::new(1.0_f32, border_color));

                    frame.show(ui, |ui| {
                        let r = crate::devtools::show_devtools_panel(ui, devtools);
                        result.close_clicked = r.close_clicked;
                        result.close_button_rect = r.close_button_rect;
                    });
                });

                // Top border resize handle in absolute screen coordinates
                let handle_rect = egui::Rect::from_min_max(
                    egui::pos2(bounds.min.x, panel_rect.min.y - 3.0),
                    egui::pos2(bounds.max.x, panel_rect.min.y + 4.0),
                );
                let handle_response = ui.interact(
                    handle_rect,
                    ui.id().with("devtools_resize_handle"),
                    egui::Sense::click_and_drag(),
                );
                if handle_response.hovered() {
                    ctx.set_cursor_icon(egui::CursorIcon::ResizeVertical);
                }
                if handle_response.dragged() {
                    if let Some(pointer_pos) = ctx.input(|i| i.pointer.interact_pos()) {
                        devtools.state.bottom_dock_height =
                            (bounds.max.y - pointer_pos.y).clamp(min_h, max_h);
                    }
                }
            });

        result.panel_rect = panel_rect;
        devtools.state.bottom_dock_height = h;
        result
    }

    /// Update the user interface, but do not paint the updated state.
    pub(crate) fn update(
        &mut self,
        state: &RunningAppState,
        window: &ServoShellWindow,
        headed_window: &headed_window::HeadedWindow,
    ) {
        self.rendering_context
            .make_current()
            .expect("Could not make RenderingContext current");

        // Track active webview for DevTools pipeline routing.
        crate::devtools::set_active_webview(
            window.active_webview().map(|wv| wv.id()),
        );

        // Always drain DevTools events so the channel does not accumulate
        // stale events while the panel is closed.
        self.devtools_panel.drain_events();

        // Frame-time telemetry for the DevTools Performance tab (only when open).
        if self.devtools_open {
            let now = std::time::Instant::now();
            let delta_ms = now.duration_since(self.last_frame_time).as_secs_f64() * 1000.0;
            self.last_frame_time = now;
            // Ignore idle pauses (no repaint) so the graph isn't polluted with
            // multi-second spikes from winit's event-driven redraw model.
            if delta_ms > 0.0 && delta_ms <= 250.0 {
                crate::devtools::sender().send(crate::devtools::DevToolsEvent::FrameTime(delta_ms));
            }
            // Keep repainting while the Performance tab is visible so FPS is live.
            if self.devtools_panel.state.active_tab == crate::devtools::DevtoolsTab::Performance {
                self.context.egui_ctx.request_repaint();
            }
        }
        let Self {
            rendering_context,
            context,
            toolbar_height,
            location,
            location_dirty,
            favicon_textures,
            toolbar_icons,
            app_menu,
            ..
        } = self;

        let winit_window = headed_window.winit_window();
        context.run(winit_window, |ctx| {
            load_pending_favicons(ctx, window, favicon_textures);

            // TODO: While in fullscreen add some way to mitigate the increased phishing risk
            // when not displaying the URL bar: https://github.com/servo/servo/issues/32443
            // Show toolbar unless fullscreen is from document (web API)
            if !headed_window.is_fullscreen_from_document() {
                let mut menu_button = None;
                let frame = egui::Frame::default()
                    .fill(ctx.style().visuals.window_fill)
                    .inner_margin(4.0);
                Panel::top("toolbar").frame(frame).show_inside(ctx, |ui| {
                    ui.allocate_ui_with_layout(
                        ui.available_size(),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            let back_button = ui
                                .add_enabled_ui(self.can_go_back, |ui| {
                                    toolbar_icons
                                        .button(ui, ToolbarIcon::Back)
                                        .on_hover_text("Back")
                                })
                                .inner;
                            back_button.widget_info(|| {
                                let mut info = WidgetInfo::new(WidgetType::Button);
                                info.label = Some("Back".into());
                                info
                            });
                            if back_button.clicked() {
                                *location_dirty = false;
                                window.queue_user_interface_command(UserInterfaceCommand::Back);
                            }

                            let forward_button = ui
                                .add_enabled_ui(self.can_go_forward, |ui| {
                                    toolbar_icons
                                        .button(ui, ToolbarIcon::Forward)
                                        .on_hover_text("Forward")
                                })
                                .inner;
                            forward_button.widget_info(|| {
                                let mut info = WidgetInfo::new(WidgetType::Button);
                                info.label = Some("Forward".into());
                                info
                            });
                            if forward_button.clicked() {
                                *location_dirty = false;
                                window.queue_user_interface_command(UserInterfaceCommand::Forward);
                            }

                            match self.load_status {
                                LoadStatus::Started | LoadStatus::HeadParsed => {
                                    let stop_button = toolbar_icons
                                        .button(ui, ToolbarIcon::Stop)
                                        .on_hover_text("Stop");
                                    stop_button.widget_info(|| {
                                        let mut info = WidgetInfo::new(WidgetType::Button);
                                        info.label = Some("Stop".into());
                                        info
                                    });
                                    if stop_button.clicked() {
                                        warn!("Do not support stop yet.");
                                    }
                                },
                                LoadStatus::Complete => {
                                    let reload_button = toolbar_icons
                                        .button(ui, ToolbarIcon::Reload)
                                        .on_hover_text("Reload");
                                    reload_button.widget_info(|| {
                                        let mut info = WidgetInfo::new(WidgetType::Button);
                                        info.label = Some("Reload".into());
                                        info
                                    });
                                    if reload_button.clicked() {
                                        *location_dirty = false;
                                        window.queue_user_interface_command(
                                            UserInterfaceCommand::Reload,
                                        );
                                    }
                                },
                            }

                            let home_button = toolbar_icons
                                .button(ui, ToolbarIcon::Home)
                                .on_hover_text("Home");
                            home_button.widget_info(|| {
                                let mut info = WidgetInfo::new(WidgetType::Button);
                                info.label = Some("Home".into());
                                info
                            });
                            if home_button.clicked() {
                                *location_dirty = false;
                                window.queue_user_interface_command(UserInterfaceCommand::Home);
                            }
                            ui.add_space(2.0);

                            ui.allocate_ui_with_layout(
                                ui.available_size(),
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    let menu_button_response = toolbar_icons
                                        .button(ui, ToolbarIcon::Menu)
                                        .on_hover_text("Menu");
                                    menu_button_response.widget_info(|| {
                                        let mut info = WidgetInfo::new(WidgetType::Button);
                                        info.label = Some("Menu".into());
                                        info.selected = Some(app_menu.is_open());
                                        info
                                    });
                                    menu_button = Some(menu_button_response);

                                    let mut experimental_preferences_enabled =
                                        state.experimental_preferences_enabled();
                                    let prefs_toggle = ui
                                        .toggle_value(&mut experimental_preferences_enabled, "☢")
                                        .on_hover_text("Enable experimental prefs");
                                    prefs_toggle.widget_info(|| {
                                        let mut info = WidgetInfo::new(WidgetType::Button);
                                        info.label = Some("Enable experimental preferences".into());
                                        info.selected = Some(experimental_preferences_enabled);
                                        info
                                    });
                                    if prefs_toggle.clicked() {
                                        state.set_experimental_preferences_enabled(
                                            experimental_preferences_enabled,
                                        );
                                        *location_dirty = false;
                                        window.queue_user_interface_command(
                                            UserInterfaceCommand::ReloadAll,
                                        );
                                    }

                                    let location_id = egui::Id::new("location_input");
                                    let location_field = ui.add_sized(
                                        ui.available_size(),
                                        egui::TextEdit::singleline(location)
                                            .id(location_id)
                                            .hint_text("Search or enter address"),
                                    );

                                    if location_field.changed() {
                                        *location_dirty = true;
                                    }
                                    // Handle adddress bar shortcut.
                                    if ui.input(|i| {
                                        if cfg!(target_os = "macos") {
                                            i.clone().consume_key(Modifiers::COMMAND, Key::L)
                                        } else {
                                            i.clone().consume_key(Modifiers::COMMAND, Key::L) ||
                                                i.clone().consume_key(Modifiers::ALT, Key::D)
                                        }
                                    }) {
                                        // The focus request immediately makes gained_focus return true.
                                        location_field.request_focus();
                                    }
                                    // Select address bar text when it's focused (click or shortcut).
                                    if location_field.gained_focus() &&
                                        let Some(mut state) =
                                            TextEditState::load(ui.ctx(), location_id)
                                    {
                                        // Select the whole input.
                                        state.cursor.set_char_range(Some(CCursorRange::two(
                                            CCursor::new(0),
                                            CCursor::new(location.len()),
                                        )));
                                        state.store(ui.ctx(), location_id);
                                    }
                                    // Navigate to address when enter is pressed in the address bar.
                                    if location_field.lost_focus() &&
                                        ui.input(|i| i.clone().key_pressed(Key::Enter))
                                    {
                                        window.queue_user_interface_command(
                                            UserInterfaceCommand::Go(location.clone()),
                                        );
                                    }
                                },
                            );
                        },
                    );
                });

                // A simple Tab header strip
                let outer = Panel::top("tabs").show_inside(ctx, |ui| {
                    // Add scroll for overflowing tabs
                    egui::ScrollArea::horizontal()
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .show(ui, |ui| {
                            ui.allocate_ui_with_layout(
                                ui.available_size(),
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    for (id, webview) in window.webviews().into_iter() {
                                        let favicon = favicon_textures
                                            .get(&id)
                                            .map(|(_, favicon)| favicon)
                                            .copied();
                                        Self::browser_tab(
                                            ui,
                                            toolbar_icons,
                                            window,
                                            webview,
                                            favicon,
                                        );
                                    }

                                    let new_tab_button = toolbar_icons
                                        .button(ui, ToolbarIcon::Plus)
                                        .on_hover_text("New tab");
                                    new_tab_button.widget_info(|| {
                                        let mut info = WidgetInfo::new(WidgetType::Button);
                                        info.label = Some("New tab".into());
                                        info
                                    });
                                    if new_tab_button.clicked() {
                                        window.queue_user_interface_command(
                                            UserInterfaceCommand::NewWebView(
                                                TopLevelWebViewCreationRequest::WithUrl(
                                                    new_tab_url(),
                                                ),
                                            ),
                                        );
                                    }
                                },
                            );
                        })
                });

                *toolbar_height = Length::new(outer.response.rect.max.y);

                if let Some(button) = &menu_button {
                    match app_menu.update(button, toolbar_icons) {
                        Some(AppMenuAction::NewTab) => {
                            window.queue_user_interface_command(UserInterfaceCommand::NewWebView(
                                TopLevelWebViewCreationRequest::WithUrl(new_tab_url()),
                            ));
                        },
                        Some(AppMenuAction::NewWindow) => {
                            window.queue_user_interface_command(UserInterfaceCommand::NewWindow(
                                TopLevelWebViewCreationRequest::WithUrl(new_tab_url()),
                            ));
                        },
                        Some(AppMenuAction::History) => {
                            window.queue_user_interface_command(UserInterfaceCommand::NewWebView(
                                TopLevelWebViewCreationRequest::WithUrl(history_url()),
                            ));
                        },
                        Some(AppMenuAction::Settings) => {
                            window.queue_user_interface_command(UserInterfaceCommand::NewWebView(
                                TopLevelWebViewCreationRequest::WithUrl(settings_url()),
                            ));
                        },
                        Some(AppMenuAction::Processes) => {
                            window.queue_user_interface_command(UserInterfaceCommand::NewWebView(
                                TopLevelWebViewCreationRequest::WithUrl(processes_url()),
                            ));
                        },
                        Some(AppMenuAction::DeveloperTools) => {
                            self.devtools_open = true;
                            self.last_frame_time = std::time::Instant::now();
                            self.devtools_panel.request_dom_tree();
                            self.devtools_panel.refresh_storage();
                            self.devtools_panel.request_page_timing();
                        },
                        Some(AppMenuAction::Exit) => {
                            state.schedule_exit();
                        },
                        None => {},
                    }
                }
            } else {
                app_menu.close_ui(ctx);
                *toolbar_height = Length::default();
            }
            // DevTools is rendered as an overlay AFTER this, so the WebView
            // always keeps 100% of the window area below the toolbar and
            // never resizes when DevTools opens, closes, or is dragged.
            let scale =
                Scale::<_, DeviceIndependentPixel, DevicePixel>::new(ctx.pixels_per_point());

            headed_window.for_each_active_dialog(window, |dialog| dialog.update(ctx));

            let available_rect = ctx.available_rect_before_wrap();
            if self.devtools_open {
                let dock = Self::show_devtools_overlay(
                    ctx,
                    available_rect,
                    &mut self.devtools_panel,
                );
                if dock.close_clicked {
                    self.devtools_open = false;
                    self.devtools_panel.clear_highlight();
                    self.devtools_rect = egui::Rect::NOTHING;
                } else {
                    self.devtools_rect = dock.panel_rect;
                }
            } else {
                self.devtools_rect = egui::Rect::NOTHING;
            }

            // Build a graft node for each WebView.
            let affine = {
                // The grafted WebView tree reports bounds in device pixels relative to the
                // WebView's own origin, so this node supplies the offset of the WebView within
                // the window, and scales it to the same scale as the rest of the nodes in egui's
                // AccessKit tree.
                let scale = (1.0 / window.platform_window().hidpi_scale_factor().get()) as f64;
                let x = available_rect.min.x as f64;
                let y = available_rect.min.y as f64;
                Affine::new([scale, 0.0, 0.0, scale, x, y])
            };
            for (webview_id, webview) in window.webviews() {
                if let Some(tree_id) = webview.accesskit_tree_id() {
                    let id = egui::Id::new(webview_id);
                    ctx.accesskit_node_builder(id, |node| {
                        node.set_tree_id(tree_id);
                        // Only the transform is set: AccessKit consumers exclude graft nodes from
                        // the presented tree, so bounds on this node would never be read.
                        node.set_transform(affine);
                    });
                }
            }
            let size = Size2D::new(available_rect.width(), available_rect.height()) * scale;
            if let Some(webview) = window.active_webview() &&
                size != webview.size()
            {
                // `rect` is sized to just the WebView viewport, which is required by
                // `OffscreenRenderingContext` See:
                // <https://github.com/servo/servo/issues/38369#issuecomment-3138378527>
                webview.resize(PhysicalSize::new(size.width as u32, size.height as u32))
            }

            if let Some(status_text) = &self.status_text {
                egui::Tooltip::always_open(
                    ctx.clone(),
                    LayerId::new(Order::Tooltip, Id::new("tooltip")),
                    "tooltip layer".into(),
                    pos2(0.0, available_rect.max.y),
                )
                .show(|ui| ui.add(Label::new(status_text.clone()).extend()));
            }

            window.repaint_webviews();

            if let Some(render_to_parent) = rendering_context.render_to_parent_callback() {
                ctx.layer_painter(LayerId::background()).add(PaintCallback {
                    rect: available_rect,
                    callback: Arc::new(CallbackFn::new(move |info, painter| {
                        let clip = info.viewport_in_pixels();
                        let rect_in_parent = Rect::new(
                            Point2D::new(clip.left_px, clip.from_bottom_px),
                            Size2D::new(clip.width_px, clip.height_px),
                        );
                        render_to_parent(painter.gl(), rect_in_parent)
                    })),
                });
            }
        });

        // If any egui widget requested a repaint, also request a repaint for our
        // containing window. This allows egui widget to animate on their own.
        if self.context.egui_ctx.has_requested_repaint() {
            window.set_needs_repaint();
        }

        let adapter = self
            .context
            .egui_winit
            .accesskit
            .as_mut()
            .expect("guaranteed by Gui::new()");
        for tree_update in self.pending_accesskit_updates.drain(..) {
            adapter.update_if_active(|| tree_update);
        }
    }

    /// Paint the GUI, as of the last update.
    pub(crate) fn paint(&mut self, window: &Window) {
        self.rendering_context
            .make_current()
            .expect("Could not make RenderingContext current");
        self.rendering_context
            .parent_context()
            .prepare_for_rendering();
        self.context.paint(window);
        self.rendering_context.parent_context().present();
    }

    /// Updates the location field from the given [`RunningAppState`], unless the user has started
    /// editing it without clicking Go, returning true iff it has changed (needing an egui update).
    fn update_location_in_toolbar(&mut self, window: &ServoShellWindow) -> bool {
        // User edited without clicking Go?
        if self.location_dirty {
            return false;
        }

        let current_url_string = window
            .active_webview()
            .and_then(|webview| Some(webview.url()?.to_string()));
        match current_url_string {
            Some(location) if location != self.location => {
                self.location = location;
                true
            },
            _ => false,
        }
    }

    fn update_load_status(&mut self, window: &ServoShellWindow) -> bool {
        let state_status = window
            .active_webview()
            .map(|webview| webview.load_status())
            .unwrap_or(LoadStatus::Complete);
        let old_status = std::mem::replace(&mut self.load_status, state_status);
        let status_changed = old_status != self.load_status;

        // When the load status changes, we want the new changes to the URL to start
        // being reflected in the location bar.
        if status_changed {
            self.location_dirty = false;
        }

        // Auto-refresh DevTools data when a page load completes.
        if status_changed && self.load_status == LoadStatus::Complete {
            if self.devtools_open || self.devtools_panel.state.auto_refresh_dom {
                self.devtools_panel.request_dom_tree();
                self.devtools_panel.refresh_storage();
            }
        }

        status_changed
    }

    fn update_status_text(&mut self, window: &ServoShellWindow) -> bool {
        let state_status = window
            .active_webview()
            .and_then(|webview| webview.status_text());
        let old_status = std::mem::replace(&mut self.status_text, state_status);
        old_status != self.status_text
    }

    fn update_can_go_back_and_forward(&mut self, window: &ServoShellWindow) -> bool {
        let (can_go_back, can_go_forward) = window
            .active_webview()
            .map(|webview| (webview.can_go_back(), webview.can_go_forward()))
            .unwrap_or((false, false));
        let old_can_go_back = std::mem::replace(&mut self.can_go_back, can_go_back);
        let old_can_go_forward = std::mem::replace(&mut self.can_go_forward, can_go_forward);
        old_can_go_back != self.can_go_back || old_can_go_forward != self.can_go_forward
    }

    /// Updates all fields taken from the given [`ServoShellWindow`], such as the location field.
    /// Returns true iff the egui needs an update.
    pub(crate) fn update_webview_data(&mut self, window: &ServoShellWindow) -> bool {
        // Note: We must use the "bitwise OR" (|) operator here instead of "logical OR" (||)
        //       because logical OR would short-circuit if any of the functions return true.
        //       We want to ensure that all functions are called. The "bitwise OR" operator
        //       does not short-circuit.
        self.update_load_status(window) |
            self.update_location_in_toolbar(window) |
            self.update_status_text(window) |
            self.update_can_go_back_and_forward(window)
    }

    /// Returns true if a redraw is required after handling the provided event.
    pub(crate) fn handle_accesskit_event(
        &mut self,
        event: &egui_winit::accesskit_winit::WindowEvent,
    ) -> bool {
        match event {
            egui_winit::accesskit_winit::WindowEvent::InitialTreeRequested => {
                self.context.egui_ctx.enable_accesskit();
                true
            },
            egui_winit::accesskit_winit::WindowEvent::ActionRequested(req) => {
                self.context
                    .egui_winit
                    .on_accesskit_action_request(req.clone());
                true
            },
            egui_winit::accesskit_winit::WindowEvent::AccessibilityDeactivated => {
                self.context.egui_ctx.disable_accesskit();
                false
            },
        }
    }

    pub(crate) fn set_zoom_factor(&self, factor: f32) {
        self.context.egui_ctx.set_zoom_factor(factor);
    }

    pub(crate) fn notify_accessibility_tree_update(&mut self, tree_update: accesskit::TreeUpdate) {
        self.pending_accesskit_updates.push(tree_update);
    }
}

fn embedder_image_to_egui_image(image: &Image) -> egui::ColorImage {
    let width = image.width as usize;
    let height = image.height as usize;

    match image.format {
        PixelFormat::K8 => egui::ColorImage::from_gray([width, height], image.data()),
        PixelFormat::KA8 => {
            // Convert to rgba
            let data: Vec<u8> = image
                .data()
                .chunks_exact(2)
                .flat_map(|pixel| [pixel[0], pixel[0], pixel[0], pixel[1]])
                .collect();
            egui::ColorImage::from_rgba_unmultiplied([width, height], &data)
        },
        PixelFormat::RGB8 => egui::ColorImage::from_rgb([width, height], image.data()),
        PixelFormat::RGBA8 => {
            egui::ColorImage::from_rgba_unmultiplied([width, height], image.data())
        },
        PixelFormat::BGRA8 => {
            // Convert from BGRA to RGBA
            let data: Vec<u8> = image
                .data()
                .chunks_exact(4)
                .flat_map(|chunk| [chunk[2], chunk[1], chunk[0], chunk[3]])
                .collect();
            egui::ColorImage::from_rgba_unmultiplied([width, height], &data)
        },
    }
}

/// Uploads all favicons that have not yet been processed to the GPU.
fn load_pending_favicons(
    ctx: &egui::Context,
    window: &ServoShellWindow,
    texture_cache: &mut HashMap<WebViewId, (egui::TextureHandle, egui::load::SizedTexture)>,
) {
    for id in window.take_pending_favicon_loads() {
        let Some(webview) = window.webview_by_id(id) else {
            continue;
        };
        let Some(favicon) = webview.favicon() else {
            continue;
        };

        let egui_image = embedder_image_to_egui_image(&favicon);
        let handle = ctx.load_texture(format!("favicon-{id:?}"), egui_image, Default::default());
        let texture = egui::load::SizedTexture::new(
            handle.id(),
            egui::vec2(favicon.width as f32, favicon.height as f32),
        );

        // We don't need the handle anymore but we can't drop it either since that would cause
        // the texture to be freed.
        texture_cache.insert(id, (handle, texture));
    }
}

#[cfg(test)]
mod tests {
    use euclid::Point2D;

    use super::*;

    fn screen_input(events: Vec<egui::Event>) -> egui::RawInput {
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(800.0, 600.0),
            )),
            events,
            ..Default::default()
        }
    }

    #[allow(dead_code)]
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

    #[test]
    fn devtools_dock_sits_at_bottom_of_viewport() {
        let ctx = egui::Context::default();
        let mut devtools = crate::devtools::DevToolsPanel::new();
        devtools.state.bottom_dock_height = 280.0;
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 40.0), egui::vec2(800.0, 560.0));

        let _ = ctx.run_ui(screen_input(vec![]), |_ui| {
            let dock = Gui::show_devtools_overlay(&ctx, viewport, &mut devtools);
            assert!(dock.panel_rect.max.y >= viewport.max.y - 10.0);
            assert!(dock.panel_rect.min.x <= viewport.min.x + 10.0);
            assert!(dock.panel_rect.max.x >= viewport.max.x - 10.0);
            assert!(dock.panel_rect.height() > 50.0);
            assert!(dock.panel_rect.height() < viewport.height() * 0.95);
            assert!(!dock.close_clicked);
        });
    }

    #[test]
    fn devtools_close_button_is_inside_panel() {
        let ctx = egui::Context::default();
        let mut devtools = crate::devtools::DevToolsPanel::new();
        let viewport = egui::Rect::from_min_size(egui::pos2(0.0, 40.0), egui::vec2(800.0, 560.0));

        let mut close_button_rect = egui::Rect::NOTHING;
        let mut panel_rect = egui::Rect::NOTHING;
        let _ = ctx.run_ui(screen_input(vec![]), |_ui| {
            let dock = Gui::show_devtools_overlay(&ctx, viewport, &mut devtools);
            close_button_rect = dock.close_button_rect;
            panel_rect = dock.panel_rect;
        });
        assert!(panel_rect.width() > 100.0);
        assert!(panel_rect.height() > 100.0);
        assert!(panel_rect.max.y >= viewport.max.y - 10.0);
        assert!(close_button_rect.is_positive(), "close_button_rect must have positive size");
        assert!(
            panel_rect.contains_rect(close_button_rect),
            "close_button_rect {:#?} must be inside panel_rect {:#?}",
            close_button_rect,
            panel_rect,
        );
    }

    #[test]
    fn pointer_over_the_devtools_dock_hits_chrome() {
        let dock = egui::Rect::from_min_max(egui::pos2(400.0, 40.0), egui::pos2(800.0, 600.0));

        assert!(!pointer_hits_browser_chrome(
            40.0,
            false,
            dock,
            Point2D::new(100.0, 200.0),
        ));
        assert!(pointer_hits_browser_chrome(
            40.0,
            false,
            dock,
            Point2D::new(600.0, 200.0),
        ));
        assert!(pointer_hits_browser_chrome(
            40.0,
            false,
            dock,
            Point2D::new(100.0, 10.0),
        ));
        assert!(pointer_hits_browser_chrome(
            40.0,
            true,
            egui::Rect::NOTHING,
            Point2D::new(100.0, 200.0),
        ));
        assert!(!pointer_hits_browser_chrome(
            40.0,
            false,
            egui::Rect::NOTHING,
            Point2D::new(600.0, 200.0),
        ));
    }
}
