use futures_util::future::BoxFuture;
use tokio::sync::Semaphore;

use super::*;
use crate::log::LogLevel;

const SECRET: &str = "0000000000000000000000000000000000000000000000000000000000000003";

/// Relays holding exactly the events a test gives them.
struct Relays(Vec<Event>);

impl EventSource for Relays {
    fn query(&self, filters: Vec<Filter>, _: Vec<String>) -> BoxFuture<'_, Vec<Event>> {
        Box::pin(async move { self.0.iter().filter(|e| filters.iter().any(|f| f.matches(e))).cloned().collect() })
    }
}

struct Site {
    _dir: tempfile::TempDir,
    blobs: BlobStore,
    files: Vec<(String, BlobHash)>,
}

fn site(files: &[(&str, &str)]) -> Site {
    let dir = tempfile::tempdir().unwrap();
    let blobs = BlobStore::allowing_private(dir.path());
    let files = files
        .iter()
        .map(|(path, source)| {
            let sha256 = BlobHash::of(source.as_bytes());
            blobs.put(sha256, source.as_bytes()).unwrap();
            (format!("{DIR}{path}"), sha256)
        })
        .collect();
    Site { _dir: dir, blobs, files }
}

fn script(files: &[(&str, &str)]) -> (Site, Arc<Script>) {
    let site = site(files);
    let loaded = load(&site.files, &site.blobs);
    assert_eq!(loaded.problems, vec![]);
    let script = Arc::new(loaded.script.unwrap());
    (site, script)
}

fn page(url: &str) -> Page {
    Page { url: url.into(), method: "GET".into(), headers: vec![("user-agent".into(), "Twitterbot/1.0".into()), ("accept-language".into(), "fr".into())] }
}

async fn run_with(script: &Arc<Script>, page: Page, events: Vec<Event>, deadline: Duration) -> Result<Ran, String> {
    let env = Env {
        runtime: tokio::runtime::Handle::current(),
        source: Arc::new(Relays(events)),
        http: http_client(),
        relays: Vec::new(),
        log: Logger::new(LogLevel::Error),
        have_image: Arc::new(|_| false),
        now: 1_700_000_000_000,
        deadline,
    };
    let permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
    run(script.clone(), page, env, permit).await
}

async fn run_page(script: &Arc<Script>, url: &str, events: Vec<Event>) -> Result<Ran, String> {
    run_with(script, page(url), events, DEADLINE).await
}

fn previewed(ran: Ran) -> Returned {
    ran.preview.expect("a preview")
}

const PROFILE: &str = r#"
import { name } from "./lib/name.js";

export default {
  async preview(request, { nostr }) {
    const pubkey = new URL(request.url).pathname.slice(1);
    const [profile] = await nostr.query([{ kinds: [0], authors: [pubkey], limit: 1 }]);
    if (!profile) return null;
    const lang = request.headers.get("accept-language");
    return { title: `${name(profile)} (${lang})`, description: "About them.", body: "<p>Hi <script>alert(1)</script></p>", image: "/avatar.png" };
  },
};
"#;

