//! The pixels behind a preview script's `OffscreenCanvas`es and
//! `ImageBitmap`s, drawn with tiny-skia.
//!
//! The 2D context itself — its state, its transform, its path — is the
//! prelude's, in JavaScript. What reaches here is already in device space:
//! a path's points transformed as they were added, as the canvas spec has
//! it, and the clip as the paths that make it up. So each call is one
//! drawing operation, start to finish, and nothing here keeps state between
//! them but the pixels.
//!
//! Composite operations other than `source-over` are drawn the way a browser
//! draws them: the shape onto a transparent layer the size of the canvas,
//! and the layer composited onto the canvas everywhere the clip allows. So
//! `destination-in` clears what the shape doesn't cover, as it should.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, OnceLock};

use parking_lot::Mutex;
use resvg::tiny_skia::{
    self, BlendMode, Color, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint, Path, PathBuilder,
    Pattern, Pixmap, PixmapPaint, Point, RadialGradient, Rect, Shader, SpreadMode, Stroke, StrokeDash, Transform,
};
use resvg::usvg::{self, fontdb, ImageHrefResolver, ImageKind};
use serde::Deserialize;

use crate::html::escape;

/// The largest canvas a script may make, a side.
pub const MAX_CANVAS: u32 = 2400;
/// The largest picture decoded, a side. A small file can be a huge picture.
pub const MAX_PICTURE: u32 = 4096;
/// The pixels one run's canvases and bitmaps may hold between them, at four
/// bytes each: 64 MB.
const MAX_PIXELS: u64 = 16 << 20;

/// The fonts installed on the system, read once, the first time anything
/// asks: by [`fonts`] when a worker starts. Every file is mapped then, rather
/// than copied — an emoji font is ten megabytes — or opened when first drawn
/// with, since a worker can't open files once it's confined.
static FONTS: OnceLock<Arc<fontdb::Database>> = OnceLock::new();

/// Families a generic one stands for when fontconfig doesn't name one that's
/// installed, the likeliest first. Then any family at all.
const SERIF: [&str; 3] = ["DejaVu Serif", "Noto Serif", "Liberation Serif"];
const SANS_SERIF: [&str; 3] = ["DejaVu Sans", "Noto Sans", "Liberation Sans"];
const MONOSPACE: [&str; 3] = ["DejaVu Sans Mono", "Noto Sans Mono", "Liberation Mono"];

/// The system's fonts, with every generic family — what `48px sans-serif`
/// asks for — standing for one that's installed. Blocking, the first time.
pub fn fonts() -> Arc<fontdb::Database> {
    FONTS
        .get_or_init(|| {
            let mut db = fontdb::Database::new();
            db.load_system_fonts();
            type Set = fn(&mut fontdb::Database, String);
            let generics: [(fontdb::Family<'_>, &[&str], Set); 5] = [
                (fontdb::Family::Serif, &SERIF, fontdb::Database::set_serif_family),
                (fontdb::Family::SansSerif, &SANS_SERIF, fontdb::Database::set_sans_serif_family),
                (fontdb::Family::Monospace, &MONOSPACE, fontdb::Database::set_monospace_family),
                (fontdb::Family::Cursive, &SANS_SERIF, fontdb::Database::set_cursive_family),
                (fontdb::Family::Fantasy, &SANS_SERIF, fontdb::Database::set_fantasy_family),
            ];
            let ids: Vec<fontdb::ID> = db.faces().map(|face| face.id).collect();
            for id in ids {
                // Every face of a file shares its mapping, so a collection's
                // first face maps the rest.
                // SAFETY: a font file changed on disk under its mapping is
                // the hazard fontdb's own lazy mapping has; the system's
                // fonts change only when packages are installed.
                let _ = unsafe { db.make_shared_face_data(id) };
            }
            for (generic, likely, set) in generics {
                if installed(&db, generic) {
                    continue;
                }
                let family = likely
                    .iter()
                    .find(|name| installed(&db, fontdb::Family::Name(name)))
                    .map(|name| name.to_string())
                    .or_else(|| db.faces().next().and_then(|face| face.families.first()).map(|(family, _)| family.clone()));
                if let Some(family) = family {
                    set(&mut db, family);
                }
            }
            Arc::new(db)
        })
        .clone()
}

fn installed(db: &fontdb::Database, family: fontdb::Family<'_>) -> bool {
    db.query(&fontdb::Query { families: &[family], ..Default::default() }).is_some()
}

fn font_options() -> usvg::Options<'static> {
    let db = fonts();
    let serif = db.family_name(&fontdb::Family::Serif).to_string();
    let font_resolver = usvg::FontResolver { select_font: usvg::FontResolver::default_font_selector(), select_fallback: Box::new(fallback) };
    usvg::Options { font_family: serif, fontdb: db, font_resolver, ..Default::default() }
}

