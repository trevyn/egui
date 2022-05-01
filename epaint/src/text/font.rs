use crate::{
    mutex::{Mutex, RwLock},
    Color32, ColorImage, ImageData, TextureAtlas,
};
use ahash::AHashMap;
use emath::{vec2, Vec2};
use std::collections::BTreeSet;
use std::sync::Arc;

// ----------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Deserialize, serde::Serialize))]
pub struct UvRect {
    /// X/Y offset for nice rendering (unit: points).
    pub offset: Vec2,

    /// Screen size (in points) of this glyph.
    /// Note that the height is different from the font height.
    pub size: Vec2,

    /// Top left corner UV in texture.
    pub min: [u16; 2],

    /// Bottom right corner (exclusive).
    pub max: [u16; 2],
}

impl UvRect {
    pub fn is_nothing(&self) -> bool {
        self.min == self.max
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GlyphInfo {
    pub(crate) id: ab_glyph::GlyphId,

    /// Unit: points.
    pub advance_width: f32,

    /// Texture coordinates. None for space.
    pub uv_rect: UvRect,
}

impl Default for GlyphInfo {
    fn default() -> Self {
        Self {
            id: ab_glyph::GlyphId(0),
            advance_width: 0.0,
            uv_rect: Default::default(),
        }
    }
}

// ----------------------------------------------------------------------------

/// A specific font with a size.
/// The interface uses points as the unit for everything.
pub struct FontImpl {
    name: String,
    ab_glyph_font: Option<ab_glyph::FontArc>,
    /// Maximum character height
    scale_in_pixels: u32,
    height_in_points: f32,
    // move each character by this much (hack)
    y_offset: f32,
    pixels_per_point: f32,
    glyph_info_cache: RwLock<AHashMap<char, GlyphInfo>>, // TODO: standard Mutex
    atlas: Arc<Mutex<TextureAtlas>>,
}

impl FontImpl {
    pub fn new(
        atlas: Arc<Mutex<TextureAtlas>>,
        pixels_per_point: f32,
        name: String,
        ab_glyph_font: Option<ab_glyph::FontArc>,
        scale_in_pixels: u32,
        y_offset_points: f32,
    ) -> FontImpl {
        assert!(scale_in_pixels > 0);
        assert!(pixels_per_point > 0.0);

        let height_in_points = scale_in_pixels as f32 / pixels_per_point;

        // TODO: use these font metrics?
        // use ab_glyph::ScaleFont as _;
        // let scaled = ab_glyph_font.as_scaled(scale_in_pixels as f32);
        // dbg!(scaled.ascent());
        // dbg!(scaled.descent());
        // dbg!(scaled.line_gap());

        // Round to closest pixel:
        let y_offset = (y_offset_points * pixels_per_point).round() / pixels_per_point;

        Self {
            name,
            ab_glyph_font,
            scale_in_pixels,
            height_in_points,
            y_offset,
            pixels_per_point,
            glyph_info_cache: Default::default(),
            atlas,
        }
    }

    fn ignore_character(&self, chr: char) -> bool {
        if self.name == "emoji-icon-font" {
            // HACK: https://github.com/emilk/egui/issues/1284 https://github.com/jslegers/emoji-icon-font/issues/18
            // Don't show the wrong fullwidth capital letters:
            if 'Ｓ' <= chr && chr <= 'Ｙ' {
                return true;
            }
        }

        matches!(
            chr,
            // Strip out a religious symbol with secondary nefarious interpretation:
            '\u{534d}' | '\u{5350}' |

            // Ignore ubuntu-specific stuff in `Ubuntu-Light.ttf`:
            '\u{E0FF}' | '\u{EFFD}' | '\u{F0FF}' | '\u{F200}'
        )
    }

    /// An un-ordered iterator over all supported characters.
    fn characters(&self) -> Option<impl Iterator<Item = char> + '_> {
        use ab_glyph::Font as _;
        self.ab_glyph_font.as_ref().map(|f| {
            f.codepoint_ids()
                .map(|(_, chr)| chr)
                .filter(|&chr| !self.ignore_character(chr))
        })
    }

    /// `\n` will result in `None`
    fn glyph_info(&self, c: char) -> Option<GlyphInfo> {
        {
            if let Some(glyph_info) = self.glyph_info_cache.read().get(&c) {
                return Some(*glyph_info);
            }
        }

        if self.ignore_character(c) {
            return None;
        }

        if c == '\t' {
            if let Some(space) = self.glyph_info(' ') {
                let glyph_info = GlyphInfo {
                    advance_width: crate::text::TAB_SIZE as f32 * space.advance_width,
                    ..GlyphInfo::default()
                };
                self.glyph_info_cache.write().insert(c, glyph_info);
                return Some(glyph_info);
            }
        }

        // Add new character:
        if let Some(ab_glyph_font) = self.ab_glyph_font.as_ref() {
            use ab_glyph::Font as _;
            let glyph_id = ab_glyph_font.glyph_id(c);

            if glyph_id.0 == 0 {
                if invisible_char(c) {
                    // hack
                    let glyph_info = GlyphInfo::default();
                    self.glyph_info_cache.write().insert(c, glyph_info);
                    Some(glyph_info)
                } else {
                    None // unsupported character
                }
            } else {
                let glyph_info = allocate_glyph(
                    &mut self.atlas.lock(),
                    &ab_glyph_font,
                    glyph_id,
                    self.scale_in_pixels as f32,
                    self.y_offset,
                    self.pixels_per_point,
                );

                self.glyph_info_cache.write().insert(c, glyph_info);
                Some(glyph_info)
            }
        } else {
            let glyph_info = allocate_native_glyph(
                &mut self.atlas.lock(),
                &self.name,
                c,
                self.scale_in_pixels as f32,
                self.y_offset,
                self.pixels_per_point,
            );

            self.glyph_info_cache.write().insert(c, glyph_info);
            Some(glyph_info)
        }
    }

    #[inline]
    pub fn pair_kerning(
        &self,
        last_glyph_id: ab_glyph::GlyphId,
        glyph_id: ab_glyph::GlyphId,
    ) -> f32 {
        if let Some(ab_glyph_font) = self.ab_glyph_font.as_ref() {
            use ab_glyph::{Font as _, ScaleFont};
            ab_glyph_font
                .as_scaled(self.scale_in_pixels as f32)
                .kern(last_glyph_id, glyph_id)
                / self.pixels_per_point
        } else {
            0.0
        }
    }

    /// Height of one row of text. In points
    #[inline(always)]
    pub fn row_height(&self) -> f32 {
        self.height_in_points
    }

    #[inline(always)]
    pub fn pixels_per_point(&self) -> f32 {
        self.pixels_per_point
    }
}

type FontIndex = usize;

// TODO: rename?
/// Wrapper over multiple [`FontImpl`] (e.g. a primary + fallbacks for emojis)
pub struct Font {
    fonts: Vec<Arc<FontImpl>>,
    /// Lazily calculated.
    characters: Option<std::collections::BTreeSet<char>>,
    replacement_glyph: (FontIndex, GlyphInfo),
    pixels_per_point: f32,
    row_height: f32,
    glyph_info_cache: AHashMap<char, (FontIndex, GlyphInfo)>,
}

impl Font {
    pub fn new(fonts: Vec<Arc<FontImpl>>) -> Self {
        if fonts.is_empty() {
            return Self {
                fonts,
                characters: None,
                replacement_glyph: Default::default(),
                pixels_per_point: 1.0,
                row_height: 0.0,
                glyph_info_cache: Default::default(),
            };
        }

        let pixels_per_point = fonts[0].pixels_per_point();
        let row_height = fonts[0].row_height();

        let mut slf = Self {
            fonts,
            characters: None,
            replacement_glyph: Default::default(),
            pixels_per_point,
            row_height,
            glyph_info_cache: Default::default(),
        };

        const PRIMARY_REPLACEMENT_CHAR: char = '◻'; // white medium square
        const FALLBACK_REPLACEMENT_CHAR: char = '?'; // fallback for the fallback

        let replacement_glyph = slf
            .glyph_info_no_cache_or_fallback(PRIMARY_REPLACEMENT_CHAR)
            .or_else(|| slf.glyph_info_no_cache_or_fallback(FALLBACK_REPLACEMENT_CHAR))
            .unwrap_or_else(|| {
                panic!(
                    "Failed to find replacement characters {:?} or {:?}",
                    PRIMARY_REPLACEMENT_CHAR, FALLBACK_REPLACEMENT_CHAR
                )
            });
        slf.replacement_glyph = replacement_glyph;

        slf
    }

