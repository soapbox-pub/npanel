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

    // A font is the gateway's to have, not the site's.
    let fonted = site(&[("preview.js", "export default { preview() {} };"), ("Emoji.ttf", "not really a font")]);
    let loaded = load(&fonted.files, &fonted.blobs);
    assert!(loaded.script.is_some());
    assert_eq!(loaded.problems.len(), 1);
    assert_eq!(loaded.problems[0].path, "/.npanel/Emoji.ttf");
    assert!(loaded.problems[0].message.contains("installed on the gateway"), "{}", loaded.problems[0].message);
}

#[tokio::test(flavor = "multi_thread")]
async fn text_is_drawn_in_the_systems_fonts() {
    if canvas::fonts().is_empty() {
        eprintln!("No fonts are installed, so there's no text to draw.");
        return;
    }
    let text = r##"
export default {
  async preview() {
    const canvas = new OffscreenCanvas(200, 100);
    const ctx = canvas.getContext("2d");
    ctx.font = "48px sans-serif";
    ctx.fillStyle = "#000000";
    ctx.textBaseline = "middle";
    ctx.fillText("HHHH", 0, 50);
    const width = ctx.measureText("HHHH").width;
    const inked = ctx.getImageData(0, 0, 200, 100).data.some((v, i) => i % 4 === 3 && v > 0);
    return { title: `${width > 0} ${inked}` };
  },
};
"##;
    let (_site, script) = script(&[("preview.js", text)]);
    let preview = previewed(run_page(&script, "https://x.example/a", vec![]).await.unwrap());
    assert_eq!(preview.title.as_deref(), Some("true true"));
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

/// How long a run of a real site's script takes, a phase at a time: Ditto's,
/// built beside this checkout, or the one in `NPANEL_BENCH_SCRIPT`'s
/// directory. Each page runs `NPANEL_BENCH_RUNS` times (20) one after
/// another, then as many at once as there are cores. Relays answer at once,
/// so what's measured is npanel and the script. The images go in the
/// system's temporary directory, to be looked at.
///
/// `cargo test --release bench_preview -- --ignored --nocapture`
#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_preview() {
    use crate::nostr::nip19;

    let dir = std::env::var("NPANEL_BENCH_SCRIPT").unwrap_or_else(|_| "../ditto/dist/.npanel".into());
    let runs: usize = std::env::var("NPANEL_BENCH_RUNS").ok().and_then(|n| n.parse().ok()).unwrap_or(20);
    let store = tempfile::tempdir().unwrap();
    let blobs = BlobStore::allowing_private(store.path());
    let mut files = Vec::new();
    for entry in walkdir(std::path::Path::new(&dir)) {
        let bytes = std::fs::read(&entry).unwrap();
        let sha256 = BlobHash::of(&bytes);
        blobs.put(sha256, &bytes).unwrap();
        files.push((format!("{DIR}{}", entry.strip_prefix(&dir).unwrap().display()), sha256));
    }
    let loaded = load(&files, &blobs);
    let ditto = Arc::new(loaded.script.unwrap_or_else(|| panic!("no preview.js in {dir}")));
    let (_s, baseline) = script(&[("preview.js", "export default { preview: () => null };")]);
    let emoji = |font: &str| {
        format!(
            r##"export default {{ preview() {{
  const canvas = new OffscreenCanvas(1200, 630);
  const ctx = canvas.getContext("2d");
  ctx.font = '120px {font}';
  ctx.textAlign = "center";
  ctx.textBaseline = "middle";
  ctx.fillText("🌈", 600, 315);
  return {{ title: "emoji", image: canvas.convertToBlob({{ type: "image/jpeg", quality: 0.85 }}) }};
}} }};"##
        )
    };
    let (_s, named) = script(&[("preview.js", &emoji(r#""Noto Color Emoji", serif"#))]);
    let (_s, fallback) = script(&[("preview.js", &emoji("serif"))]);

    let c = |hex: &str, role: &str| vec!["c".to_string(), hex.to_string(), role.to_string()];
    let profile = Event::sign(SECRET, 1, 0, vec![], r#"{"name":"Alex","about":"Building things on Nostr."}"#);
    let pubkey = profile.pubkey.clone();
    let colors = vec![c("#14161f", "background"), c("#f8fafc", "text"), c("#7c3aed", "primary")];
    let active = Event::sign(SECRET, 2, 16767, colors.clone(), "");
    let mut theme_tags = vec![vec!["d".to_string(), "midnight".to_string()], vec!["title".to_string(), "Midnight".to_string()]];
    theme_tags.extend(colors);
    let theme = Event::sign(SECRET, 3, 36767, theme_tags, "");
    let note = Event::sign(SECRET, 4, 1, vec![], "Hello, Nostr! This is a note with a few words in it, for a preview to describe.");
    let moment_tags = ["#ff595e", "#ffca3a", "#8ac926", "#1982c4", "#6a4c93"].iter().map(|hex| vec!["c".to_string(), hex.to_string()]).collect();
    let moment = Event::sign(SECRET, 5, 3367, moment_tags, "🌈");
    let events = vec![profile, active, theme.clone(), note.clone(), moment.clone()];
    let nevent = |e: &Event| nip19::nevent_encode(&e.id, &[], Some(&e.pubkey), Some(e.kind as u32));

    let cases: Vec<(&str, &Arc<Script>, String)> = vec![
        ("baseline: preview() => null", &baseline, "x".into()),
        ("text: \"Noto Color Emoji\"", &named, "x".into()),
        ("text: serif, falling back", &fallback, "x".into()),
        ("ditto: profile, themed", &ditto, nip19::npub_encode(&pubkey)),
        ("ditto: note", &ditto, nevent(&note)),
        ("ditto: theme", &ditto, nip19::naddr_encode(&pubkey, 36767, "midnight", &[])),
        ("ditto: color moment", &ditto, nevent(&moment)),
    ];

    let fonts = Instant::now();
    let faces = canvas::fonts().len();
    println!("\n{faces} font faces loaded in {:.1} ms; {} cores; {} KB of modules\n", ms(fonts.elapsed()), cores(), ditto.modules.values().map(|m| m.len()).sum::<usize>() / 1024);
    println!("{:<30} {:>8} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9} {:>8}", "median ms (p95 total)", "spawn", "setup", "prelude", "module", "handler", "image", "total", "heap KB");
    for (name, script, path) in &cases {
        let mut samples: Vec<Sample> = Vec::new();
        for i in 0..runs {
            let sample = bench_run(script, &format!("https://ditto.example/{path}"), events.clone()).await;
            if i == 0 {
                if let Some((bytes, _)) = &sample.image {
                    let slug: String = name.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { ' ' }).collect();
                    let extension = if imagesize::image_type(bytes).ok() == Some(imagesize::ImageType::Png) { "png" } else { "jpg" };
                    let file = std::env::temp_dir().join(format!("npanel-bench-{}.{extension}", slug.split_whitespace().collect::<Vec<_>>().join("-")));
                    std::fs::write(&file, bytes).unwrap();
                }
            }
            samples.push(sample);
        }
        let med = |f: &dyn Fn(&Sample) -> f64| percentile(samples.iter().map(f).collect(), 0.5);
        println!(
            "{:<30} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>8.1} {:>4.0} ({:>3.0}) {:>8}",
            name,
            med(&|s| ms(s.ran.spawn)),
            med(&|s| ms(s.ran.setup)),
            med(&|s| ms(s.ran.prelude)),
            med(&|s| ms(s.ran.module)),
            med(&|s| ms(s.ran.handler)),
            med(&|s| s.image.as_ref().map_or(0.0, |(_, t)| ms(*t))),
            med(&|s| ms(s.total)),
            percentile(samples.iter().map(|s| ms(s.total)).collect(), 0.95),
            med(&|s| s.ran.heap as f64) as u64 / 1024,
        );
    }

    // As many at once as may run, for throughput.
    let url = format!("https://ditto.example/{}", nevent(&moment));
    let at_once = cores() * 4;
    let runs = Arc::new(Semaphore::new(cores()));
    let started = Instant::now();
    let all = futures_util::future::join_all((0..at_once).map(|_| {
        let (ditto, url, events, runs) = (ditto.clone(), url.clone(), events.clone(), runs.clone());
        async move {
            let _turn = runs.acquire().await.unwrap();
            bench_run(&ditto, &url, events).await
        }
    }))
    .await;
    let elapsed = started.elapsed();
    let totals: Vec<f64> = all.iter().map(|s| ms(s.total)).collect();
    println!(
        "\n{at_once} color moments, {} at a time: {:.0} ms in all, {:.1} a second; each {:.0} ms median, {:.0} ms p95\n",
        cores(),
        ms(elapsed),
        at_once as f64 / elapsed.as_secs_f64(),
        percentile(totals.clone(), 0.5),
        percentile(totals, 0.95),
    );
}

struct Sample {
    ran: Timings,
    /// The image, and how long it took after the handler returned.
    image: Option<(Vec<u8>, Duration)>,
    total: Duration,
}

async fn bench_run(script: &Arc<Script>, url: &str, events: Vec<Event>) -> Sample {
    let started = Instant::now();
    let ran = run_page(script, url, events).await.unwrap();
    let answered = Instant::now();
    let image = match ran.preview.map(|p| p.image) {
        Some(Image::Pending(image)) => image.await.unwrap().map(|bytes| (bytes, answered.elapsed())),
        Some(Image::Bytes(bytes)) => Some((bytes, Duration::ZERO)),
        _ => None,
    };
    Sample { ran: ran.timings, image, total: started.elapsed() }
}

fn walkdir(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let path = entry.unwrap().path();
        if path.is_dir() {
            found.extend(walkdir(&path));
        } else {
            found.push(path);
        }
    }
    found
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

fn cores() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get())
}

fn percentile(mut values: Vec<f64>, at: f64) -> f64 {
    values.sort_by(f64::total_cmp);
    values[((values.len() - 1) as f64 * at).round() as usize]
}
