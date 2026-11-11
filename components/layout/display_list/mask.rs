/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! `mask-image` for a solid background color.
//!
//! WebRender image-mask clips panic when attached to a rectangle. The mask is
//! rasterized to an alpha image and drawn with the background color instead.

use app_units::Au;
use euclid::Size2D;
use net_traits::image_cache::Image as CachedImage;
use pixels::PixelFormat;
use style::values::computed::LengthPercentage;
use style::values::computed::background::BackgroundSize;
use style::values::computed::image::Image;
use style::values::specified::background::BackgroundRepeatKeyword;
use webrender_api::ImageKey;
use webrender_api::units::{DeviceIntSize, LayoutRect, LayoutSize};

use crate::context::ResolvedImage;
use crate::display_list::DisplayListBuilder;
use crate::display_list::background::get_cyclic;
use crate::replaced::NaturalSizes;

pub(super) enum MaskImage {
    None,
    Pending,
    Ready {
        image_key: ImageKey,
        rect: LayoutRect,
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
        let Ok(ResolvedImage::Image { image, size, .. }) =
            self.image_resolver.resolve_image(node, image)
        else {
            return MaskImage::Pending;
        };

        let natural = NaturalSizes::from_width_and_height(size.width, size.height);
        let Some(tile_size) = mask_tile_size(
            get_cyclic(&svg.mask_size.0, layer_index),
            border_rect.size(),
            natural,
        ) else {
            return MaskImage::Pending;
        };
        if tile_size.width <= 0.0 || tile_size.height <= 0.0 {
            return MaskImage::None;
        }

        let repeat = get_cyclic(&svg.mask_repeat.0, layer_index);
        let mask_rect = if repeat.0 == BackgroundRepeatKeyword::NoRepeat &&
            repeat.1 == BackgroundRepeatKeyword::NoRepeat
        {
            let origin_x = mask_position(
                get_cyclic(&svg.mask_position_x.0, layer_index),
                border_rect.min.x,
                border_rect.width(),
                tile_size.width,
            );
            let origin_y = mask_position(
                get_cyclic(&svg.mask_position_y.0, layer_index),
                border_rect.min.y,
                border_rect.height(),
                tile_size.height,
            );
            LayoutRect::from_origin_and_size(euclid::point2(origin_x, origin_y), tile_size)
        } else {
            border_rect
        };

        let scale = self.device_pixel_ratio.get();
        let raster_size = Size2D::new(mask_rect.width() * scale, mask_rect.height() * scale)
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
        let Some(image_key) = self.image_resolver.image_cache.alpha_mask_image_key(
            &raster.bytes,
            raster.metadata.width as i32,
            raster.metadata.height as i32,
        ) else {
            return MaskImage::Pending;
        };
        MaskImage::Ready {
            image_key,
            rect: mask_rect,
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

fn mask_position(position: &LengthPercentage, area_origin: f32, area_size: f32, tile: f32) -> f32 {
    area_origin +
        position
            .to_used_value(Au::from_f32_px(area_size - tile))
            .to_f32_px()
}
