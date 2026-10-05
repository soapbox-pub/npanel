//! The processes preview scripts run in, and what passes between them and
//! the gateway.
//!
//! A script is a stranger's code, run by QuickJS, a C engine, and drawn with
//! decoders of pictures and SVG. A bug in any of those is a bug in whatever
//! process they run in — and the gateway's holds its database: every TLS and
//! onion key, the ACME account, and what every host serves. So scripts run in
//! processes of their own, the gateway's own binary started again as
//! `npanel preview-worker`, which hold nothing and may do next to nothing:
//! on Linux, no file can be opened, no socket made or connected, no program
//! run and no other process touched (Landlock and seccomp), and what memory
//! they may take is limited. Everything a script asks of the world — relays,
//! `fetch`, its log — is a message to the gateway, which decides, and does it
//! itself. What a script returns is held to every limit again here, as from
//! anyone else.
//!
//! The gateway keeps the deadline too: a process that hasn't answered in time
//! is killed, wherever it is — in a promise that never settles, or a decoder
//! that never returns. One that crashes takes nothing else with it.
//!
//! Processes are kept between runs, one a core at most, so a run seldom pays
//! for starting one, and each keeps the bytecode of the scripts it has run.

use std::io::{BufRead, BufReader};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::codec::LengthDelimitedCodec;

use super::{Page, Timings};
use crate::fields;
use crate::log::{LogLevel, Logger};

/// The command that starts a worker.
pub const COMMAND: &str = "preview-worker";

/// The descriptor a worker's end of its channel is on.
pub const CHANNEL_FD: RawFd = 3;

/// The largest message either side sends: a fetched body or a drawn image,
/// and the JSON beside it.
const MAX_FRAME: usize = 48 << 20;

/// What a worker may take up, besides the files it maps: a run's 64 MB of
/// JavaScript and 64 MB of pixels, with room for drawing's scratch layers and
/// what passes through its channel.
#[cfg(target_os = "linux")]
const MAX_DATA: u64 = 1 << 30;

/// Runs one process does before another takes its place.
const MAX_JOBS: u32 = 1000;
/// How long a process is kept with nothing to do.
const MAX_IDLE: Duration = Duration::from_secs(600);

/// The lines a worker may write to its standard error, and the longest kept:
/// a panic's message, usually.
const MAX_STDERR_LINES: usize = 20;
const MAX_STDERR_LINE: usize = 1024;

/// What a worker is given of the gateway's environment: what finding fonts
/// reads, and nothing the gateway was configured with.
const PASSED_ENV: &[&str] = &[
    "HOME",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_HOME",
    "XDG_CONFIG_DIRS",
    "FONTCONFIG_FILE",
    "FONTCONFIG_PATH",
    "FONTCONFIG_SYSROOT",
    "RUST_BACKTRACE",
];

// --- What passes between them. ----------------------------------------------

/// From the gateway to a worker.
#[derive(Debug, Serialize, Deserialize)]
pub enum ToWorker {
    Job(Job),
    /// The script's text, when the worker asked for it.
    Source(String),
    /// What a query found, as NIP-01 events in JSON, or why there's nothing.
    Queried { call: u64, result: Result<String, String> },
    /// A fetch's response, its body the frame's payload.
    Fetched { call: u64, result: Result<Fetched, String> },
    /// Whether to draw the image the handler promised.
    Draw(bool),
}

/// One run of a script, or, without a page, one load of it to check it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Job {
    /// The script's digest: the worker asks for its text if it hasn't it.
    pub script: String,
    pub page: Option<Page>,
    /// What `Date` says, in milliseconds.
    pub now: i64,
    pub deadline: Duration,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Fetched {
    pub status: u16,
    pub url: String,
    pub headers: Vec<(String, String)>,
}