/// The faces characters were found in, by the character and the face that
/// lacked it. Finding one looks through every face installed, and a system
/// can have thousands: an emoji took longer than everything else a preview
/// draws.
static FALLBACKS: LazyLock<Mutex<Fallbacks>> = LazyLock::new(Default::default);
type Fallbacks = HashMap<(char, fontdb::ID), Option<fontdb::ID>>;
const MAX_FALLBACKS: usize = 4096;

/// usvg's choice of a face for a character the one asked for lacks,
/// remembered.
fn fallback(c: char, used: &[fontdb::ID], db: &mut Arc<fontdb::Database>) -> Option<fontdb::ID> {
    let search = usvg::FontResolver::default_fallback_selector();
    let Some(&lacking) = used.first() else { return search(c, used, db) };
    if let Some(&found) = FALLBACKS.lock().get(&(c, lacking)) {
        // One already tried for this character is no answer.
        if found.is_none_or(|id| !used.contains(&id)) {
            return found;
        }
    }
    let found = search(c, used, db);
    let mut fallbacks = FALLBACKS.lock();
    if fallbacks.len() >= MAX_FALLBACKS {
        fallbacks.clear();
    }
    fallbacks.insert((c, lacking), found);
    found
}

/// What every drawing operation is drawn under.
#[derive(Debug, Deserialize)]
pub struct State {
    pub alpha: f32,
    pub op: String,
    #[serde(default)]
    pub clips: Vec<Clip>,
    #[serde(default = "yes")]
    pub smoothing: bool,
    #[serde(default)]
    pub quality: String,
}

fn yes() -> bool {
    true
}

/// One path the clip is intersected with, in device space.
#[derive(Debug, Deserialize)]
pub struct Clip {
    pub path: Vec<f64>,
    pub rule: String,
}

/// What a fill or stroke is painted with.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Style {
    Color(String),
    Linear { points: [f32; 4], stops: Vec<(f32, String)>, matrix: [f32; 6] },
    Radial { points: [f32; 6], stops: Vec<(f32, String)>, matrix: [f32; 6] },
}

#[derive(Debug, Deserialize)]
pub struct Line {
    pub width: f32,
    pub cap: String,
    pub join: String,
    pub miter: f32,
    #[serde(default)]
    pub dash: Vec<f32>,
    #[serde(default)]
    pub offset: f32,
}

/// Text as `fillText` or `strokeText` draws it.
#[derive(Debug, Deserialize)]
pub struct Text {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub font: Font,
    pub align: String,
    pub baseline: String,
    pub matrix: [f32; 6],
    #[serde(default)]
    pub max_width: Option<f32>,
    /// Stroked rather than filled, at this width.
    #[serde(default)]
    pub stroke: Option<f32>,
}

#[derive(Debug, Deserialize)]
pub struct Font {
    pub family: String,
    pub size: f32,
    pub weight: String,
    pub style: String,
}

#[derive(Debug, PartialEq)]
pub struct Metrics {
    pub width: f32,
    pub left: f32,
    pub right: f32,
    pub ascent: f32,
    pub descent: f32,
}

/// The surfaces of one run: canvases and bitmaps alike, by number.
#[derive(Default)]
pub struct Surfaces {
    entries: Vec<Option<Pixmap>>,
    pixels: u64,
}

type Result<T> = std::result::Result<T, String>;