    pub fn preload_common_characters(&mut self) {
        // Preload the printable ASCII characters [32, 126] (which excludes control codes):
        const FIRST_ASCII: usize = 32; // 32 == space
        const LAST_ASCII: usize = 126;
        for c in (FIRST_ASCII..=LAST_ASCII).map(|c| c as u8 as char) {
            self.glyph_info(c);
        }
        self.glyph_info('°');
        self.glyph_info(crate::text::PASSWORD_REPLACEMENT_CHAR);
    }

    /// All supported characters
    pub fn characters(&mut self) -> &BTreeSet<char> {
        self.characters.get_or_insert_with(|| {
            let mut characters = BTreeSet::new();
            for font in &self.fonts {
                font.characters().map(|c| characters.extend(c));
            }
            characters
        })
    }

    #[inline(always)]
    pub fn round_to_pixel(&self, point: f32) -> f32 {
        (point * self.pixels_per_point).round() / self.pixels_per_point
    }

    /// Height of one row of text. In points
    #[inline(always)]
    pub fn row_height(&self) -> f32 {
        self.row_height
    }

    pub fn uv_rect(&self, c: char) -> UvRect {
        self.glyph_info_cache
            .get(&c)
            .map(|gi| gi.1.uv_rect)
            .unwrap_or_default()
    }

