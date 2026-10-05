//! A preview script's run, inside the process it runs in (`sandbox.rs`):
//! QuickJS, with a memory limit, a deadline, and the web a preview needs
//! (`prelude.js`). Whatever reaches beyond the process — relays, `fetch`, the
//! log — is a message to the gateway, which decides.
//!
//! The prelude and each script are parsed once a process, and their bytecode
//! loaded after, since parsing a bundle costs more than all the rest of a
//! run.

use std::cell::RefCell;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::os::fd::FromRawFd;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use base64::Engine;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use parking_lot::Mutex;
use rquickjs::function::Opt;
use rquickjs::loader::{ImportAttributes, Loader, Resolver};
use rquickjs::module::{Declared, WriteOptions};
use rquickjs::prelude::Promised;
use rquickjs::{AsyncContext, AsyncRuntime, Ctx, Exception, Function, Module, Object, Promise, TypedArray, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{FramedRead, FramedWrite};

use super::canvas::{self, Font, Line, State, Style, Surfaces, Text};
use super::sandbox::{self, FromWorker, Job, Picture, Returned, ToWorker};
use super::{Timings, MAX_LOG_LINE, MAX_LOG_LINES, PATH};

const PRELUDE: &str = include_str!("prelude.js");

/// What one run's JavaScript may hold, and how deep it may call.
const MEMORY: usize = 64 << 20;
const STACK: usize = 1 << 20;
/// The stack the worker's thread runs on: deep enough for any SVG usvg
/// will parse, nested as deep as it allows.
const THREAD_STACK: usize = 64 << 20;
/// Scripts a worker keeps, compiled.
const MAX_SCRIPTS: usize = 16;

/// Be a worker, on the channel the gateway left on [`sandbox::CHANNEL_FD`],
/// until the gateway goes.
pub fn main() -> ! {
    // SAFETY: the gateway put the channel there; anything else there fails
    // as a socket below, and the worker stops.
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(sandbox::CHANNEL_FD) };
    if stream.set_nonblocking(true).is_err() {
        eprintln!("npanel {} is started by npanel itself, for preview scripts.", sandbox::COMMAND);
        std::process::exit(2);
    }
    // Read while files can be: they're mapped, and stay readable after. The
    // runtime too is made while sockets can be.
    let faces = canvas::fonts().len();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_io().enable_time().build().expect("a runtime");
    let unconfined = sandbox::confine();
    let thread = std::thread::Builder::new()
        .name("npanel-preview".into())
        .stack_size(THREAD_STACK)
        .spawn(move || runtime.block_on(serve(stream, faces, unconfined)));
    if let Ok(thread) = thread {
        let _ = thread.join();
    }
    std::process::exit(0)
}

/// The worker's end of its channel: messages out, and the calls awaiting
/// the gateway's answers.
struct Link {
    out: mpsc::UnboundedSender<Bytes>,
    pending: Mutex<HashMap<u64, oneshot::Sender<(ToWorker, Bytes)>>>,
    calls: AtomicU64,
}

impl Link {
    fn send(&self, message: &FromWorker, payload: &[u8]) {
        let _ = self.out.send(sandbox::encode(message, payload));
    }

    /// Ask the gateway something, and await its answer.
    fn call(&self, ask: impl FnOnce(u64) -> FromWorker) -> oneshot::Receiver<(ToWorker, Bytes)> {
        let call = self.calls.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        self.pending.lock().insert(call, answer);
        self.send(&ask(call), &[]);
        answered
    }
}

/// A script as a worker keeps it: its text, and its bytecode once compiled.
struct Source {
    text: String,
    bytecode: OnceLock<Vec<u8>>,
}

type Control = mpsc::UnboundedReceiver<(ToWorker, Bytes)>;