impl Surfaces {
    /// Whether a surface this size fits in what's left, asked before its
    /// pixels are made: a picture is drawn into them, which takes time.
    fn fits(&self, width: u32, height: u32) -> Result<()> {
        if self.pixels + u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err("A preview may draw 16 million pixels at most, in all its canvases and images together.".into());
        }
        Ok(())
    }

    fn add(&mut self, pixmap: Pixmap) -> Result<usize> {
        self.fits(pixmap.width(), pixmap.height())?;
        self.pixels += u64::from(pixmap.width()) * u64::from(pixmap.height());
        self.entries.push(Some(pixmap));
        Ok(self.entries.len() - 1)
    }

    /// A new transparent canvas.
    pub fn create(&mut self, width: u32, height: u32) -> Result<usize> {
        if width == 0 || height == 0 || width > MAX_CANVAS || height > MAX_CANVAS {
            return Err(format!("A canvas must be between 1 and {MAX_CANVAS} pixels a side."));
        }
        self.add(Pixmap::new(width, height).expect("a nonzero size"))
    }

    /// Let a surface go, and the pixels it held count for nothing.
    pub fn free(&mut self, id: usize) {
        if let Some(pixmap) = self.entries.get_mut(id).and_then(Option::take) {
            self.pixels -= u64::from(pixmap.width()) * u64::from(pixmap.height());
        }
    }

    pub fn copy(&mut self, id: usize) -> Result<usize> {
        let pixmap = self.get(id)?.clone();
        self.add(pixmap)
    }

    pub fn size(&self, id: usize) -> Result<(u32, u32)> {
        let pixmap = self.get(id)?;
        Ok((pixmap.width(), pixmap.height()))
    }

    fn get(&self, id: usize) -> Result<&Pixmap> {
        self.entries.get(id).and_then(Option::as_ref).ok_or_else(|| "That image has been closed.".into())
    }

    fn get_mut(&mut self, id: usize) -> Result<&mut Pixmap> {
        self.entries.get_mut(id).and_then(Option::as_mut).ok_or_else(|| "That image has been closed.".into())
    }

    pub fn fill(&mut self, id: usize, path: &[f64], rule: &str, style: &Style, state: &State) -> Result<()> {
        let Some(path) = build_path(path) else { return Ok(()) };
        let rule = fill_rule(rule);
        let paint = paint(style)?;
        self.paint_with(id, state, |pixmap, paint_over, mask| {
            let mut paint = paint.clone();
            paint_over(&mut paint);
            pixmap.fill_path(&path, &paint, rule, Transform::identity(), mask);
        })
    }

    pub fn stroke(&mut self, id: usize, path: &[f64], style: &Style, line: &Line, state: &State) -> Result<()> {
        let Some(path) = build_path(path) else { return Ok(()) };
        if !(line.width > 0.0 && line.width.is_finite()) {
            return Ok(());
        }
        let paint = paint(style)?;
        let dash = (!line.dash.is_empty()).then(|| StrokeDash::new(line.dash.clone(), line.offset)).flatten();
        let stroke = Stroke {
            width: line.width,
            miter_limit: line.miter,
            line_cap: match line.cap.as_str() {
                "round" => LineCap::Round,
                "square" => LineCap::Square,
                _ => LineCap::Butt,
            },
            line_join: match line.join.as_str() {
                "round" => LineJoin::Round,
                "bevel" => LineJoin::Bevel,
                _ => LineJoin::Miter,
            },
            dash,
        };
        self.paint_with(id, state, |pixmap, paint_over, mask| {
            let mut paint = paint.clone();
            paint_over(&mut paint);
            pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), mask);
        })
    }

    /// Draw part of one surface onto another: `source` in the source's
    /// pixels, to `target` in the canvas's user space under `matrix`.
    pub fn draw_image(&mut self, id: usize, from: usize, source: [f32; 4], target: [f32; 4], matrix: [f32; 6], state: &State) -> Result<()> {
        let [sx, sy, sw, sh] = source;
        let [dx, dy, dw, dh] = target;
        let Some(rect) = Rect::from_xywh(sx, sy, sw, sh) else { return Ok(()) };
        if dw == 0.0 || dh == 0.0 {
            return Ok(());
        }
        let image = self.get(from)?.clone();
        let quality = if !state.smoothing {
            FilterQuality::Nearest
        } else if state.quality == "high" {
            FilterQuality::Bicubic
        } else {
            FilterQuality::Bilinear
        };
        let transform = matrix_transform(matrix).pre_translate(dx, dy).pre_scale(dw / sw, dh / sh).pre_translate(-sx, -sy);
        self.paint_with(id, state, |pixmap, paint_over, mask| {
            let mut paint = Paint { shader: Pattern::new(image.as_ref(), SpreadMode::Pad, quality, 1.0, Transform::identity()), ..Default::default() };
            paint_over(&mut paint);
            pixmap.fill_rect(rect, &paint, transform, mask);
        })
    }

    /// Pixels as `getImageData` gives them: RGBA, not premultiplied, and
    /// transparent black outside the canvas.
    pub fn get_image_data(&self, id: usize, x: i32, y: i32, width: u32, height: u32) -> Result<Vec<u8>> {
        let pixmap = self.get(id)?;
        if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
            return Err("getImageData's width and height must be positive, and the area 16 million pixels at most.".into());
        }
        let mut out = vec![0; (width * height * 4) as usize];
        for row in 0..height as i32 {
            for col in 0..width as i32 {
                let (px, py) = (x + col, y + row);
                if px < 0 || py < 0 || px >= pixmap.width() as i32 || py >= pixmap.height() as i32 {
                    continue;
                }
                let color = pixmap.pixel(px as u32, py as u32).expect("in bounds").demultiply();
                let at = ((row as u32 * width + col as u32) * 4) as usize;
                out[at..at + 4].copy_from_slice(&[color.red(), color.green(), color.blue(), color.alpha()]);
            }
        }
        Ok(out)
    }

    /// Write pixels as `putImageData` does: as they are, ignoring the
    /// transform, the clip and the composite operation.
    pub fn put_image_data(&mut self, id: usize, data: &[u8], x: i32, y: i32, width: u32, height: u32) -> Result<()> {
        if data.len() as u64 != u64::from(width) * u64::from(height) * 4 {
            return Err("The image data's size doesn't match its width and height.".into());
        }
        let pixmap = self.get_mut(id)?;
        let (canvas_width, canvas_height) = (pixmap.width() as i32, pixmap.height() as i32);
        let pixels = pixmap.pixels_mut();
        for row in 0..height as i32 {
            for col in 0..width as i32 {
                let (px, py) = (x + col, y + row);
                if px < 0 || py < 0 || px >= canvas_width || py >= canvas_height {
                    continue;
                }
                let at = ((row as u32 * width + col as u32) * 4) as usize;
                let [r, g, b, a] = [data[at], data[at + 1], data[at + 2], data[at + 3]];
                pixels[(py * canvas_width + px) as usize] = tiny_skia::ColorU8::from_rgba(r, g, b, a).premultiply();
            }
        }
        Ok(())
    }

    pub fn draw_text(&mut self, id: usize, text: &Text, style: &Style, state: &State) -> Result<()> {
        let Style::Color(color) = style else {
            return Err("Text can only be drawn in a colour, not a gradient.".into());
        };
        let color = parse_color(color)?;
        let (width, height) = self.size(id)?;
        let mut matrix = matrix_transform(text.matrix);
        if let Some(max) = text.max_width.filter(|max| *max > 0.0) {
            let measured = self.measure_text(&text.text, &text.font)?.width;
            if measured > max {
                matrix = matrix.pre_translate(text.x, text.y).pre_scale(max / measured, 1.0).pre_translate(-text.x, -text.y);
            }
        }
        let paint = match text.stroke {
            Some(width) => format!(r#"fill="none" stroke="{}" stroke-width="{width}""#, css_color(color)),
            None => format!(r#"fill="{}""#, css_color(color)),
        };
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}"><text x="{}" y="{}" {} text-anchor="{}" dominant-baseline="{}" transform="matrix({} {} {} {} {} {})" {paint}>{}</text></svg>"#,
            text.x,
            text.y,
            font_attributes(&text.font),
            anchor(&text.align),
            baseline(&text.baseline),
            matrix.sx,
            matrix.ky,
            matrix.kx,
            matrix.sy,
            matrix.tx,
            matrix.ty,
            escape(&text.text),
        );
        let tree = usvg::Tree::from_str(&svg, &font_options()).map_err(|e| format!("The text couldn't be drawn: {e}"))?;
        // Text drawn as most is, opaque over what's there and unclipped, goes
        // straight onto the canvas, sparing a layer the size of it.
        if state.op == "source-over" && state.alpha == 1.0 && state.clips.is_empty() {
            resvg::render(&tree, Transform::identity(), &mut self.get_mut(id)?.as_mut());
            return Ok(());
        }
        let mut layer = Pixmap::new(width, height).expect("a nonzero size");
        resvg::render(&tree, Transform::identity(), &mut layer.as_mut());
        self.composite(id, &layer, state)
    }

    pub fn measure_text(&self, text: &str, font: &Font) -> Result<Metrics> {
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="1" height="1"><text x="0" y="0" {}>{}</text></svg>"#,
            font_attributes(font),
            escape(text)
        );
        let tree = usvg::Tree::from_str(&svg, &font_options()).map_err(|e| format!("The text couldn't be measured: {e}"))?;
        let bounds = tree.root().children().iter().find_map(|node| match node {
            usvg::Node::Text(text) => Some(text.bounding_box()),
            _ => None,
        });
        Ok(match bounds {
            Some(rect) => Metrics { width: rect.right(), left: -rect.left(), right: rect.right(), ascent: -rect.top(), descent: rect.bottom() },
            None => Metrics { width: 0.0, left: 0.0, right: 0.0, ascent: 0.0, descent: 0.0 },
        })
    }

    /// A picture's pixels, as a new surface: PNG, JPEG, GIF (its first
    /// frame), WebP, or SVG. An SVG draws what it holds and nothing it
    /// names elsewhere, as a browser draws one used as an image.
    pub fn decode(&mut self, bytes: Vec<u8>) -> Result<usize> {
        let data = Arc::new(bytes);
        let (svg, width, height) = match picture(data.clone()) {
            Some((_, width, height)) => {
                let svg = format!(
                    r#"<svg xmlns="http://www.w3.org/2000/svg" width="{width}" height="{height}"><image href="picture" width="{width}" height="{height}" preserveAspectRatio="none"/></svg>"#
                );
                (svg, width, height)
            }
            None => {
                let text = std::str::from_utf8(&data).map_err(|_| "The image isn't PNG, JPEG, GIF, WebP or SVG.".to_string())?;
                let tree = usvg::Tree::from_str(text, &self.picture_options(None)).map_err(|_| "The image isn't PNG, JPEG, GIF, WebP or SVG.".to_string())?;
                let size = tree.size().to_int_size();
                (text.to_string(), size.width(), size.height())
            }
        };
        if width == 0 || height == 0 || width > MAX_PICTURE || height > MAX_PICTURE {
            return Err(format!("An image must be between 1 and {MAX_PICTURE} pixels a side."));
        }
        self.fits(width, height)?;
        let tree = usvg::Tree::from_str(&svg, &self.picture_options(Some(data))).map_err(|e| format!("The image couldn't be read: {e}"))?;
        let mut pixmap = Pixmap::new(width, height).expect("a nonzero size");
        let size = tree.size();
        resvg::render(&tree, Transform::from_scale(width as f32 / size.width(), height as f32 / size.height()), &mut pixmap.as_mut());
        self.add(pixmap)
    }

    /// What a picture is drawn with: the system's fonts, `data:` pictures, and
    /// `picture` itself under that name — never a file or anything remote.
    fn picture_options(&self, picture_bytes: Option<Arc<Vec<u8>>>) -> usvg::Options<'static> {
        usvg::Options {
            image_href_resolver: ImageHrefResolver {
                resolve_data: Box::new(|_, data, _| picture(data).map(|(kind, ..)| kind)),
                resolve_string: Box::new(move |href, _| match (&picture_bytes, href) {
                    (Some(bytes), "picture") => picture(bytes.clone()).map(|(kind, ..)| kind),
                    _ => None,
                }),
            },
            ..font_options()
        }
    }

    /// A surface as a file: PNG, or JPEG on black, as a browser makes one
    /// of a canvas with transparent pixels.
    pub fn encode(&self, id: usize, mime: &str, quality: Option<f64>) -> Result<(Vec<u8>, &'static str)> {
        let pixmap = self.get(id)?;
        if mime == "image/jpeg" {
            // Premultiplied colour is the colour over black.
            let rgb: Vec<u8> = pixmap.data().as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
            let quality = quality.filter(|q| (0.0..=1.0).contains(q)).unwrap_or(0.92);
            let mut jpeg = Vec::new();
            jpeg_encoder::Encoder::new(&mut jpeg, (quality * 100.0).round().clamp(1.0, 100.0) as u8)
                .encode(&rgb, pixmap.width() as u16, pixmap.height() as u16, jpeg_encoder::ColorType::Rgb)
                .map_err(|e| e.to_string())?;
            return Ok((jpeg, "image/jpeg"));
        }
        Ok((pixmap.encode_png().map_err(|e| e.to_string())?, "image/png"))
    }

    /// Draw onto a canvas: straight onto it for `source-over`, or onto a
    /// layer composited onto it for anything else. `draw` paints with the
    /// paint it's handed, after letting the closure it gets set the paint's
    /// opacity and blend.
    fn paint_with(&mut self, id: usize, state: &State, draw: impl Fn(&mut Pixmap, &dyn Fn(&mut Paint), Option<&Mask>)) -> Result<()> {
        let blend = blend_mode(&state.op)?;
        let (width, height) = self.size(id)?;
        let mask = clip_mask(width, height, &state.clips);
        if blend == BlendMode::SourceOver {
            let alpha = state.alpha;
            let pixmap = self.get_mut(id)?;
            draw(pixmap, &|paint: &mut Paint| paint.shader.apply_opacity(alpha), mask.as_ref());
            return Ok(());
        }
        let mut layer = Pixmap::new(width, height).expect("a nonzero size");
        draw(&mut layer, &|_: &mut Paint| {}, None);
        self.composite(id, &layer, state)
    }

    /// A layer the size of the canvas composited onto it, everywhere the
    /// clip allows.
    fn composite(&mut self, id: usize, layer: &Pixmap, state: &State) -> Result<()> {
        let blend_mode = blend_mode(&state.op)?;
        let (width, height) = self.size(id)?;
        let mask = clip_mask(width, height, &state.clips);
        let paint = PixmapPaint { opacity: state.alpha, blend_mode, quality: FilterQuality::Nearest };
        self.get_mut(id)?.draw_pixmap(0, 0, layer.as_ref(), &paint, Transform::identity(), mask.as_ref());
        Ok(())
    }
}