/// From a worker to the gateway.
#[derive(Debug, Serialize, Deserialize)]
pub enum FromWorker {
    /// Started: the font faces it found, and what confining it couldn't do.
    Ready { faces: usize, unconfined: Vec<String> },
    /// It hasn't the job's script.
    Want,
    Query { call: u64, filters: String },
    Fetch { call: u64, url: String },
    Log { level: String, message: String },
    /// A check's answer: the routes the script lists.
    Loaded { routes: Option<Vec<String>> },
    /// A run's answer: what the handler returned.
    Returned(Box<Returned>),
    /// The job failed, and why.
    Failed(String),
    /// The promised image, its bytes the frame's payload, or `false` for
    /// none.
    Drawn(bool),
}

/// What a handler returned, as the worker read it.
#[derive(Debug, Serialize, Deserialize)]
pub struct Returned {
    /// `null`: the page as published.
    pub none: bool,
    pub title: Option<String>,
    pub description: Option<String>,
    pub body: Option<String>,
    pub image: Picture,
    /// The rest, as JSON.
    pub extras: Option<String>,
    /// The request headers it read.
    pub headers: Vec<String>,
    pub timings: Timings,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Picture {
    None,
    Url(String),
    /// The frame's payload.
    Bytes,
    /// To follow, once drawn, if the gateway asks for it.
    Pending,
}

pub fn codec() -> LengthDelimitedCodec {
    LengthDelimitedCodec::builder().max_frame_length(MAX_FRAME).new_codec()
}

/// A message, as a frame: its JSON's length, its JSON, and its payload.
pub fn encode<T: Serialize>(message: &T, payload: &[u8]) -> Bytes {
    let json = serde_json::to_vec(message).expect("messages serialize");
    let mut frame = BytesMut::with_capacity(4 + json.len() + payload.len());
    frame.put_u32(json.len() as u32);
    frame.put_slice(&json);
    frame.put_slice(payload);
    frame.freeze()
}

pub fn decode<T: DeserializeOwned>(mut frame: BytesMut) -> Result<(T, Bytes), String> {
    if frame.len() < 4 {
        return Err("A message was cut short.".into());
    }
    let length = frame.get_u32() as usize;
    if length > frame.len() {
        return Err("A message was cut short.".into());
    }
    let json = frame.split_to(length);
    let message = serde_json::from_slice(&json).map_err(|e| format!("A message couldn't be read: {e}"))?;
    Ok((message, frame.freeze()))
}

// --- The processes. -------------------------------------------------------

/// The workers: as many running at once as there are cores, and those with
/// nothing to do kept for the next.
pub struct Pool {
    slots: Arc<Semaphore>,
    idle: Mutex<Vec<Process>>,
    /// Whether what the first worker found has been logged.
    announced: Mutex<bool>,
}

static POOL: OnceLock<Pool> = OnceLock::new();
static LOG: OnceLock<Logger> = OnceLock::new();

/// Log through `log` from now on: what workers say, and how they're
/// confined.
pub fn init(log: Logger) {
    let _ = LOG.set(log);
}

fn log() -> Logger {
    *LOG.get_or_init(|| Logger::new(LogLevel::Info))
}

pub fn pool() -> &'static Pool {
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism().map_or(2, |n| n.get());
        Pool { slots: Arc::new(Semaphore::new(cores)), idle: Mutex::new(Vec::new()), announced: Mutex::new(false) }
    })
}

/// A worker process, and the gateway's end of its channel.
pub struct Process {
    child: Option<Child>,
    stream: Option<UnixStream>,
    jobs: u32,
    /// Hasn't yet said it's ready.
    pub fresh: bool,
    idle_since: Instant,
}

impl Process {
    fn alive(&mut self) -> bool {
        self.child.as_mut().is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    }

    /// How it ended, if it has: for a run it ended in the middle of.
    pub fn ended(&mut self) -> Option<String> {
        let status = self.child.as_mut()?.try_wait().ok()??;
        Some(match std::os::unix::process::ExitStatusExt::signal(&status) {
            Some(signal) => format!("killed by signal {signal}"),
            None => format!("exited with {status}"),
        })
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else { return };
        let _ = child.kill();
        // Reaped off the runtime: waiting is a system call, however short.
        let _ = std::thread::Builder::new().name("npanel-reap".into()).spawn(move || child.wait());
    }
}

