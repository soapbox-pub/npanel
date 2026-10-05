//! What a site's templates can call, beyond what Jinja has: reading tags,
//! JSON and colours, and drawing a glyph as a mask.
//!
//! None of them fails. Given something missing or malformed they return
//! nothing, or what they were given, so a template copes with whatever its
//! queries found without guarding every call.

use std::sync::Arc;

use base64::Engine;
use minijinja::value::Value;
use minijinja::Environment;
use resvg::tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};
use resvg::usvg;

use super::render::escape;
use super::templates::Fonts;

/// The longest JSON `fromjson` reads. A profile is a few kilobytes.
const MAX_JSON: usize = 256 * 1024;
/// The longest text a glyph mask is drawn from, and the largest it's drawn.
const MAX_GLYPH_TEXT: usize = 32;
const MAX_MASK_SIZE: u32 = 1200;
/// Alpha below this is the edge of a glyph's antialiasing, not the glyph.
const MASK_ALPHA: u8 = 25;

pub fn register(env: &mut Environment<'static>, fonts: Arc<Fonts>) {
    env.add_function("tags", |event: Value, name: String| Value::from(tags(&event, &name)));
    env.add_function("tag", |event: Value, name: String| tags(&event, &name).into_iter().next().unwrap_or(Value::from(())));
    env.add_function("imeta", |tag: Value| imeta(&tag));
    env.add_filter("fromjson", |text: Value| from_json(&text));
    env.add_filter("lighten", |color: Value, amount: f64| shift(&color, amount));
    env.add_filter("darken", |color: Value, amount: f64| shift(&color, -amount));
    env.add_filter("is_dark", |color: Value| color.as_str().and_then(parse_hex).is_some_and(|rgb| luminance(rgb) < 0.2));
    // A colour's hue in degrees, and saturation and lightness in percent,
    // and back: what any other adjustment is made of.
    env.add_filter("hsl", |color: Value| match color.as_str().and_then(parse_hex) {
        Some(rgb) => {
            let (h, s, l) = to_hsl(rgb);
            Value::from(vec![h * 360.0, s * 100.0, l * 100.0])
        }
        None => Value::from(()),
    });
    env.add_function("hsl_color", |h: f64, s: f64, l: f64| {
        let (r, g, b) = from_hsl((h / 360.0).rem_euclid(1.0), (s / 100.0).clamp(0.0, 1.0), (l / 100.0).clamp(0.0, 1.0));
        format!("#{r:02x}{g:02x}{b:02x}")
    });
    // For drawing anything round.
    env.add_global("pi", std::f64::consts::PI);
    env.add_function("sin", |radians: f64| radians.sin());
    env.add_function("cos", |radians: f64| radians.cos());
    env.add_function("glyph_mask", move |text: Value, size: Option<u32>| {
        let Some(text) = text.as_str().filter(|t| !t.is_empty() && t.len() <= MAX_GLYPH_TEXT) else { return Value::from(()) };
        let size = size.unwrap_or(400).clamp(1, MAX_MASK_SIZE);
        let key = (text.to_string(), size);
        if let Some(mask) = fonts.masks.lock().get(&key) {
            return Value::from(mask.clone());
        }
        let mask = glyph_mask(&fonts, text, size);
        fonts.masks.lock().put(key, mask.clone());
        Value::from(mask)
    });
}

/// An event's tags named `name`, each the list it is in the event.
fn tags(event: &Value, name: &str) -> Vec<Value> {
    let Ok(tags) = event.get_attr("tags") else { return Vec::new() };
    let Ok(tags) = tags.try_iter() else { return Vec::new() };
    tags.filter(|tag| tag.get_item(&Value::from(0)).ok().is_some_and(|first| first.as_str() == Some(name))).collect()
}

/// A NIP-92 style tag — `["imeta", "url https://…", "m image/png"]` — as a
/// map of its fields. The first value of a field wins.
fn imeta(tag: &Value) -> Value {
    let mut fields = std::collections::BTreeMap::new();
    if let Ok(entries) = tag.try_iter() {
        for entry in entries.skip(1) {
            if let Some((key, value)) = entry.as_str().and_then(|e| e.split_once(' ')) {
                fields.entry(key.to_string()).or_insert_with(|| value.to_string());
            }
        }
    }
    Value::from_serialize(&fields)
}

fn from_json(text: &Value) -> Value {
    let Some(text) = text.as_str().filter(|t| t.len() <= MAX_JSON) else { return Value::from(()) };
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => Value::from_serialize(&value),
        Err(_) => Value::from(()),
    }
}

/// `#rgb` or `#rrggbb`.
fn parse_hex(color: &str) -> Option<(u8, u8, u8)> {
    let hex = color.trim().strip_prefix('#')?;
    let channel = |s: &str| u8::from_str_radix(s, 16).ok();
    match hex.len() {
        3 => {
            let mut digits = hex.chars().map(|c| c.to_digit(16).map(|d| (d * 17) as u8));
            Some((digits.next()??, digits.next()??, digits.next()??))
        }
        6 if hex.is_ascii() => Some((channel(&hex[0..2])?, channel(&hex[2..4])?, channel(&hex[4..6])?)),
        _ => None,
    }
}