/// A path from the prelude's commands: 0 move, 1 line, 2 cubic, 3 quad,
/// 4 close, each followed by its points.
fn build_path(commands: &[f64]) -> Option<Path> {
    let mut builder = PathBuilder::new();
    let mut at = 0;
    let p = |i: usize| commands.get(i).copied().unwrap_or(0.0) as f32;
    while at < commands.len() {
        match commands[at] as u8 {
            0 => {
                builder.move_to(p(at + 1), p(at + 2));
                at += 3;
            }
            1 => {
                builder.line_to(p(at + 1), p(at + 2));
                at += 3;
            }
            2 => {
                builder.cubic_to(p(at + 1), p(at + 2), p(at + 3), p(at + 4), p(at + 5), p(at + 6));
                at += 7;
            }
            3 => {
                builder.quad_to(p(at + 1), p(at + 2), p(at + 3), p(at + 4));
                at += 5;
            }
            _ => {
                builder.close();
                at += 1;
            }
        }
    }
    builder.finish()
}

fn fill_rule(rule: &str) -> FillRule {
    if rule == "evenodd" { FillRule::EvenOdd } else { FillRule::Winding }
}

fn clip_mask(width: u32, height: u32, clips: &[Clip]) -> Option<Mask> {
    let mut mask: Option<Mask> = None;
    for clip in clips {
        let Some(path) = build_path(&clip.path) else {
            // A clip to nothing leaves nothing to draw on.
            return Some(Mask::new(width, height).expect("a nonzero size"));
        };
        match &mut mask {
            Some(mask) => mask.intersect_path(&path, fill_rule(&clip.rule), true, Transform::identity()),
            None => {
                let mut new = Mask::new(width, height).expect("a nonzero size");
                new.fill_path(&path, fill_rule(&clip.rule), true, Transform::identity());
                mask = Some(new);
            }
        }
    }
    mask
}