/// A worker, for one job. Dropped without being given back, its process is
/// killed: whatever happened mid-job, nothing about it can be trusted after.
pub struct Lease {
    process: Option<Process>,
    _slot: OwnedSemaphorePermit,
}

impl Lease {
    pub fn process(&mut self) -> &mut Process {
        self.process.as_mut().expect("leased")
    }

    pub fn take_stream(&mut self) -> UnixStream {
        self.process().stream.take().expect("a channel")
    }

    /// Done with it, its channel clean, for the next job.
    pub fn give_back(mut self, stream: UnixStream) {
        let mut process = self.process.take().expect("leased");
        process.stream = Some(stream);
        process.jobs += 1;
        process.idle_since = Instant::now();
        if process.jobs >= MAX_JOBS {
            return;
        }
        let mut idle = pool().idle.lock();
        idle.retain(|p| p.idle_since.elapsed() < MAX_IDLE);
        idle.push(process);
    }
}

impl Pool {
    /// A worker, once one may run: one kept, or a new one.
    pub async fn lease(&self) -> Result<Lease, String> {
        let slot = self.slots.clone().acquire_owned().await.expect("the pool's semaphore is never closed");
        let kept = {
            let mut idle = self.idle.lock();
            let mut found = None;
            while let Some(mut process) = idle.pop() {
                if process.alive() && process.idle_since.elapsed() < MAX_IDLE {
                    found = Some(process);
                    break;
                }
            }
            found
        };
        let process = match kept {
            Some(process) => process,
            None => tokio::task::spawn_blocking(spawn)
                .await
                .map_err(|e| e.to_string())
                .and_then(|spawned| spawned.map_err(|e| e.to_string()))
                .map_err(|e| format!("No process could be started for the preview script: {e}"))?,
        };
        Ok(Lease { process: Some(process), _slot: slot })
    }

    /// Log what the first worker found, once.
    pub fn announce(&self, faces: usize, unconfined: &[String]) {
        let mut announced = self.announced.lock();
        if std::mem::replace(&mut *announced, true) {
            return;
        }
        let log = log();
        match faces {
            0 => log.warn("link_preview_no_fonts", fields! {}),
            faces => log.info("link_preview_fonts", fields! { "faces" => faces }),
        }
        if !unconfined.is_empty() {
            log.warn("link_preview_unconfined", fields! { "err_msg" => unconfined.join(" ") });
        }
    }
}