/// A colour with its HSL lightness moved by `amount` percentage points, as
/// Ditto's theme tokens are derived. Anything not a colour comes back as it
/// was.
fn shift(color: &Value, amount: f64) -> Value {
    let Some(rgb) = color.as_str().and_then(parse_hex) else { return color.clone() };
    let (h, s, l) = to_hsl(rgb);
    let (r, g, b) = from_hsl(h, s, (l + amount / 100.0).clamp(0.0, 1.0));
    Value::from(format!("#{r:02x}{g:02x}{b:02x}"))
}

/// WCAG's relative luminance.
fn luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let linear = |c: u8| {
        let c = f64::from(c) / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn to_hsl((r, g, b): (u8, u8, u8)) -> (f64, f64, f64) {
    let (r, g, b) = (f64::from(r) / 255.0, f64::from(g) / 255.0, f64::from(b) / 255.0);
    let (max, min) = (r.max(g).max(b), r.min(g).min(b));
    let l = (max + min) / 2.0;
    if max == min {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn from_hsl(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let byte = |v: f64| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    if s == 0.0 {
        return (byte(l), byte(l), byte(l));
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let channel = |t: f64| {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (byte(channel(h + 1.0 / 3.0)), byte(channel(h)), byte(channel(h - 1.0 / 3.0)))
}

/// `text` drawn in the site's fonts, cropped to a square around what it
/// covers, scaled to `size`, and made a luminance mask — white where the
/// glyph is — as a `data:` PNG for an SVG `<mask>`. `None` when the fonts
/// draw nothing for it.
fn glyph_mask(fonts: &Fonts, text: &str, size: u32) -> Option<String> {
    const CANVAS: u32 = 768;
    let svg = format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{CANVAS}" height="{CANVAS}"><text x="384" y="384" font-size="512" text-anchor="middle" dominant-baseline="central">{}</text></svg>"#,
        escape(text)
    );
    let options = usvg::Options { fontdb: fonts.db(), font_family: fonts.default_family()?, ..Default::default() };
    let tree = usvg::Tree::from_str(&svg, &options).ok()?;
    let mut drawn = Pixmap::new(CANVAS, CANVAS)?;
    resvg::render(&tree, Transform::identity(), &mut drawn.as_mut());

    let (mut left, mut top, mut right, mut bottom) = (CANVAS, CANVAS, 0, 0);
    for (i, pixel) in drawn.pixels().iter().enumerate() {
        if pixel.alpha() > MASK_ALPHA {
            let (x, y) = (i as u32 % CANVAS, i as u32 / CANVAS);
            (left, top, right, bottom) = (left.min(x), top.min(y), right.max(x + 1), bottom.max(y + 1));
        }
    }
    if left >= right || top >= bottom {
        return None;
    }
    // Square, around the middle of what was drawn, so the shape keeps its
    // proportions whatever they are.
    let side = (right - left).max(bottom - top) as f32;
    let (cx, cy) = ((left + right) as f32 / 2.0, (top + bottom) as f32 / 2.0);
    let scale = size as f32 / side;

    let mut mask = Pixmap::new(size, size)?;
    let paint = PixmapPaint { quality: FilterQuality::Bicubic, ..Default::default() };
    let transform = Transform::from_translate(-(cx - side / 2.0), -(cy - side / 2.0)).post_scale(scale, scale);
    mask.draw_pixmap(0, 0, drawn.as_ref(), &paint, transform, None);
    for pixel in mask.data_mut().as_chunks_mut::<4>().0 {
        let alpha = pixel[3];
        *pixel = [alpha, alpha, alpha, 255];
    }
    let png = mask.encode_png().ok()?;
    Some(format!("data:image/png;base64,{}", base64::engine::general_purpose::STANDARD.encode(png)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours() {
        assert_eq!(parse_hex("#fff"), Some((255, 255, 255)));
        assert_eq!(parse_hex("#14161f"), Some((0x14, 0x16, 0x1f)));
        assert_eq!(parse_hex("red"), None);
        assert_eq!(parse_hex("#ééé"), None);
        for rgb in [(0x14, 0x16, 0x1f), (0x7c, 0x3a, 0xed), (255, 255, 255), (0, 0, 0)] {
            let (h, s, l) = to_hsl(rgb);
            assert_eq!(from_hsl(h, s, l), rgb);
        }
        assert_eq!(shift(&Value::from("#000000"), 50.0).as_str(), Some("#808080"));
        assert_eq!(shift(&Value::from("#ffffff"), -100.0).as_str(), Some("#000000"));
        assert_eq!(shift(&Value::from("nope"), 10.0).as_str(), Some("nope"));
        assert!(luminance((0x14, 0x16, 0x1f)) < 0.2);
        assert!(luminance((0xf8, 0xfa, 0xfc)) > 0.2);
    }

    #[test]
    fn reads_tags_and_json() {
        let event = Value::from_serialize(serde_json::json!({ "tags": [["c", "#000", "background"], ["bg", "url https://x.example/a.png", "mode cover"], ["c", "#fff", "text"]] }));
        assert_eq!(tags(&event, "c").len(), 2);
        assert_eq!(tags(&Value::UNDEFINED, "c").len(), 0);
        let bg = tags(&event, "bg").remove(0);
        assert_eq!(imeta(&bg).get_attr("url").unwrap().as_str(), Some("https://x.example/a.png"));
        assert!(imeta(&Value::from(())).get_attr("url").unwrap().is_undefined());
        assert_eq!(from_json(&Value::from(r#"{"name":"a"}"#)).get_attr("name").unwrap().as_str(), Some("a"));
        assert!(from_json(&Value::from("not json")).is_none());
        assert!(from_json(&Value::UNDEFINED).is_none());
    }
}