fn matrix_transform([a, b, c, d, e, f]: [f32; 6]) -> Transform {
    Transform::from_row(a, b, c, d, e, f)
}

fn parse_color(text: &str) -> Result<Color> {
    let color: svgtypes::Color = text.trim().parse().map_err(|_| format!("{text} isn't a colour."))?;
    Ok(Color::from_rgba8(color.red, color.green, color.blue, color.alpha))
}

fn css_color(color: Color) -> String {
    let c = color.to_color_u8();
    format!("rgba({},{},{},{})", c.red(), c.green(), c.blue(), f32::from(c.alpha()) / 255.0)
}

fn paint(style: &Style) -> Result<Paint<'static>> {
    let gradient = |stops: &[(f32, String)]| -> Result<Vec<GradientStop>> {
        stops.iter().map(|(offset, color)| Ok(GradientStop::new(*offset, parse_color(color)?))).collect()
    };
    let shader = match style {
        Style::Color(color) => Shader::SolidColor(parse_color(color)?),
        // No stops paints nothing, one paints its colour everywhere.
        Style::Linear { points: [x0, y0, x1, y1], stops, matrix } => {
            LinearGradient::new(Point::from_xy(*x0, *y0), Point::from_xy(*x1, *y1), gradient(stops)?, SpreadMode::Pad, matrix_transform(*matrix))
                .unwrap_or(Shader::SolidColor(Color::TRANSPARENT))
        }
        Style::Radial { points: [x0, y0, r0, x1, y1, r1], stops, matrix } => {
            RadialGradient::new(Point::from_xy(*x0, *y0), *r0, Point::from_xy(*x1, *y1), *r1, gradient(stops)?, SpreadMode::Pad, matrix_transform(*matrix))
                .unwrap_or(Shader::SolidColor(Color::TRANSPARENT))
        }
    };
    Ok(Paint { shader, anti_alias: true, ..Default::default() })
}