fn spawn() -> std::io::Result<Process> {
    let (ours, theirs) = UnixStream::pair()?;
    let mut command = worker_command();
    command.env_clear();
    for name in PASSED_ENV {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    // Local time is UTC, read from nowhere: a worker opens no files.
    command.env("TZ", "UTC0");
    #[cfg(test)]
    command.env(TEST_ENV, "1");
    command.stdin(Stdio::null()).stdout(Stdio::null());
    command.stderr(if cfg!(test) { Stdio::inherit() } else { Stdio::piped() });

    let fd = theirs.as_raw_fd();
    // SAFETY: only async-signal-safe calls, between fork and exec.
    unsafe {
        command.pre_exec(move || {
            if fd == CHANNEL_FD {
                // Already where it goes, but closed on exec, as the gateway
                // opens everything.
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            } else if libc::dup2(fd, CHANNEL_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            // Nothing else of the gateway's crosses exec. It opens
            // everything so already; this is for whatever didn't.
            //
            // There's no PR_SET_PDEATHSIG: it fires when the *thread* that
            // started the worker ends, which for a blocking pool's thread is
            // any time. A worker stops when its channel closes instead, as it
            // does when the gateway goes.
            #[cfg(target_os = "linux")]
            libc::syscall(libc::SYS_close_range, CHANNEL_FD as u32 + 1, u32::MAX, libc::CLOSE_RANGE_CLOEXEC);
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    drop(theirs);

    if let Some(stderr) = child.stderr.take() {
        let pid = child.id();
        let _ = std::thread::Builder::new().name("npanel-worker-stderr".into()).spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok).take(MAX_STDERR_LINES) {
                let line = crate::site_preview::clamp(&line, MAX_STDERR_LINE);
                log().warn("link_preview_worker_said", fields! { "pid" => pid, "line" => line });
            }
        });
    }

    Ok(Process { child: Some(child), stream: Some(ours), jobs: 0, fresh: true, idle_since: Instant::now() })
}

/// The gateway's own binary, as the worker. On Linux it's the binary
/// running, even if an upgrade has replaced the file since.
#[cfg(not(test))]
fn worker_command() -> Command {
    let exe = if cfg!(target_os = "linux") { "/proc/self/exe".into() } else { std::env::current_exe().unwrap_or_else(|_| "npanel".into()) };
    let mut command = Command::new(exe);
    command.arg(COMMAND);
    command
}

/// Under `cargo test` the binary is the test harness, so the worker is a
/// test of its own, which only works as one with [`TEST_ENV`] set.
#[cfg(test)]
pub const TEST_ENV: &str = "NPANEL_PREVIEW_WORKER";

#[cfg(test)]
fn worker_command() -> Command {
    let mut command = Command::new(std::env::current_exe().expect("the test binary"));
    command.args(["link_previews::script::tests::preview_worker", "--exact", "--nocapture", "--test-threads=1", "-q"]);
    command
}

// --- Confinement, from inside the worker. ----------------------------------

/// Take away everything a worker doesn't need, as far as this system allows.
/// What it couldn't, for the gateway's log.
///
/// Called once fonts are read and before any script is: from here on the
/// worker opens no file, makes no socket, starts no program and signals no
/// process but itself.
#[cfg(target_os = "linux")]
pub fn confine() -> Vec<String> {
    let mut unconfined = Vec::new();

    let limit = |resource, value: u64| {
        let limit = libc::rlimit { rlim_cur: value, rlim_max: value };
        // SAFETY: a plain system call on a value it copies.
        unsafe { libc::setrlimit(resource, &limit) == 0 }
    };
    for (name, resource, value) in [
        ("RLIMIT_DATA", libc::RLIMIT_DATA, MAX_DATA),
        ("RLIMIT_CORE", libc::RLIMIT_CORE, 0),
        ("RLIMIT_FSIZE", libc::RLIMIT_FSIZE, 0),
        ("RLIMIT_NOFILE", libc::RLIMIT_NOFILE, 64),
    ] {
        if !limit(resource, value) {
            unconfined.push(format!("{name}: {}", std::io::Error::last_os_error()));
        }
    }

    if let Err(error) = landlock() {
        unconfined.push(format!("Landlock: {error}"));
    }
    if let Err(error) = seccomp() {
        unconfined.push(format!("seccomp: {error}"));
    }
    unconfined
}

#[cfg(not(target_os = "linux"))]
pub fn confine() -> Vec<String> {
    vec!["Only Linux confines a preview script's process; here it is only a process of its own.".into()]
}

/// No file, no TCP, and no signal or abstract socket beyond the worker.
#[cfg(target_os = "linux")]
fn landlock() -> Result<(), String> {
    use landlock::{Access, AccessFs, AccessNet, Ruleset, RulesetAttr, RulesetStatus, Scope, ABI};

    let status = Ruleset::default()
        .handle_access(AccessFs::from_all(ABI::V9))
        .and_then(|r| r.handle_access(AccessNet::from_all(ABI::V9)))
        .and_then(|r| r.scope(Scope::from_all(ABI::V9)))
        .and_then(|r| r.create())
        .and_then(|r| r.restrict_self())
        .map_err(|e| e.to_string())?;
    // Partly is as much as an older kernel's Landlock knows to enforce, and
    // seccomp covers the files and sockets it may not.
    match status.ruleset {
        RulesetStatus::NotEnforced => Err("not enforced: this kernel has no Landlock".into()),
        _ => Ok(()),
    }
}

/// The system calls a worker is refused, whatever Landlock covered: opening
/// files, sockets, programs, other processes, and the kernel's riskier
/// corners. A refusal is an error, `EPERM`, as from anything else denied.
#[cfg(target_os = "linux")]
fn seccomp() -> Result<(), String> {
    use std::collections::BTreeMap;

    use seccompiler::{SeccompAction, SeccompCmpArgLen, SeccompCmpOp, SeccompCondition, SeccompFilter, SeccompRule};

    let arch = std::env::consts::ARCH.try_into().map_err(|_| format!("{} isn't an architecture seccompiler knows", std::env::consts::ARCH))?;
    // SAFETY: getpid has no preconditions.
    let me = unsafe { libc::getpid() } as u64;
    let other = |arg: u8, but: &[u64]| -> Result<Vec<SeccompRule>, String> {
        let conditions = but
            .iter()
            .map(|value| SeccompCondition::new(arg, SeccompCmpArgLen::Dword, SeccompCmpOp::Ne, *value))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| e.to_string())?;
        Ok(vec![SeccompRule::new(conditions).map_err(|e| e.to_string())?])
    };

    let mut rules: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
    #[rustfmt::skip]
    let refused = [
        // Files.
        libc::SYS_openat, libc::SYS_openat2, libc::SYS_open_by_handle_at, libc::SYS_name_to_handle_at,
        libc::SYS_mknodat, libc::SYS_fanotify_init, libc::SYS_inotify_init1,
        // The network.
        libc::SYS_socket, libc::SYS_socketpair, libc::SYS_connect, libc::SYS_bind, libc::SYS_listen, libc::SYS_accept4,
        // Programs and processes.
        libc::SYS_execve, libc::SYS_execveat, libc::SYS_ptrace, libc::SYS_process_vm_readv, libc::SYS_process_vm_writev,
        libc::SYS_tkill, libc::SYS_pidfd_open, libc::SYS_pidfd_getfd, libc::SYS_pidfd_send_signal,
        // The system.
        libc::SYS_mount, libc::SYS_umount2, libc::SYS_pivot_root, libc::SYS_chroot, libc::SYS_unshare, libc::SYS_setns,
        libc::SYS_init_module, libc::SYS_finit_module, libc::SYS_delete_module, libc::SYS_kexec_load, libc::SYS_bpf,
        libc::SYS_perf_event_open, libc::SYS_keyctl, libc::SYS_add_key, libc::SYS_request_key, libc::SYS_userfaultfd,
        // io_uring does all of the above without a system call seccomp sees.
        libc::SYS_io_uring_setup, libc::SYS_io_uring_enter, libc::SYS_io_uring_register,
    ];
    for call in refused {
        rules.insert(call, Vec::new());
    }
    #[cfg(target_arch = "x86_64")]
    for call in [libc::SYS_open, libc::SYS_creat, libc::SYS_mknod, libc::SYS_inotify_init, libc::SYS_accept, libc::SYS_fork, libc::SYS_vfork] {
        rules.insert(call, Vec::new());
    }
    // Signals and limits for itself only: `abort` signals itself, and a
    // limit is set with a pid of 0.
    rules.insert(libc::SYS_kill, other(0, &[0, me])?);
    rules.insert(libc::SYS_tgkill, other(0, &[me])?);
    rules.insert(libc::SYS_prlimit64, other(0, &[0, me])?);

    let filter = SeccompFilter::new(rules, SeccompAction::Allow, SeccompAction::Errno(libc::EPERM as u32), arch).map_err(|e| e.to_string())?;
    let program: seccompiler::BpfProgram = filter.try_into().map_err(|e: seccompiler::BackendError| e.to_string())?;
    seccompiler::apply_filter_all_threads(&program).map_err(|e| e.to_string())
}