async fn serve(stream: std::os::unix::net::UnixStream, faces: usize, unconfined: Vec<String>) {
    let Ok(stream) = tokio::net::UnixStream::from_std(stream) else { return };
    let (read, write) = stream.into_split();
    let (mut read, mut write) = (FramedRead::new(read, sandbox::codec()), FramedWrite::new(write, sandbox::codec()));

    let (out, mut outgoing) = mpsc::unbounded_channel::<Bytes>();
    tokio::spawn(async move {
        while let Some(frame) = outgoing.recv().await {
            if write.send(frame).await.is_err() {
                std::process::exit(0);
            }
        }
    });
    let link = Arc::new(Link { out, pending: Mutex::new(HashMap::new()), calls: AtomicU64::new(0) });

    // Answers go to whoever called; the rest is the worker's to act on.
    let (control_tx, mut control) = mpsc::unbounded_channel();
    {
        let link = link.clone();
        tokio::spawn(async move {
            while let Some(Ok(frame)) = read.next().await {
                let Ok((message, payload)) = sandbox::decode::<ToWorker>(frame) else { break };
                match message {
                    ToWorker::Queried { call, .. } | ToWorker::Fetched { call, .. } => {
                        if let Some(caller) = link.pending.lock().remove(&call) {
                            let _ = caller.send((message, payload));
                        }
                    }
                    other => {
                        let _ = control_tx.send((other, payload));
                    }
                }
            }
            // The gateway is gone, or speaking nonsense: either way, done.
            std::process::exit(0);
        });
    }

    link.send(&FromWorker::Ready { faces, unconfined }, &[]);
    let mut scripts: lru::LruCache<String, Arc<Source>> = lru::LruCache::new(NonZeroUsize::new(MAX_SCRIPTS).unwrap());
    while let Some((message, _)) = control.recv().await {
        let ToWorker::Job(job) = message else { continue };
        let script = match scripts.get(&job.script) {
            Some(script) => script.clone(),
            None => {
                link.send(&FromWorker::Want, &[]);
                let Some((ToWorker::Source(text), _)) = control.recv().await else { return };
                let script = Arc::new(Source { text, bytecode: OnceLock::new() });
                scripts.put(job.script.clone(), script.clone());
                script
            }
        };
        execute(&link, &mut control, script, job).await;
    }
}

async fn execute(link: &Arc<Link>, control: &mut Control, script: Arc<Source>, job: Job) {
    let started = Instant::now();
    let mut timings = Timings::default();
    let deadline = job.deadline;
    let limit = Arc::new(AtomicU64::new(deadline.as_millis() as u64));
    let fail = |message: String| link.send(&FromWorker::Failed(message), &[]);
    let runtime = match AsyncRuntime::new() {
        Ok(runtime) => runtime,
        Err(error) => return fail(error.to_string()),
    };
    runtime.set_memory_limit(MEMORY).await;
    runtime.set_max_stack_size(STACK).await;
    {
        let limit = limit.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || started.elapsed().as_millis() as u64 > limit.load(Ordering::Relaxed)))).await;
    }
    runtime.set_loader(Only(script.clone()), Only(script.clone())).await;
    let context = match AsyncContext::full(&runtime).await {
        Ok(context) => context,
        Err(error) => return fail(error.to_string()),
    };

    context
        .async_with(async move |ctx| {
            let page = job.page;
            let url = page.as_ref().map(|p| p.url.clone()).unwrap_or_default();
            let extend = |to: Duration| limit.store(to.as_millis() as u64, Ordering::Relaxed);
            let overran = || started.elapsed().as_millis() as u64 >= limit.load(Ordering::Relaxed);
            let failure = |ctx: &Ctx<'_>, error: rquickjs::Error| {
                if overran() {
                    PAST_DEADLINE.to_string()
                } else {
                    describe(ctx, error)
                }
            };

            timings.setup = started.elapsed();
            // Its top level gets the deadline too, which the interrupt can't
            // enforce on a promise that never settles: nothing runs.
            let start = async {
                let at = Instant::now();
                let natives = natives(&ctx, link.clone(), page.is_some())?;
                let prelude = prelude(&ctx).await?;
                let seed = u32::from_le_bytes(Sha256::digest(url.as_bytes())[..4].try_into().unwrap());
                let hooks: Object = prelude.call((natives, seed, job.now as f64))?;
                timings.prelude = at.elapsed();
                let at = Instant::now();
                let module: Object = Module::import(&ctx, PATH)?.into_future().await?;
                let handler: Value = module.get("default")?;
                timings.module = at.elapsed();
                rquickjs::Result::Ok((hooks, handler))
            };
            let (hooks, handler) = match tokio::time::timeout(deadline, start).await {
                Ok(Ok(started)) => started,
                Ok(Err(error)) => return fail(failure(&ctx, error)),
                Err(_) => return fail(PAST_DEADLINE.into()),
            };
            let Some(page) = page else {
                match exports(&handler) {
                    Ok(routes) => link.send(&FromWorker::Loaded { routes }, &[]),
                    Err(error) => fail(error),
                }
                return;
            };

            let called = Instant::now();
            let call = async {
                let call: Function = hooks.get("call")?;
                let headers = ctx.json_parse(serde_json::to_string(&page.headers).expect("headers serialize"))?;
                let promise: Promise = call.call((handler, page.url.as_str(), page.method.as_str(), headers, deadline.as_millis() as f64))?;
                let out: Object = promise.into_future().await?;
                rquickjs::Result::Ok(out)
            };
            let remaining = deadline.saturating_sub(started.elapsed());
            let out = match tokio::time::timeout(remaining, call).await {
                Ok(Ok(out)) => out,
                Ok(Err(error)) => return fail(failure(&ctx, error)),
                Err(_) => return fail(PAST_DEADLINE.into()),
            };
            timings.handler = called.elapsed();
            timings.heap = heap(&ctx);

            let read = || {
                let headers: Vec<String> = out.get("headers")?;
                if out.get::<_, bool>("none")? {
                    let returned = Returned { none: true, title: None, description: None, body: None, image: Picture::None, extras: None, headers, timings };
                    return rquickjs::Result::Ok((returned, Vec::new(), None));
                }
                let (mut image, mut bytes, mut pending) = (Picture::None, Vec::new(), None);
                if let Some(url) = out.get::<_, Option<String>>("url")? {
                    image = Picture::Url(url);
                } else if let Some(array) = out.get::<_, Option<TypedArray<u8>>>("bytes")? {
                    image = Picture::Bytes;
                    bytes = bytes_of(&array);
                } else if let Some(promise) = out.get::<_, Option<Promise>>("pending")? {
                    image = Picture::Pending;
                    pending = Some(promise);
                }
                let returned = Returned {
                    none: false,
                    title: out.get("title")?,
                    description: out.get("description")?,
                    body: out.get("body")?,
                    image,
                    extras: out.get("extras")?,
                    headers,
                    timings,
                };
                Ok((returned, bytes, pending))
            };
            let (returned, bytes, pending) = match read() {
                Ok(read) => read,
                Err(error) => return fail(failure(&ctx, error)),
            };
            link.send(&FromWorker::Returned(Box::new(returned)), &bytes);

            // The fields have gone; the image follows, if the gateway hasn't
            // one of this version already.
            let Some(pending) = pending else { return };
            let Some((ToWorker::Draw(true), _)) = control.recv().await else { return };
            extend(deadline * 2);
            let remaining = (deadline * 2).saturating_sub(started.elapsed());
            match tokio::time::timeout(remaining, pending.into_future::<Option<TypedArray<u8>>>()).await {
                Ok(Ok(Some(array))) => link.send(&FromWorker::Drawn(true), &bytes_of(&array)),
                Ok(Ok(None)) => link.send(&FromWorker::Drawn(false), &[]),
                Ok(Err(error)) => fail(failure(&ctx, error)),
                Err(_) => fail("The preview's image took too long to draw.".into()),
            }
        })
        .await;
}