fn blend_mode(op: &str) -> Result<BlendMode> {
    Ok(match op {
        "source-over" => BlendMode::SourceOver,
        "source-in" => BlendMode::SourceIn,
        "source-out" => BlendMode::SourceOut,
        "source-atop" => BlendMode::SourceAtop,
        "destination-over" => BlendMode::DestinationOver,
        "destination-in" => BlendMode::DestinationIn,
        "destination-out" => BlendMode::DestinationOut,
        "destination-atop" => BlendMode::DestinationAtop,
        "lighter" => BlendMode::Plus,
        "copy" => BlendMode::Source,
        "xor" => BlendMode::Xor,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "color-dodge" => BlendMode::ColorDodge,
        "color-burn" => BlendMode::ColorBurn,
        "hard-light" => BlendMode::HardLight,
        "soft-light" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "hue" => BlendMode::Hue,
        "saturation" => BlendMode::Saturation,
        "color" => BlendMode::Color,
        "luminosity" => BlendMode::Luminosity,
        other => return Err(format!("{other} isn't a composite operation.")),
    })
}

fn font_attributes(font: &Font) -> String {
    format!(
        r#"font-family="{}" font-size="{}" font-weight="{}" font-style="{}""#,
        escape(&font.family),
        font.size,
        escape(&font.weight),
        escape(&font.style)
    )
}

