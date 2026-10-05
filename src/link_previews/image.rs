//! A template's SVG made into the JPEG a crawler is pointed at.
//!
//! What the SVG draws from elsewhere — an avatar, a background — is fetched
//! first, then the SVG is drawn off the runtime threads, with only what was
//! fetched: the renderer itself reads no file and opens no connection.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use futures_util::future::join_all;
use regex::Regex;
use resvg::tiny_skia::{Color, Pixmap, Transform};
use resvg::usvg::{self, ImageHrefResolver, ImageKind};
use tokio::sync::Semaphore;

use super::templates::Fonts;
use crate::html::decode_attribute;
use crate::net::PublicResolver;

/// What every preview image is: the size Open Graph images are drawn at.
pub const WIDTH: u32 = 1200;
pub const HEIGHT: u32 = 630;
const QUALITY: u8 = 85;

/// The most images one SVG may draw from elsewhere, how large each may be,
/// and how long they may all take.
const MAX_IMAGES: usize = 16;
const MAX_IMAGE_BYTES: usize = 8 << 20;
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest picture decoded. A small file can be a huge picture, and
/// decoding one takes four bytes a pixel.
const MAX_DIMENSION: usize = 4096;

pub struct Painter {
    http: reqwest::Client,
    /// Renders at once: drawing is CPU work, and a crawl asks for images in
    /// bursts.
    renders: Semaphore,
}

impl Default for Painter {
    fn default() -> Self {
        Self::new()
    }
}

impl Painter {
    pub fn new() -> Painter {
        // An avatar's URL is whatever its author wrote, so it is held to the
        // rule blob downloads follow: public addresses only, on every hop.
        let http = reqwest::Client::builder()
            .dns_resolver(Arc::new(PublicResolver))
            .redirect(reqwest::redirect::Policy::limited(3))
            .user_agent(concat!("npanel/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(FETCH_TIMEOUT)
            .timeout(FETCH_TIMEOUT)
            .build()
            .expect("an HTTP client");
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        Painter { http, renders: Semaphore::new(cores) }
    }

    /// Draw `svg` as a JPEG. `site_file` reads a path of the site's own,
    /// for an `href` like `/logo.png`.
    pub async fn paint<F, Fut>(&self, svg: String, fonts: Arc<Fonts>, site_file: F) -> anyhow::Result<Vec<u8>>
    where
        F: Fn(String) -> Fut,
        Fut: Future<Output = Option<Vec<u8>>>,
    {
        let fetches = hrefs(&svg).into_iter().take(MAX_IMAGES).map(|href| {
            let site_file = &site_file;
            async move {
                let bytes = if href.starts_with('/') && !href.starts_with("//") {
                    site_file(href.clone()).await
                } else if href.starts_with("https://") {
                    self.fetch(&href).await
                } else {
                    None
                };
                bytes.map(|bytes| (href, Arc::new(bytes)))
            }
        });
        let images: HashMap<String, Arc<Vec<u8>>> =
            tokio::time::timeout(FETCH_TIMEOUT, join_all(fetches)).await.unwrap_or_default().into_iter().flatten().collect();

        let _permit = self.renders.acquire().await?;
        tokio::task::spawn_blocking(move || rasterize(&svg, &fonts, images)).await?
    }

    async fn fetch(&self, url: &str) -> Option<Vec<u8>> {
        let mut response = self.http.get(url).send().await.ok()?.error_for_status().ok()?;
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.ok()? {
            body.extend_from_slice(&chunk);
            if body.len() > MAX_IMAGE_BYTES {
                return None;
            }
        }
        Some(body)
    }
}

/// Images drawn already, least recently used first out, bounded by what they
/// take up. Keyed by everything an image is drawn from, so nothing has to
/// invalidate them.
pub struct Painted {
    entries: lru::LruCache<String, bytes::Bytes>,
    bytes: usize,
}

/// What drawn images in memory may take up: a few hundred of them.
const PAINTED_BYTES: usize = 64 << 20;

impl Default for Painted {
    fn default() -> Self {
        Painted { entries: lru::LruCache::unbounded(), bytes: 0 }
    }
}

impl Painted {
    pub fn get(&mut self, key: &str) -> Option<bytes::Bytes> {
        self.entries.get(key).cloned()
    }

    pub fn put(&mut self, key: String, image: bytes::Bytes) {
        self.bytes += key.len() + image.len();
        if let Some((old_key, old)) = self.entries.push(key, image) {
            self.bytes -= old_key.len() + old.len();
        }
        while self.bytes > PAINTED_BYTES {
            let Some((key, old)) = self.entries.pop_lru() else { break };
            self.bytes -= key.len() + old.len();
        }
    }
}

/// Every image `href` an SVG names that isn't a `data:` URL, as its
/// attribute's value reads once unescaped.
fn hrefs(svg: &str) -> Vec<String> {
    static HREF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?:\s|^)(?:xlink:)?href\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap());
    let mut found: Vec<String> = HREF
        .captures_iter(svg)
        .filter_map(|c| c.get(1).or(c.get(2)))
        .map(|m| decode_attribute(m.as_str().trim()))
        .filter(|href| !href.starts_with("data:") && !href.starts_with('#'))
        .collect();
    found.sort();
    found.dedup();
    found
}

