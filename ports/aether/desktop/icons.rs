/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! SVG toolbar icons. Add new buttons by dropping an SVG in `resources/icons/`
//! and a variant on [`ToolbarIcon`].

use std::collections::HashMap;

use egui::load::SizedTexture;
use log::warn;
use resvg::tiny_skia;
use resvg::usvg;

/// Logical size of a toolbar icon, in egui points.
const TOOLBAR_ICON_SIZE: f32 = 14.0;

/// Toolbar button hit target, matching [`super::gui::Gui::toolbar_button`].
const TOOLBAR_BUTTON_SIZE: f32 = 20.0;

/// A bundled SVG used by the chrome toolbar.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ToolbarIcon {
    Home,
    Reload,
    Stop,
}

impl ToolbarIcon {
    fn svg_bytes(self) -> &'static [u8] {
        match self {
            Self::Home => include_bytes!("../../../resources/icons/home.svg"),
            Self::Reload => include_bytes!("../../../resources/icons/reload.svg"),
            Self::Stop => include_bytes!("../../../resources/icons/stop-reload.svg"),
        }
    }

    fn texture_name(self) -> &'static str {
        match self {
            Self::Home => "toolbar-home",
            Self::Reload => "toolbar-reload",
            Self::Stop => "toolbar-stop",
        }
    }
}

/// Rasterized SVG textures, cached by icon and physical pixel size.
#[derive(Default)]
pub(crate) struct ToolbarIconCache {
    textures: HashMap<(ToolbarIcon, u32), egui::TextureHandle>,
}

impl ToolbarIconCache {
    pub(crate) fn button(&mut self, ui: &mut egui::Ui, icon: ToolbarIcon) -> egui::Response {
        let pixel_size = (TOOLBAR_ICON_SIZE * ui.pixels_per_point()).round().max(1.0) as u32;
        let button = match self.texture(ui.ctx(), icon, pixel_size) {
            Some(handle) => {
                let image = egui::Image::from_texture(SizedTexture::new(
                    handle.id(),
                    egui::vec2(TOOLBAR_ICON_SIZE, TOOLBAR_ICON_SIZE),
                ))
                .fit_to_exact_size(egui::vec2(TOOLBAR_ICON_SIZE, TOOLBAR_ICON_SIZE));
                egui::Button::new(image)
            },
            None => egui::Button::new(""),
        };
        ui.add(
            button
                .frame(false)
                .min_size(egui::vec2(TOOLBAR_BUTTON_SIZE, TOOLBAR_BUTTON_SIZE))
                .image_tint_follows_text_color(true),
        )
    }

    fn texture(
        &mut self,
        ctx: &egui::Context,
        icon: ToolbarIcon,
        pixel_size: u32,
    ) -> Option<egui::TextureHandle> {
        if let Some(handle) = self.textures.get(&(icon, pixel_size)) {
            return Some(handle.clone());
        }

        let color_image = rasterize_svg(icon.svg_bytes(), pixel_size)?;
        let handle = ctx.load_texture(
            format!("{}@{pixel_size}", icon.texture_name()),
            color_image,
            egui::TextureOptions::LINEAR,
        );
        self.textures.insert((icon, pixel_size), handle.clone());
        Some(handle)
    }
}

pub(crate) fn rasterize_svg(svg: &[u8], size_px: u32) -> Option<egui::ColorImage> {
    let tree = usvg::Tree::from_data(svg, &usvg::Options::default())
        .map_err(|error| {
            warn!("Failed to parse toolbar SVG: {error}");
            error
        })
        .ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(size_px, size_px)?;
    let svg_size = tree.size();
    let transform = tiny_skia::Transform::from_scale(
        size_px as f32 / svg_size.width(),
        size_px as f32 / svg_size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(egui::ColorImage::from_rgba_premultiplied(
        [size_px as usize, size_px as usize],
        pixmap.data(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_svg_rasterizes() {
        let image =
            rasterize_svg(ToolbarIcon::Home.svg_bytes(), 32).expect("home.svg should rasterize");
        assert_eq!(image.size, [32, 32]);
        assert!(
            image.pixels.iter().any(|pixel| pixel.a() > 0),
            "home icon should not be fully transparent"
        );
    }

    #[test]
    fn reload_svg_rasterizes() {
        let image = rasterize_svg(ToolbarIcon::Reload.svg_bytes(), 32)
            .expect("reload.svg should rasterize");
        assert_eq!(image.size, [32, 32]);
        assert!(
            image.pixels.iter().any(|pixel| pixel.a() > 0),
            "reload icon should not be fully transparent"
        );
    }

    #[test]
    fn stop_svg_rasterizes() {
        let image = rasterize_svg(ToolbarIcon::Stop.svg_bytes(), 32)
            .expect("stop-reload.svg should rasterize");
        assert_eq!(image.size, [32, 32]);
        assert!(
            image.pixels.iter().any(|pixel| pixel.a() > 0),
            "stop icon should not be fully transparent"
        );
    }
}