fn anchor(align: &str) -> &'static str {
    match align {
        "center" => "middle",
        "right" | "end" => "end",
        _ => "start",
    }
}

fn baseline(baseline: &str) -> &'static str {
    match baseline {
        "top" => "text-before-edge",
        "hanging" => "hanging",
        "middle" => "central",
        "ideographic" => "ideographic",
        "bottom" => "text-after-edge",
        _ => "alphabetic",
    }
}

/// A picture usvg may draw, and its size: one of the formats it decodes,
/// of a size it is safe to.
fn picture(data: Arc<Vec<u8>>) -> Option<(ImageKind, u32, u32)> {
    let size = imagesize::blob_size(&data).ok()?;
    let (width, height) = (u32::try_from(size.width).ok()?, u32::try_from(size.height).ok()?);
    if width > MAX_PICTURE || height > MAX_PICTURE {
        return None;
    }
    let kind = match imagesize::image_type(&data).ok()? {
        imagesize::ImageType::Png => ImageKind::PNG(data),
        imagesize::ImageType::Jpeg => ImageKind::JPEG(data),
        imagesize::ImageType::Gif => ImageKind::GIF(data),
        imagesize::ImageType::Webp => ImageKind::WEBP(data),
        _ => return None,
    };
    Some((kind, width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn surfaces() -> Surfaces {
        Surfaces::default()
    }

    fn state(op: &str) -> State {
        State { alpha: 1.0, op: op.into(), clips: Vec::new(), smoothing: true, quality: String::new() }
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> Vec<f64> {
        vec![0.0, x, y, 1.0, x + w, y, 1.0, x + w, y + h, 1.0, x, y + h, 4.0]
    }

    fn pixel(s: &Surfaces, id: usize, x: i32, y: i32) -> [u8; 4] {
        s.get_image_data(id, x, y, 1, 1).unwrap().try_into().unwrap()
    }

    #[test]
    fn fills_clips_and_composites() {
        let mut s = surfaces();
        let id = s.create(10, 10).unwrap();
        let red = Style::Color("#ff0000".into());
        s.fill(id, &rect(0.0, 0.0, 10.0, 10.0), "nonzero", &red, &state("source-over")).unwrap();
        assert_eq!(pixel(&s, id, 5, 5), [255, 0, 0, 255]);

        // Clipped to the left half.
        let clipped = State { clips: vec![Clip { path: rect(0.0, 0.0, 5.0, 10.0), rule: "nonzero".into() }], ..state("source-over") };
        s.fill(id, &rect(0.0, 0.0, 10.0, 10.0), "nonzero", &Style::Color("blue".into()), &clipped).unwrap();
        assert_eq!(pixel(&s, id, 2, 5), [0, 0, 255, 255]);
        assert_eq!(pixel(&s, id, 8, 5), [255, 0, 0, 255]);

        // destination-in keeps only what the shape covers, as a browser does.
        s.fill(id, &rect(0.0, 0.0, 3.0, 3.0), "nonzero", &red, &state("destination-in")).unwrap();
        assert_eq!(pixel(&s, id, 1, 1), [0, 0, 255, 255]);
        assert_eq!(pixel(&s, id, 8, 8), [0, 0, 0, 0]);

        assert!(s.fill(id, &rect(0.0, 0.0, 1.0, 1.0), "nonzero", &red, &state("nonsense")).is_err());
        assert!(s.fill(id, &rect(0.0, 0.0, 1.0, 1.0), "nonzero", &Style::Color("not a colour".into()), &state("source-over")).is_err());
    }

    #[test]
    fn moves_pixels_both_ways() {
        let mut s = surfaces();
        let id = s.create(4, 4).unwrap();
        let data: Vec<u8> = (0..16).flat_map(|_| [10, 20, 30, 255]).collect();
        s.put_image_data(id, &data, 0, 0, 4, 4).unwrap();
        assert_eq!(s.get_image_data(id, 0, 0, 4, 4).unwrap(), data);
        // Outside the canvas is transparent.
        assert_eq!(s.get_image_data(id, -1, 0, 1, 1).unwrap(), [0, 0, 0, 0]);

        let (png, mime) = s.encode(id, "image/png", None).unwrap();
        assert_eq!(mime, "image/png");
        let copy = s.decode(png).unwrap();
        assert_eq!(pixel(&s, copy, 1, 1), [10, 20, 30, 255]);
        let (jpeg, mime) = s.encode(id, "image/jpeg", Some(0.8)).unwrap();
        assert_eq!((mime, imagesize::blob_size(&jpeg).unwrap().width), ("image/jpeg", 4));

        // Drawn, scaled up, onto a bigger canvas.
        let big = s.create(8, 8).unwrap();
        s.draw_image(big, copy, [0.0, 0.0, 4.0, 4.0], [0.0, 0.0, 8.0, 8.0], [1.0, 0.0, 0.0, 1.0, 0.0, 0.0], &state("source-over")).unwrap();
        assert_eq!(pixel(&s, big, 7, 7), [10, 20, 30, 255]);
    }

    #[test]
    fn draws_svg_but_nothing_it_names() {
        let mut s = surfaces();
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="2"><rect width="4" height="2" fill="lime"/><image href="/etc/passwd" width="4" height="2"/></svg>"#;
        let id = s.decode(svg.to_vec()).unwrap();
        assert_eq!(s.size(id).unwrap(), (4, 2));
        assert_eq!(pixel(&s, id, 1, 1), [0, 255, 0, 255]);
        assert!(s.decode(b"neither".to_vec()).is_err());
    }

    #[test]
    fn is_bounded() {
        let mut s = surfaces();
        assert!(s.create(MAX_CANVAS + 1, 1).is_err());
        assert!(s.create(0, 1).is_err());
        let mut ids = Vec::new();
        let err = loop {
            match s.create(MAX_CANVAS, MAX_CANVAS) {
                Ok(id) => ids.push(id),
                Err(e) => break e,
            }
        };
        assert!(err.contains("16 million"), "{err}");
        s.free(ids[0]);
        assert!(s.create(MAX_CANVAS, MAX_CANVAS).is_ok());
        assert!(s.size(ids[0]).is_err());

        // A picture that wouldn't fit isn't drawn at all.
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="4000"/>"#;
        assert!(s.decode(svg.to_vec()).unwrap_err().contains("16 million"));
    }
}