#[tokio::test(flavor = "multi_thread")]
async fn a_handler_previews_what_its_queries_find() {
    let (_site, script) = script(&[("preview.js", PROFILE), ("lib/name.js", "export const name = (e) => JSON.parse(e.content).name;")]);
    let profile = Event::sign(SECRET, 1, 0, vec![], r#"{"name":"Alex"}"#);
    let url = format!("https://ditto.example/{}", profile.pubkey);

    let ran = run_page(&script, &url, vec![profile.clone()]).await.unwrap();
    assert_eq!(ran.vary, ["accept-language"]);
    let version = ran.version.clone();
    let preview = previewed(ran);
    assert_eq!(preview.title.as_deref(), Some("Alex (fr)"));
    assert_eq!(preview.body.as_deref(), Some("<p>Hi <script>alert(1)</script></p>"));
    assert!(matches!(&preview.image, Image::Url(url) if url == "/avatar.png"));

    // The same inputs name the same image; another event, another one.
    assert_eq!(run_page(&script, &url, vec![profile.clone()]).await.unwrap().version, version);
    let renamed = Event::sign(SECRET, 2, 0, vec![], r#"{"name":"Alexander"}"#);
    assert_ne!(run_page(&script, &url, vec![profile, renamed]).await.unwrap().version, version);

    // Nothing found leaves the page as published.
    let ran = run_page(&script, &url, vec![]).await.unwrap();
    assert!(ran.preview.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handler_draws_on_a_canvas() {
    let drawing = r##"
export default {
  async preview(request) {
    const canvas = new OffscreenCanvas(120, 63);
    const ctx = canvas.getContext("2d");
    ctx.fillStyle = "#ff0000";
    ctx.fillRect(0, 0, 120, 63);
    ctx.save();
    ctx.beginPath();
    ctx.arc(60, 31, 20, 0, Math.PI * 2);
    ctx.clip();
    ctx.fillStyle = "blue";
    ctx.fillRect(0, 0, 120, 63);
    ctx.restore();
    const middle = ctx.getImageData(60, 31, 1, 1).data;
    const corner = ctx.getImageData(1, 1, 1, 1).data;
    const later = new URL(request.url).pathname === "/later";
    const image = (async () => {
      const blob = await canvas.convertToBlob({ type: "image/png" });
      const bitmap = await createImageBitmap(blob);
      const copy = new OffscreenCanvas(bitmap.width, bitmap.height);
      copy.getContext("2d").drawImage(bitmap, 0, 0);
      return copy.convertToBlob({ type: "image/jpeg", quality: 0.9 });
    })();
    return { title: `${middle.join(",")} ${corner.join(",")}`, image: later ? image : await canvas.convertToBlob() };
  },
};
"##;
    let (_site, script) = script(&[("preview.js", drawing)]);

    let preview = previewed(run_page(&script, "https://x.example/now", vec![]).await.unwrap());
    assert_eq!(preview.title.as_deref(), Some("0,0,255,255 255,0,0,255"));
    let Image::Bytes(png) = preview.image else { panic!("{:?}", preview.image) };
    assert_eq!(imagesize::image_type(&png).unwrap(), imagesize::ImageType::Png);

    // A promise of an image comes after the fields.
    let preview = previewed(run_page(&script, "https://x.example/later", vec![]).await.unwrap());
    let Image::Pending(image) = preview.image else { panic!("{:?}", preview.image) };
    let jpeg = image.await.unwrap().unwrap();
    assert_eq!(imagesize::blob_size(&jpeg).unwrap(), imagesize::ImageSize { width: 120, height: 63 });
}

#[tokio::test(flavor = "multi_thread")]
async fn the_web_it_runs_in() {
    let web = r#"
export default {
  async preview(request) {
    const url = new URL("/a/b?x=1&y=two#h", request.url);
    const text = new TextDecoder().decode(new TextEncoder().encode("é✓"));
    const blob = new Blob(["ab", new Uint8Array([99])], { type: "text/plain" });
    const headers = new Headers({ "X-One": "1" });
    const signal = AbortSignal.timeout(1);
    await new Promise((resolve) => setTimeout(resolve, 5));
    const parts = [
      url.href, url.searchParams.get("y"), text, btoa("hi"), atob("aGk="), await blob.text(), headers.get("x-one"),
      signal.aborted, Date.now(), new Date().getUTCFullYear(), typeof Math.random(), structuredCloneMissing(),
    ];
    return { title: parts.join("|") };
  },
};
function structuredCloneMissing() {
  try { new Path2D("M0 0"); return "path data"; } catch (e) { return e instanceof TypeError; }
}
"#;
    let (_site, script) = script(&[("preview.js", web)]);
    let preview = previewed(run_page(&script, "https://x.example/npub1", vec![]).await.unwrap());
    assert_eq!(
        preview.title.as_deref(),
        Some("https://x.example/a/b?x=1&y=two#h|two|é✓|aGk=|hi|abc|1|true|1700000000000|2023|number|true")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_handler_is_held_to_its_limits() {
    let (_site, script_) = script(&[("preview.js", "export default { preview() { for (;;) {} } };")]);
    let started = Instant::now();
    let error = run_with(&script_, page("https://x.example/a"), vec![], Duration::from_millis(300)).await.unwrap_err();
    assert!(error.contains("deadline"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));

    let (_site, script_) = script(&[("preview.js", r#"export default { async preview() { await fetch("http://127.0.0.1/"); } };"#)]);
    let error = run_page(&script_, "https://x.example/a", vec![]).await.unwrap_err();
    assert!(error.contains("https://"), "{error}");

    let (_site, script_) = script(&[("preview.js", r#"export default { async preview(r, { nostr }) { await nostr.query([{ kinds: [1] }]); } };"#)]);
    let error = run_page(&script_, "https://x.example/a", vec![]).await.unwrap_err();
    assert!(error.contains("something in particular"), "{error}");

    let (_site, script_) = script(&[("preview.js", r#"export default { preview() { return { title: "x", image: new Uint8Array([1, 2, 3]) }; } };"#)]);
    let error = run_page(&script_, "https://x.example/a", vec![]).await.unwrap_err();
    assert!(error.contains("PNG, JPEG"), "{error}");

    let (_site, script_) = script(&[("preview.js", r#"export default { preview() { return new OffscreenCanvas(5000, 1); } };"#)]);
    let error = run_page(&script_, "https://x.example/a", vec![]).await.unwrap_err();
    assert!(error.contains("2400"), "{error}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deploy_checks_what_it_can() {
    let (_site, good) = script(&[("preview.js", PROFILE), ("lib/name.js", "export const name = () => 1;")]);
    assert_eq!(check(good).await, Ok(()));

    let (_site, missing) = script(&[("preview.js", "export default {};")]);
    assert!(check(missing).await.unwrap_err().contains("export default"));

    let (_site, broken) = script(&[("preview.js", "export default { preview( };")]);
    let error = check(broken).await.unwrap_err();
    assert!(error.contains("SyntaxError"), "{error}");

    let (_site, bare) = script(&[("preview.js", "import { nip19 } from 'nostr-tools'; export default { preview() {} };")]);
    let error = check(bare).await.unwrap_err();
    assert!(error.contains("bundle packages"), "{error}");

    let (_site, escape) = script(&[("preview.js", "import x from '../secrets.js'; export default { preview() {} };")]);
    assert!(check(escape).await.unwrap_err().contains("outside /.npanel/"));

    let loaded = load(&site(&[("notes.txt", "x"), ("lib.js", "export {}")]).files, &BlobStore::allowing_private(tempfile::tempdir().unwrap().path()));
    assert!(loaded.script.is_none());
}

#[test]
fn reads_filters() {
    let pubkey = "a".repeat(64);
    let filters = parse_filters(&format!(r##"[{{"kinds":[16767],"authors":["{pubkey}"],"limit":1}},{{"#e":["x"],"until":5}}]"##)).unwrap();
    assert_eq!(filters[0].limit, Some(1));
    assert_eq!(filters[1].tags[&'e'], vec!["x".to_string()]);
    for (json, error) in [
        (r#"[{"kinds":[1]}]"#, "something in particular"),
        (r#"[{"authors":["npub1"]}]"#, "isn't a valid value"),
        (r#"[{"search":"x","ids":[]}]"#, "isn't a filter field"),
        (r#"{}"#, "array of filters"),
    ] {
        let message = parse_filters(json).unwrap_err();
        assert!(message.contains(error), "{json}: {message}");
    }
}

#[test]
fn selects_the_newest_each_filter_asks_for() {
    let notes: Vec<Event> = (0..5).map(|i| Event::sign(SECRET, 100 + i, 1, vec![], &i.to_string())).collect();
    let author = notes[0].pubkey.clone();
    let filters = vec![Filter { kinds: vec![1], authors: vec![author.clone()], limit: Some(2), ..Default::default() }];
    let picked = select(&filters, notes.clone());
    assert_eq!(picked.iter().map(|e| e.content.as_str()).collect::<Vec<_>>(), ["4", "3"]);
    let unlimited = vec![Filter { kinds: vec![1], authors: vec![author], ..Default::default() }];
    assert_eq!(select(&unlimited, notes).len(), 5);
}