/// A picture usvg may draw: one of the formats it decodes, of a size it is
/// safe to.
fn picture(data: Arc<Vec<u8>>) -> Option<ImageKind> {
    let size = imagesize::blob_size(&data).ok()?;
    if size.width > MAX_DIMENSION || size.height > MAX_DIMENSION {
        return None;
    }
    match imagesize::image_type(&data).ok()? {
        imagesize::ImageType::Png => Some(ImageKind::PNG(data)),
        imagesize::ImageType::Jpeg => Some(ImageKind::JPEG(data)),
        imagesize::ImageType::Gif => Some(ImageKind::GIF(data)),
        imagesize::ImageType::Webp => Some(ImageKind::WEBP(data)),
        _ => None,
    }
}

/// Draw an SVG at [`WIDTH`] × [`HEIGHT`], fitted and centred on white, and
/// encode it. Blocking.
fn rasterize(svg: &str, fonts: &Fonts, images: HashMap<String, Arc<Vec<u8>>>) -> anyhow::Result<Vec<u8>> {
    let options = usvg::Options {
        fontdb: fonts.db(),
        font_family: fonts.default_family().unwrap_or_default(),
        // Pictures only, and never from a file: usvg would otherwise read
        // any path an `href` names.
        image_href_resolver: ImageHrefResolver {
            resolve_data: Box::new(|_, data, _| picture(data)),
            resolve_string: Box::new(move |href, _| images.get(href).cloned().and_then(picture)),
        },
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(svg, &options)?;

    let mut pixmap = Pixmap::new(WIDTH, HEIGHT).expect("a nonzero size");
    pixmap.fill(Color::WHITE);
    let size = tree.size();
    let scale = (WIDTH as f32 / size.width()).min(HEIGHT as f32 / size.height());
    let (dx, dy) = ((WIDTH as f32 - size.width() * scale) / 2.0, (HEIGHT as f32 - size.height() * scale) / 2.0);
    resvg::render(&tree, Transform::from_scale(scale, scale).post_translate(dx, dy), &mut pixmap.as_mut());

    // Opaque throughout, on its white ground, so premultiplied is plain.
    let rgb: Vec<u8> = pixmap.data().as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut jpeg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpeg, QUALITY).encode(&rgb, WIDTH as u16, HEIGHT as u16, jpeg_encoder::ColorType::Rgb)?;
    Ok(jpeg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut pixmap = Pixmap::new(width, height).unwrap();
        pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
        pixmap.encode_png().unwrap()
    }

    fn fonts() -> Arc<Fonts> {
        let dir = tempfile::tempdir().unwrap();
        Arc::new(Fonts::new(crate::blobs::BlobStore::allowing_private(dir.path()), Vec::new()))
    }

    #[test]
    fn finds_hrefs() {
        let svg = r##"<svg><image href="https://a.example/x.png?a=1&amp;b=2"/><image xlink:href='/logo.png'/><use href="#shape"/><image href="data:image/png;base64,AA"/><image href="https://a.example/x.png?a=1&amp;b=2"/></svg>"##;
        assert_eq!(hrefs(svg), ["/logo.png", "https://a.example/x.png?a=1&b=2"]);
    }

    #[test]
    fn refuses_huge_pictures() {
        assert!(picture(Arc::new(png(8, 8))).is_some());
        assert!(picture(Arc::new(png(MAX_DIMENSION as u32 + 1, 1))).is_none());
        assert!(picture(Arc::new(b"<svg/>".to_vec())).is_none());
    }

    #[test]
    fn draws_what_was_fetched_and_nothing_else() {
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="120" height="63"><image href="/red.png" width="120" height="63"/><image href="/etc/passwd" width="10" height="10"/></svg>"#;
        let images = HashMap::from([("/red.png".to_string(), Arc::new(png(4, 4)))]);
        let jpeg = rasterize(svg, &fonts(), images).unwrap();
        assert_eq!(imagesize::blob_size(&jpeg).unwrap(), imagesize::ImageSize { width: WIDTH as usize, height: HEIGHT as usize });
        assert!(rasterize("<not svg", &fonts(), HashMap::new()).is_err());
    }

    #[tokio::test]
    async fn paints_site_files() {
        let painter = Painter::new();
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630"><image href="/red.png" width="1200" height="630"/></svg>"#;
        let jpeg = painter.paint(svg.into(), fonts(), |path| async move { (path == "/red.png").then(|| png(2, 2)) }).await.unwrap();
        assert!(jpeg.starts_with(&[0xff, 0xd8]));
    }
}