const PAST_DEADLINE: &str = "The preview script ran past its deadline.";

/// What a script's default export lists as its routes, having checked it
/// has a handler.
fn exports(handler: &Value<'_>) -> Result<Option<Vec<String>>, String> {
    let shape = || format!("{PATH} must export default {{ routes, preview(request, ctx) }}.");
    let handler = handler.as_object().ok_or_else(shape)?;
    if !handler.get::<_, Value>("preview").is_ok_and(|f| f.is_function()) {
        return Err(shape());
    }
    let routes: Value = handler.get("routes").map_err(|_| shape())?;
    if routes.is_undefined() || routes.is_null() {
        return Ok(None);
    }
    let list = routes.as_array().ok_or("A preview script's routes must be a list of URLPattern pathnames, like \"/:id\".")?;
    list.iter::<Value>()
        .map(|route| route.ok().and_then(|r| r.as_string().and_then(|s| s.to_string().ok())).ok_or_else(|| "A preview script's routes must all be strings.".to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map(Some)
}

/// The prelude's function, from its bytecode once it has been compiled.
async fn prelude<'js>(ctx: &Ctx<'js>) -> rquickjs::Result<Function<'js>> {
    /// The prelude, compiled by the first run.
    static BYTECODE: OnceLock<Vec<u8>> = OnceLock::new();
    let module = match BYTECODE.get() {
        // SAFETY: the bytecode is what this QuickJS wrote of the prelude, below.
        Some(bytecode) => unsafe { Module::load(ctx.clone(), bytecode)? },
        None => {
            let module = Module::declare(ctx.clone(), "npanel:prelude", PRELUDE)?;
            if let Ok(bytecode) = module.write(WriteOptions::default()) {
                let _ = BYTECODE.set(bytecode);
            }
            module
        }
    };
    let (module, evaluated) = module.eval()?;
    evaluated.into_future::<()>().await?;
    module.get("default")
}