    /// Width of this character in points.
    pub fn glyph_width(&mut self, c: char) -> f32 {
        self.glyph_info(c).1.advance_width
    }

    /// `\n` will (intentionally) show up as the replacement character.
    fn glyph_info(&mut self, c: char) -> (FontIndex, GlyphInfo) {
        if let Some(font_index_glyph_info) = self.glyph_info_cache.get(&c) {
            return *font_index_glyph_info;
        }

        let font_index_glyph_info = self.glyph_info_no_cache_or_fallback(c);
        let font_index_glyph_info = font_index_glyph_info.unwrap_or(self.replacement_glyph);
        self.glyph_info_cache.insert(c, font_index_glyph_info);
        font_index_glyph_info
    }

    #[inline]
    pub(crate) fn glyph_info_and_font_impl(&mut self, c: char) -> (Option<&FontImpl>, GlyphInfo) {
        if self.fonts.is_empty() {
            return (None, self.replacement_glyph.1);
        }
        let (font_index, glyph_info) = self.glyph_info(c);
        let font_impl = &self.fonts[font_index];
        (Some(font_impl), glyph_info)
    }

    fn glyph_info_no_cache_or_fallback(&mut self, c: char) -> Option<(FontIndex, GlyphInfo)> {
        for (font_index, font_impl) in self.fonts.iter().enumerate() {
            if let Some(glyph_info) = font_impl.glyph_info(c) {
                self.glyph_info_cache.insert(c, (font_index, glyph_info));
                return Some((font_index, glyph_info));
            }
        }
        None
    }
}

#[inline]
fn invisible_char(c: char) -> bool {
    // See https://github.com/emilk/egui/issues/336

    // From https://www.fileformat.info/info/unicode/category/Cf/list.htm
    ('\u{200B}'..='\u{206F}').contains(&c) // TODO: heed bidi characters
}

fn allocate_glyph(
    atlas: &mut TextureAtlas,
    font: &ab_glyph::FontArc,
    glyph_id: ab_glyph::GlyphId,
    scale_in_pixels: f32,
    y_offset: f32,
    pixels_per_point: f32,
) -> GlyphInfo {
    assert!(glyph_id.0 != 0);
    use ab_glyph::{Font as _, ScaleFont};

    let glyph =
        glyph_id.with_scale_and_position(scale_in_pixels, ab_glyph::Point { x: 0.0, y: 0.0 });

    let uv_rect = font.outline_glyph(glyph).map(|glyph| {
        let bb = glyph.px_bounds();
        let glyph_width = bb.width() as usize;
        let glyph_height = bb.height() as usize;
        if glyph_width == 0 || glyph_height == 0 {
            UvRect::default()
        } else {
            let (glyph_pos, image) = atlas.allocate((glyph_width, glyph_height));

            match image {
                ImageData::Font(image) => {
                    glyph.draw(|x, y, v| {
                        if v > 0.0 {
                            let px = glyph_pos.0 + x as usize;
                            let py = glyph_pos.1 + y as usize;
                            image[(px, py)] = v;
                        }
                    });
                }
                ImageData::Color(image) => {
                    glyph.draw(|x, y, v| {
                        if v > 0.0 {
                            let px = glyph_pos.0 + x as usize;
                            let py = glyph_pos.1 + y as usize;
                            let gamma = 1.0;
                            let a = crate::image::fast_round(v.powf(gamma / 2.2) * 255.0);
                            image[(px, py)] = Color32::from_rgba_premultiplied(a, a, a, a);
                        }
                    });
                }
            }

            let offset_in_pixels = vec2(bb.min.x as f32, scale_in_pixels + bb.min.y as f32);
            let offset = offset_in_pixels / pixels_per_point + y_offset * Vec2::Y;
            UvRect {
                offset,
                size: vec2(glyph_width as f32, glyph_height as f32) / pixels_per_point,
                min: [glyph_pos.0 as u16, glyph_pos.1 as u16],
                max: [
                    (glyph_pos.0 + glyph_width) as u16,
                    (glyph_pos.1 + glyph_height) as u16,
                ],
            }
        }
    });
    let uv_rect = uv_rect.unwrap_or_default();

    let advance_width_in_points =
        font.as_scaled(scale_in_pixels).h_advance(glyph_id) / pixels_per_point;

    GlyphInfo {
        id: glyph_id,
        advance_width: advance_width_in_points,
        uv_rect,
    }
}

fn allocate_native_glyph(
    atlas: &mut TextureAtlas,
    name: &String,
    c: char,
    scale_in_pixels: f32,
    y_offset: f32,
    pixels_per_point: f32,
) -> GlyphInfo {
    use wasm_bindgen::JsCast;

    let document = web_sys::window().unwrap().document().unwrap();
    let canvas = document.create_element("canvas").unwrap();
    let canvas: web_sys::HtmlCanvasElement =
        canvas.dyn_into::<web_sys::HtmlCanvasElement>().unwrap();

    let context = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .unwrap();

    context.set_font(format!("100 {}px {}", scale_in_pixels, name).as_str());

    let metrics = context.measure_text(c.to_string().as_str()).unwrap();

    let glyph_width = (((metrics.actual_bounding_box_left().abs()
        + metrics.actual_bounding_box_right().abs()) as f32)
        .ceil()
        + 1.0) as usize;
    let glyph_height = ((metrics.actual_bounding_box_ascent().abs() as f32
        + metrics.actual_bounding_box_descent().abs() as f32
        + (scale_in_pixels / 4.0)) // Many emoji seem to report incorrect descent metrics, this prevents them from getting cut off
        .ceil()
        + 1.0) as usize;

    canvas.set_width(glyph_width as u32);
    canvas.set_height(glyph_height as u32);

    let context = canvas
        .get_context("2d")
        .unwrap()
        .unwrap()
        .dyn_into::<web_sys::CanvasRenderingContext2d>()
        .unwrap();

    context.set_font(format!("100 {}px {}", scale_in_pixels, name).as_str());
    context.set_fill_style(&"white".into());

    let uv_rect = if glyph_width == 0 || glyph_height == 0 {
        UvRect::default()
    } else {
        let (glyph_pos, image) = atlas.allocate((glyph_width, glyph_height));

        match image {
            ImageData::Font(_image) => {}
            ImageData::Color(image) => {
                context
                    .fill_text(
                        c.to_string().as_str(),
                        0.0,
                        metrics.actual_bounding_box_ascent() + 1.0,
                    )
                    .unwrap();

                let data_url = canvas.to_data_url_with_type("image/png").unwrap();
                let data = data_url.strip_prefix("data:image/png;base64,").unwrap();
                let image_bytes = base64::decode(data).unwrap();
                let loaded_image = image::load_from_memory(&image_bytes).unwrap();
                let glyph_image = ColorImage::from_rgba_unmultiplied(
                    [loaded_image.width() as _, loaded_image.height() as _],
                    loaded_image.to_rgba8().as_flat_samples().as_slice(),
                );

                for y in 0..glyph_height {
                    for x in 0..glyph_width {
                        let px = glyph_pos.0 + x as usize;
                        let py = glyph_pos.1 + y as usize;
                        image[(px, py)] = glyph_image[(x, y)];
                    }
                }
            }
        }

        let offset_in_pixels = vec2(
            0.0,
            scale_in_pixels - metrics.actual_bounding_box_ascent() as f32,
        );
        let offset = offset_in_pixels / pixels_per_point + y_offset * Vec2::Y;

        UvRect {
            offset,
            size: vec2(glyph_width as f32, glyph_height as f32) / pixels_per_point,
            min: [glyph_pos.0 as u16, glyph_pos.1 as u16],
            max: [
                (glyph_pos.0 + glyph_width) as u16,
                (glyph_pos.1 + glyph_height) as u16,
            ],
        }
    };

    let advance_width_in_points = metrics.width() as f32 / pixels_per_point;

    GlyphInfo {
        id: ab_glyph::GlyphId(0),
        advance_width: advance_width_in_points,
        uv_rect,
    }
}
