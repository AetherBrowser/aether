/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `mask-image` for a solid background color.
//!
//! WebRender image-mask clips panic when attached to a rectangle. The mask is
//! rasterized to an alpha image and drawn with the background color instead.

use app_units::Au;
use euclid::{Size2D, Vector2D};
use net_traits::image_cache::Image as CachedImage;
use pixels::PixelFormat;
use style::values::computed::MaskMode;
use style::values::computed::background::BackgroundSize;
use style::values::computed::image::Image;
use style::values::specified::background::BackgroundRepeat as RepeatXY;
use webrender_api::ImageKey;
use webrender_api::units::{DeviceIntSize, LayoutRect, LayoutSize};

use crate::context::{ResolveImageError, ResolvedImage};
use crate::display_list::DisplayListBuilder;
use crate::display_list::background::{get_cyclic, layout_1d};
use crate::replaced::NaturalSizes;

pub(super) enum MaskImage {
    /// No mask layer. The background color is painted as a solid rectangle.
    None,
    /// A mask image is specified but not decoded yet. Skip the color so it
    /// does not flash unmasked.
    Pending,
    /// The mask covers nothing (for example `mask-size: 0 0`). Skip the color.
    Transparent,
    Ready {
        image_key: ImageKey,
        bounds: LayoutRect,
        tile_size: LayoutSize,
        tile_spacing: LayoutSize,
        repeat: bool,
    },
}

impl DisplayListBuilder<'_> {
    pub(super) fn mask_image(
        &mut self,
        node: Option<style::dom::OpaqueNode>,
        style: &style::properties::ComputedValues,
        border_rect: LayoutRect,
    ) -> MaskImage {
        let svg = style.get_svg();
        let Some(layer_index) = svg
            .mask_image
            .0
            .iter()
            .position(|image| !matches!(image, Image::None))
        else {
            return MaskImage::None;
        };
        let image = &svg.mask_image.0[layer_index];
        let (image, size) = match self.image_resolver.resolve_image(node, image) {
            Ok(ResolvedImage::Image { image, size, .. }) => (image, size),
            Err(ResolveImageError::ImagePending | ResolveImageError::OnlyMetadata) => {
                return MaskImage::Pending;
            },
            // Gradients, solid colors, and failed loads are not rasterized.
            // Leave the background color unmasked instead of dropping it.
            _ => return MaskImage::None,
        };

        let natural = NaturalSizes::from_width_and_height(size.width, size.height);
        let Some(mut tile_size) = mask_tile_size(
            get_cyclic(&svg.mask_size.0, layer_index),
            border_rect.size(),
            natural,
        ) else {
            return MaskImage::Pending;
        };
        if tile_size.width <= 0.0 || tile_size.height <= 0.0 {
            return MaskImage::Transparent;
        }

        let RepeatXY(repeat_x, repeat_y) = *get_cyclic(&svg.mask_repeat.0, layer_index);
        let result_x = layout_1d(
            &mut tile_size.width,
            repeat_x,
            get_cyclic(&svg.mask_position_x.0, layer_index),
            0.0,
            border_rect.width(),
            border_rect.width(),
        );
        let result_y = layout_1d(
            &mut tile_size.height,
            repeat_y,
            get_cyclic(&svg.mask_position_y.0, layer_index),
            0.0,
            border_rect.height(),
            border_rect.height(),
        );
        if tile_size.width <= 0.0 || tile_size.height <= 0.0 {
            return MaskImage::Transparent;
        }
        let bounds = LayoutRect::from_origin_and_size(
            border_rect.min + Vector2D::new(result_x.bounds_origin, result_y.bounds_origin),
            Size2D::new(result_x.bounds_size, result_y.bounds_size),
        );
        let tile_spacing = LayoutSize::new(result_x.tile_spacing, result_y.tile_spacing);

        let scale = self.device_pixel_ratio.get();
        let raster_size = Size2D::new(tile_size.width * scale, tile_size.height * scale)
            .to_i32()
            .max(DeviceIntSize::new(1, 1));
        let raster = match &image {
            CachedImage::Raster(raster) => raster.clone(),
            CachedImage::Vector(vector) => {
                let Some(node) = node else {
                    return MaskImage::Pending;
                };
                let Some(raster) = self.image_resolver.rasterize_vector_image(
                    vector.id,
                    raster_size,
                    node,
                    vector.svg_id,
                ) else {
                    return MaskImage::Pending;
                };
                std::sync::Arc::new(raster)
            },
        };
        if raster.format != PixelFormat::RGBA8 {
            return MaskImage::Pending;
        }
        let (byte_range, width, height) = match raster.frames.first() {
            Some(frame) => (
                frame.byte_range.clone(),
                frame.width as i32,
                frame.height as i32,
            ),
            None => (
                0..raster.bytes.len(),
                raster.metadata.width as i32,
                raster.metadata.height as i32,
            ),
        };
        let luminance = matches!(
            get_cyclic(&svg.mask_mode.0, layer_index),
            MaskMode::Luminance
        );
        let Some(image_key) = self.image_resolver.image_cache.alpha_mask_image_key(
            &raster.bytes,
            byte_range,
            width,
            height,
            luminance,
        ) else {
            return MaskImage::Pending;
        };
        MaskImage::Ready {
            image_key,
            bounds,
            tile_size,
            tile_spacing,
            repeat: result_x.repeat || result_y.repeat,
        }
    }
}

fn mask_tile_size(
    size: &BackgroundSize,
    area: LayoutSize,
    natural: NaturalSizes,
) -> Option<LayoutSize> {
    let contain_or_cover = |cover: bool| {
        let mut tile = area;
        if let Some(ratio) = natural.ratio {
            let area_ratio = area.width / area.height;
            let fit_width = if cover {
                area_ratio > ratio
            } else {
                area_ratio <= ratio
            };
            if fit_width {
                tile.height = tile.width / ratio;
            } else {
                tile.width = tile.height * ratio;
            }
        }
        tile
    };

    let tile = match size {
        BackgroundSize::Contain => contain_or_cover(false),
        BackgroundSize::Cover => contain_or_cover(true),
        BackgroundSize::ExplicitSize { width, height } => {
            let width = width
                .non_auto()
                .map(|length| length.0.to_used_value(Au::from_f32_px(area.width)));
            let height = height
                .non_auto()
                .map(|length| length.0.to_used_value(Au::from_f32_px(area.height)));
            match (width, height) {
                (None, None) => {
                    let width = natural.width.map(|value| value.to_f32_px());
                    let height = natural.height.map(|value| value.to_f32_px());
                    match (width, height) {
                        (Some(width), Some(height)) => LayoutSize::new(width, height),
                        _ => contain_or_cover(false),
                    }
                },
                (Some(width), Some(height)) => {
                    LayoutSize::new(width.to_f32_px(), height.to_f32_px())
                },
                (Some(width), None) => {
                    let height = natural
                        .ratio
                        .map(|ratio| width.scale_by(1.0 / ratio).to_f32_px())
                        .or_else(|| natural.height.map(|value| value.to_f32_px()))
                        .unwrap_or(area.height);
                    LayoutSize::new(width.to_f32_px(), height)
                },
                (None, Some(height)) => {
                    let width = natural
                        .ratio
                        .map(|ratio| height.scale_by(ratio).to_f32_px())
                        .or_else(|| natural.width.map(|value| value.to_f32_px()))
                        .unwrap_or(area.width);
                    LayoutSize::new(width, height.to_f32_px())
                },
            }
        },
    };
    Some(tile)
}