/// What a run's JavaScript heap holds now, in bytes.
fn heap(ctx: &Ctx<'_>) -> u64 {
    // SAFETY: `ctx` holds its runtime's lock, and the runtime lives as long
    // as it does.
    let usage = unsafe {
        let mut usage = std::mem::MaybeUninit::uninit();
        rquickjs::qjs::JS_ComputeMemoryUsage(rquickjs::qjs::JS_GetRuntime(ctx.as_raw().as_ptr()), usage.as_mut_ptr());
        usage.assume_init()
    };
    usage.memory_used_size.max(0) as u64
}

/// What a script threw, for a log or a deploy log.
fn describe(ctx: &Ctx<'_>, error: rquickjs::Error) -> String {
    if !matches!(error, rquickjs::Error::Exception) {
        return error.to_string();
    }
    let thrown = ctx.catch();
    if let Some(exception) = thrown.as_exception() {
        let message = match exception.get::<_, String>("name") {
            Ok(name) if !name.is_empty() => format!("{name}: {}", exception.message().unwrap_or_default()),
            _ => exception.message().unwrap_or_default(),
        };
        return match exception.stack().filter(|stack| !stack.trim().is_empty()) {
            Some(stack) => format!("{message}\n{}", stack.trim_end()),
            None => message,
        };
    }
    match thrown.as_string().and_then(|s| s.to_string().ok()) {
        Some(text) => text,
        None => "The preview script threw something that isn't an Error.".into(),
    }
}

fn bytes_of(array: &TypedArray<'_, u8>) -> Vec<u8> {
    // SAFETY: the bytes are copied out at once, while nothing else runs.
    unsafe { array.as_bytes() }.map(<[u8]>::to_vec).unwrap_or_default()
}

/// The script, and nothing else: a preview script imports nothing.
struct Only(Arc<Source>);

impl Resolver for Only {
    fn resolve<'js>(&mut self, _: &Ctx<'js>, base: &str, name: &str, _: Option<ImportAttributes<'js>>) -> rquickjs::Result<String> {
        if base.is_empty() && name == PATH {
            return Ok(PATH.into());
        }
        Err(rquickjs::Error::new_resolving_message(base, name, "a preview script imports nothing; bundle what it needs into it"))
    }
}

impl Loader for Only {
    fn load<'js>(&mut self, ctx: &Ctx<'js>, name: &str, _: Option<ImportAttributes<'js>>) -> rquickjs::Result<Module<'js, Declared>> {
        if name != PATH {
            return Err(rquickjs::Error::new_loading(name));
        }
        if let Some(bytecode) = self.0.bytecode.get() {
            // SAFETY: the bytecode is what this QuickJS wrote of this script,
            // below.
            return unsafe { Module::load(ctx.clone(), bytecode) };
        }
        let module = Module::declare(ctx.clone(), name, self.0.text.as_bytes())?;
        if let Ok(bytecode) = module.write(WriteOptions::default()) {
            let _ = self.0.bytecode.set(bytecode);
        }
        Ok(module)
    }
}

fn thrown(ctx: &Ctx<'_>, message: impl std::fmt::Display) -> rquickjs::Error {
    Exception::throw_type(ctx, &message.to_string())
}

fn parse<T: serde::de::DeserializeOwned>(ctx: &Ctx<'_>, json: &str) -> rquickjs::Result<T> {
    serde_json::from_str(json).map_err(|e| thrown(ctx, format!("npanel's canvas was given something it can't read: {e}")))
}

/// What `prelude.js` is built on. `running` is false for a check, which
/// asks nothing of the world.
fn natives<'js>(ctx: &Ctx<'js>, link: Arc<Link>, running: bool) -> rquickjs::Result<Object<'js>> {
    let natives = Object::new(ctx.clone())?;
    let surfaces = Rc::new(RefCell::new(Surfaces::default()));
    natives.set("utf8Encode", Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| TypedArray::<u8>::new(ctx, text.into_bytes())))?;
    natives.set("utf8Decode", Function::new(ctx.clone(), |bytes: TypedArray<'js, u8>| String::from_utf8_lossy(&bytes_of(&bytes)).into_owned()))?;
    natives.set(
        "btoa",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
            let bytes = text.chars().map(|c| u8::try_from(u32::from(c)).map_err(|_| thrown(&ctx, "btoa only encodes Latin-1 text."))).collect::<rquickjs::Result<Vec<u8>>>()?;
            rquickjs::Result::Ok(base64::engine::general_purpose::STANDARD.encode(bytes))
        }),
    )?;
    natives.set(
        "atob",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, text: String| {
            let compact: String = text.chars().filter(|c| !c.is_ascii_whitespace()).collect();
            let bytes = base64::engine::general_purpose::STANDARD_NO_PAD
                .decode(compact.trim_end_matches('='))
                .map_err(|_| thrown(&ctx, "atob was given something that isn't base64."))?;
            rquickjs::Result::Ok(bytes.into_iter().map(char::from).collect::<String>())
        }),
    )?;
    natives.set(
        "urlParse",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, input: String, base: Opt<String>| {
            let parsed = match base.0 {
                Some(base) => url::Url::parse(&base).and_then(|base| base.join(&input)),
                None => url::Url::parse(&input),
            }
            .map_err(|_| thrown(&ctx, format!("{input} isn't a valid URL.")))?;
            let parts = Object::new(ctx.clone())?;
            let host = parsed.host_str().unwrap_or_default();
            let port = parsed.port().map(|p| p.to_string()).unwrap_or_default();
            parts.set("href", parsed.as_str())?;
            parts.set("origin", parsed.origin().ascii_serialization())?;
            parts.set("protocol", format!("{}:", parsed.scheme()))?;
            parts.set("username", parsed.username())?;
            parts.set("password", parsed.password().unwrap_or_default())?;
            parts.set("host", if port.is_empty() { host.to_string() } else { format!("{host}:{port}") })?;
            parts.set("hostname", host)?;
            parts.set("port", port)?;
            parts.set("pathname", parsed.path())?;
            parts.set("search", parsed.query().map(|q| format!("?{q}")).unwrap_or_default())?;
            parts.set("hash", parsed.fragment().map(|f| format!("#{f}")).unwrap_or_default())?;
            rquickjs::Result::Ok(parts)
        }),
    )?;
    {
        let (link, logged) = (link.clone(), Rc::new(std::cell::Cell::new(0)));
        natives.set(
            "log",
            Function::new(ctx.clone(), move |level: String, message: String| {
                // The gateway holds a run to its limits; this only spares it
                // what it would throw away.
                if !running || logged.get() >= MAX_LOG_LINES {
                    return;
                }
                logged.set(logged.get() + 1);
                let message = crate::site_preview::clamp(&message, MAX_LOG_LINE);
                link.send(&FromWorker::Log { level, message }, &[]);
            }),
        )?;
    }
    natives.set("isColor", Function::new(ctx.clone(), |text: String| text.trim().parse::<svgtypes::Color>().is_ok()))?;

    // Canvases and bitmaps.
    let surface = |ctx: &Ctx<'js>, result: Result<(), String>| result.map_err(|e| thrown(ctx, e));
    {
        let s = surfaces.clone();
        natives.set(
            "canvasNew",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, width: u32, height: u32| s.borrow_mut().create(width, height).map(|id| id as u32).map_err(|e| thrown(&ctx, e))),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set("surfaceFree", Function::new(ctx.clone(), move |id: u32| s.borrow_mut().free(id as usize)))?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "surfaceCopy",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32| s.borrow_mut().copy(id as usize).map(|id| id as u32).map_err(|e| thrown(&ctx, e))),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "surfaceSize",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32| s.borrow().size(id as usize).map(|(w, h)| vec![w, h]).map_err(|e| thrown(&ctx, e))),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "fill",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, path: Vec<f64>, rule: String, style: String, state: String| {
                let (style, state) = (parse::<Style>(&ctx, &style)?, parse::<State>(&ctx, &state)?);
                surface(&ctx, s.borrow_mut().fill(id as usize, &path, &rule, &style, &state))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "stroke",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, path: Vec<f64>, style: String, line: String, state: String| {
                let (style, line, state) = (parse::<Style>(&ctx, &style)?, parse::<Line>(&ctx, &line)?, parse::<State>(&ctx, &state)?);
                surface(&ctx, s.borrow_mut().stroke(id as usize, &path, &style, &line, &state))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "drawImage",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, source: u32, rects: Vec<f64>, matrix: Vec<f64>, state: String| {
                let state = parse::<State>(&ctx, &state)?;
                let (Ok(rects), Ok(matrix)) = (<[f64; 8]>::try_from(rects), <[f64; 6]>::try_from(matrix)) else {
                    return Err(thrown(&ctx, "drawImage was given the wrong number of coordinates."));
                };
                let rects = rects.map(|v| v as f32);
                let source_rect = [rects[0], rects[1], rects[2], rects[3]];
                let target = [rects[4], rects[5], rects[6], rects[7]];
                surface(&ctx, s.borrow_mut().draw_image(id as usize, source as usize, source_rect, target, matrix.map(|v| v as f32), &state))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "getImageData",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, x: f64, y: f64, width: f64, height: f64| {
                let data = s.borrow().get_image_data(id as usize, x as i32, y as i32, width as u32, height as u32).map_err(|e| thrown(&ctx, e))?;
                TypedArray::<u8>::new(ctx, data)
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "putImageData",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, data: TypedArray<'js, u8>, x: f64, y: f64, width: f64, height: f64| {
                surface(&ctx, s.borrow_mut().put_image_data(id as usize, &bytes_of(&data), x as i32, y as i32, width as u32, height as u32))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "drawText",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, text: String, style: String, state: String| {
                let (text, style, state) = (parse::<Text>(&ctx, &text)?, parse::<Style>(&ctx, &style)?, parse::<State>(&ctx, &state)?);
                surface(&ctx, s.borrow_mut().draw_text(id as usize, &text, &style, &state))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "measureText",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, text: String, font: String| {
                let font = parse::<Font>(&ctx, &font)?;
                let m = s.borrow().measure_text(&text, &font).map_err(|e| thrown(&ctx, e))?;
                rquickjs::Result::Ok(vec![m.width, m.left, m.right, m.ascent, m.descent])
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "decode",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, bytes: TypedArray<'js, u8>| {
                s.borrow_mut().decode(bytes_of(&bytes)).map(|id| id as u32).map_err(|e| thrown(&ctx, e))
            }),
        )?;
    }
    {
        let s = surfaces.clone();
        natives.set(
            "encode",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, id: u32, mime: String, quality: f64| {
                let (bytes, mime) = s.borrow().encode(id as usize, &mime, Some(quality)).map_err(|e| thrown(&ctx, e))?;
                let out = Object::new(ctx.clone())?;
                out.set("bytes", TypedArray::<u8>::new(ctx.clone(), bytes)?)?;
                out.set("type", mime)?;
                rquickjs::Result::Ok(out)
            }),
        )?;
    }

    // Time, relays and the network: the last two the gateway's to do.
    natives.set(
        "sleep",
        Function::new(ctx.clone(), |ms: f64| {
            Promised(async move {
                tokio::time::sleep(Duration::from_millis(ms.clamp(0.0, 60_000.0) as u64)).await;
            })
        }),
    )?;
    const DEPLOYING: &str = "Nothing is fetched while a site deploys.";
    const GONE: &str = "The gateway didn't answer.";
    {
        let link = link.clone();
        natives.set(
            "query",
            Function::new(ctx.clone(), move |ctx: Ctx<'js>, filters: String| {
                if !running {
                    return Err(thrown(&ctx, DEPLOYING));
                }
                let answered = link.call(|call| FromWorker::Query { call, filters });
                rquickjs::Result::Ok(Promised(async move {
                    match answered.await {
                        Ok((ToWorker::Queried { result: Ok(events), .. }, _)) => Ok(events),
                        Ok((ToWorker::Queried { result: Err(error), .. }, _)) => Err(thrown(&ctx, error)),
                        _ => Err(thrown(&ctx, GONE)),
                    }
                }))
            }),
        )?;
    }
    natives.set(
        "fetch",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, url: String| {
            if !running {
                return Err(thrown(&ctx, DEPLOYING));
            }
            let answered = link.call(|call| FromWorker::Fetch { call, url });
            rquickjs::Result::Ok(Promised(async move {
                let (fetched, body) = match answered.await {
                    Ok((ToWorker::Fetched { result: Ok(fetched), .. }, body)) => (fetched, body),
                    Ok((ToWorker::Fetched { result: Err(error), .. }, _)) => return Err(thrown(&ctx, error)),
                    _ => return Err(thrown(&ctx, GONE)),
                };
                let out = Object::new(ctx.clone())?;
                out.set("status", fetched.status)?;
                out.set("url", fetched.url)?;
                out.set("headers", ctx.json_parse(serde_json::to_string(&fetched.headers).expect("headers serialize"))?)?;
                out.set("body", TypedArray::<u8>::new(ctx.clone(), body.to_vec())?)?;
                rquickjs::Result::Ok(out)
            }))
        }),
    )?;
    Ok(natives)
}
