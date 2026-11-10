//! FramePlayerX: a native VR video player for the Valve Steam Frame.
//!
//! `frameplayerx [FILE|URL]` runs in the headset through OpenXR.
//! `frameplayerx --preview OUT_DIR [--script FILE] [FILE|URL]` runs without a
//! headset, simulating the head and a controller, and saves screenshots;
//! used for automated UI tests.
//! `frameplayerx --decode-bench FILE` decodes without a headset and prints
//! the decoder and its frame rate.

#[macro_use]
mod i18n;
mod app;
mod bindings;
mod controls;
mod crash;
mod drives;
mod image_cache;
mod jobs;
mod libraries;
mod logger;
mod perf;
mod pin;
mod playback;
mod preview;
mod prober;
mod queue;
mod seektest;
mod services;
mod settings;
mod shuffle;
mod subtitles;
mod ui;
mod watch;
mod world;

use app::{App, FrameInput, LayerDraw};
use fp_render::timing::{self, Series};
use fp_render::{Gpu, Quality, Renderer, VideoParams};
use fp_xr::{
    DashboardRate, QuadSubmit, SessionEvent, XrContext, XrOptions, XrSession, XrStats, fit_layers,
    scaled_eye_size,
};
use settings::{RenderScale, Settings};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

type Error = Box<dyn std::error::Error>;

#[derive(Default)]
struct Args {
    preview: Option<PathBuf>,
    script: Option<PathBuf>,
    size: Option<u32>,
    exit_after: Option<f64>,
    info: Option<String>,
    bench: Option<String>,
    bench_opts: fp_media::bench::BenchOptions,
    seek_test: Option<String>,
    seek_opts: seektest::SeekTestOptions,
    verbose: bool,
    open: Option<String>,
}

const USAGE: &str = "usage: frameplayerx [--verbose] [--exit-after SECONDS] [FILE|URL]
       frameplayerx --preview OUT_DIR [--script FILE] [--size PIXELS] [FILE|URL]
       frameplayerx --info FILE      (print what FramePlayerX detects about a video)
       frameplayerx --decode-bench FILE [--frames N] [--sw] [--seek SECONDS]
                   [--checksum N,N,...]  (decode only; print decoder and fps)
       frameplayerx --seek-test FILE|URL [--seeks N] [--step SECONDS] [--rounds N] [--sw]
                   [--play SECONDS] [--trace-reads] [--read-test SECONDS] [--read-ahead SECONDS]
                   (skip through a video headless; checks buffering and that
                   hardware decoders are given back, and measures reads)
       frameplayerx --version";

fn parse_args() -> Result<Args, String> {
    let mut a = Args::default();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--preview" => a.preview = Some(it.next().ok_or("--preview needs a folder")?.into()),
            "--script" => a.script = Some(it.next().ok_or("--script needs a file")?.into()),
            "--size" => {
                a.size = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .ok_or("--size needs a number")?,
                );
            }
            "--exit-after" => {
                a.exit_after = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .ok_or("--exit-after needs seconds")?,
                );
            }
            "--info" => a.info = Some(it.next().ok_or("--info needs a file")?),
            "--decode-bench" => a.bench = Some(it.next().ok_or("--decode-bench needs a file")?),
            "--frames" => {
                a.bench_opts.frames = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--frames needs a number")?;
            }
            "--sw" => {
                a.bench_opts.hw = fp_media::HwDecode::Off;
                a.seek_opts.hw = fp_media::HwDecode::Off;
            }
            "--seek-test" => {
                a.seek_test = Some(it.next().ok_or("--seek-test needs a file or URL")?);
            }
            "--seeks" => {
                a.seek_opts.seeks = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--seeks needs a number")?;
            }
            "--step" => {
                a.seek_opts.step = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--step needs seconds")?;
            }
            "--rounds" => {
                a.seek_opts.rounds = it
                    .next()
                    .and_then(|s| s.parse().ok())
                    .ok_or("--rounds needs a number")?;
            }
            "--play" => {
                a.seek_opts.play = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .ok_or("--play needs seconds")?,
                );
            }
            "--trace-reads" => a.seek_opts.trace_reads = true,
            "--read-ahead" => {
                a.seek_opts.read_ahead = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .ok_or("--read-ahead needs seconds")?,
                );
            }
            "--read-test" => {
                a.seek_opts.read_test = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .ok_or("--read-test needs seconds")?,
                );
            }
            "--seek" => {
                a.bench_opts.seek = Some(
                    it.next()
                        .and_then(|s| s.parse().ok())
                        .ok_or("--seek needs seconds")?,
                );
            }
            "--checksum" => {
                a.bench_opts.checksum = it
                    .next()
                    .ok_or("--checksum needs frame numbers")?
                    .split(',')
                    .filter_map(|s| s.trim().parse().ok())
                    .collect();
            }
            "-v" | "--verbose" => a.verbose = true,
            "--version" => {
                println!("FramePlayerX {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            s if s.starts_with('-') => return Err(format!("unknown option {s}\n{USAGE}")),
            s => a.open = Some(s.to_string()),
        }
    }
    Ok(a)
}

/// Absolute path for local files given on the command line.
fn location(arg: &str) -> String {
    if arg.contains("://") || arg.starts_with('/') {
        return arg.to_string();
    }
    std::env::current_dir()
        .map(|d| d.join(arg).display().to_string())
        .unwrap_or_else(|_| arg.to_string())
}

fn make_app() -> Result<App, Error> {
    make_app_with(Settings::load(&Settings::path()))
}

/// The app with `settings` already read, for a caller that needed some of
/// them first.
fn make_app_with(settings: Settings) -> Result<App, Error> {
    let library = fp_library::Library::open_default()?;
    Ok(App::new(settings, library))
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    logger::init(args.verbose);
    fp_media::init_logging();
    log::info!("FramePlayerX {} starting", env!("CARGO_PKG_VERSION"));
    if args.bench.is_some() || args.seek_test.is_some() {
        allow_hevc_10bit_headless();
    }
    if let Some(f) = &args.bench {
        std::process::exit(match bench(f, &args.bench_opts) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        });
    }
    if let Some(l) = &args.seek_test {
        std::process::exit(match seektest::run(&location(l), &args.seek_opts) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        });
    }
    if let Some(f) = &args.info {
        std::process::exit(match info(f) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("{e}");
                1
            }
        });
    }
    let r = match &args.preview {
        Some(dir) => preview::run(&args, dir),
        None => run_xr(&args),
    };
    if let Err(e) = r {
        log::error!("{e}");
        eprintln!("FramePlayerX: {e}");
        std::process::exit(1);
    }
}

/// For the headless decode tools: decodes 10-bit HEVC in hardware when a
/// renderer on this GPU could show it, as playback would.
fn allow_hevc_10bit_headless() {
    let shows = Gpu::headless()
        .and_then(|gpu| Renderer::new(Arc::new(gpu), ash::vk::Format::R8G8B8A8_SRGB))
        .is_ok_and(|r| r.shows_q10c());
    fp_media::set_hevc_10bit_hardware(shows);
}

/// Prints container, streams and the detected VR format, and decodes the
/// first frame, without needing a headset or GPU.
fn info(file: &str) -> Result<(), Error> {
    let path = std::path::Path::new(file);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let src = || -> Result<Arc<dyn fp_core::ByteSource>, Error> {
        Ok(Arc::new(fp_core::source::FileSource::open(path)?))
    };
    let i = fp_media::thumb::probe(src()?, &name, Default::default())?;
    println!("{name}: {} · {:.1} s", i.container, i.duration);
    for s in &i.streams {
        let lang = s
            .language
            .as_deref()
            .map(|l| format!(" [{l}]"))
            .unwrap_or_default();
        match s.kind {
            fp_media::info::StreamKind::Video => {
                println!(
                    "  #{} video {} {}x{}{} {:.3} fps {}-bit {} ({}) {:?}{lang}",
                    s.index,
                    s.codec,
                    s.width,
                    s.height,
                    if s.interlaced { " interlaced" } else { "" },
                    s.fps,
                    s.bit_depth,
                    s.chroma(),
                    s.pixel_format,
                    s.transfer
                );
            }
            fp_media::info::StreamKind::Audio => println!(
                "  #{} audio {} {} Hz {} ch{}{lang}",
                s.index,
                s.codec,
                s.sample_rate,
                s.channels,
                if s.ambisonic { " ambisonic" } else { "" }
            ),
            _ => println!("  #{} {:?} {}{lang}", s.index, s.kind, s.codec),
        }
    }
    let (w, h) = i
        .video_stream()
        .map(fp_media::StreamInfo::display_size)
        .unwrap_or((0, 0));
    let d = fp_core::format::resolve(None, i.hints, &name, w, h);
    println!("  format: {} ({})", d.format.label(), d.evidence.label());
    let f = fp_media::thumb::decode_first_frame(src()?, &name)?;
    println!(
        "  first frame: {}x{} {:?} decoded",
        f.width, f.height, f.layout
    );
    Ok(())
}

/// Decodes a file as the player would, without a headset or GPU, and prints
/// the decoder used and its rate.
fn bench(file: &str, opts: &fp_media::bench::BenchOptions) -> Result<(), Error> {
    let path = std::path::Path::new(file);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let src: Arc<dyn fp_core::ByteSource> = Arc::new(fp_core::source::FileSource::open(path)?);
    let r = fp_media::bench::decode_bench(src, &name, opts)?;
    println!(
        "{name}: {}x{} {:?} via {} ({})",
        r.width,
        r.height,
        r.layout,
        r.decoder,
        if r.hardware { "hardware" } else { "software" }
    );
    let line = |label: &str, run: &fp_media::bench::BenchRun| {
        println!(
            "  {label}: {} frames, first after {:.3} s, {:.2} fps (pts {:.3?}..{:.3?})",
            run.frames, run.first_frame_secs, run.fps, run.first_pts, run.last_pts
        );
    };
    line("decode", &r.run);
    if let Some(s) = &r.after_seek {
        line("after seek", s);
    }
    for (i, pts, sum) in &r.checksums {
        println!("  frame {i} pts {pts:.3}: yuv fnv {sum:016x}");
    }
    Ok(())
}

/// Quad layer keys of the two pointer rays (panels use their index).
const RAY_LAYER: u64 = 100;
const RAY_PX: [u32; 2] = [64, 8];

/// The pointer ray's picture: light blue, soft across its width, fainter
/// for the hand that isn't pointing. Premultiplied, as the compositor
/// expects.
fn ray_picture(active: bool) -> Vec<u8> {
    let peak = if active { 0.85 } else { 0.35 };
    let across = [0.12, 0.5, 0.9, 1.0, 1.0, 0.9, 0.5, 0.12];
    let mut px = Vec::with_capacity((RAY_PX[0] * RAY_PX[1] * 4) as usize);
    for a in across {
        let a = a * peak;
        for _ in 0..RAY_PX[0] {
            px.extend([140.0 * a, 190.0 * a, 255.0 * a, 255.0 * a].map(|c: f32| c.round() as u8));
        }
    }
    px
}

/// How long shutdown may take once the session has ended.
const EXIT_WITHIN: Duration = Duration::from_secs(5);

/// Ends the process if shutdown has not finished within `limit`. Progress
/// and settings are saved first; what is left (network teardown, threads
/// in a read) is not worth a process that never exits, which SteamVR shows
/// as a frozen app until it is killed.
fn exit_watchdog(limit: Duration) {
    let _ = std::thread::Builder::new()
        .name("fp-exit".into())
        .spawn(move || {
            std::thread::sleep(limit);
            log::warn!("shutdown took over {} s; exiting", limit.as_secs());
            std::process::exit(0);
        });
}

/// Set for the copy [`run_xr`] starts when it restarts.
const RESTARTED: &str = "FRAMEPLAYERX_RESTARTED";

/// A/B switches for frame pacing, read once at start from the environment
/// (the launcher sources `~/.config/frameplayerx/env`). With none set the
/// app behaves as usual.
#[derive(Debug, Default, PartialEq)]
struct Switches {
    /// `FRAMEPLAYERX_UI_IN_EYES=1`: draw the UI into the eye buffers even
    /// where the runtime could show it as compositor quad layers.
    ui_in_eyes: bool,
    /// `FRAMEPLAYERX_REQUEST_HZ=<rate>`: ask the runtime for this display
    /// refresh rate once the session runs.
    request_hz: Option<f32>,
    /// `FRAMEPLAYERX_RENDER_SCALE=<factor>`: the eye pictures' size against
    /// the runtime's recommended one, instead of Settings › Render
    /// resolution.
    render_scale: Option<f32>,
    /// `FRAMEPLAYERX_BARE_LOOP=<seconds>`: run only a frame loop that clears
    /// the eyes for this long, then exit (no app, panels or quad layers).
    bare_loop: Option<f32>,
    /// `FRAMEPLAYERX_XR_MINIMAL=1`: create the OpenXR instance with only the
    /// extensions it can't do without.
    xr_minimal: bool,
    /// `FRAMEPLAYERX_NO_DASHBOARD_CLOSE=1`: leave SteamVR's dashboard open at
    /// launch instead of closing it.
    no_dashboard_close: bool,
    /// `FRAMEPLAYERX_QUALITY=nodither,nosupersample,noedgeaa`: turn picture
    /// quality features off, to compare a look with and without on one build.
    quality: Quality,
}

impl Switches {
    fn from_env() -> Switches {
        Switches::parse(|name| std::env::var(name).ok())
    }

    /// Reads the switches through `get`, which gives a variable's value.
    fn parse(get: impl Fn(&str) -> Option<String>) -> Switches {
        let on = |name: &str| get(name).is_some_and(|v| !matches!(v.trim(), "" | "0"));
        let number = |name: &str| {
            get(name)
                .and_then(|v| v.trim().parse::<f32>().ok())
                .filter(|n| n.is_finite())
        };
        Switches {
            ui_in_eyes: on("FRAMEPLAYERX_UI_IN_EYES"),
            request_hz: number("FRAMEPLAYERX_REQUEST_HZ").filter(|hz| *hz >= 0.0),
            render_scale: number("FRAMEPLAYERX_RENDER_SCALE").filter(|k| *k > 0.0),
            bare_loop: number("FRAMEPLAYERX_BARE_LOOP").filter(|s| *s > 0.0),
            xr_minimal: on("FRAMEPLAYERX_XR_MINIMAL"),
            no_dashboard_close: on("FRAMEPLAYERX_NO_DASHBOARD_CLOSE"),
            quality: Quality::without(get("FRAMEPLAYERX_QUALITY").as_deref().unwrap_or("")),
        }
    }

    fn describe(&self) -> String {
        let mut on = Vec::new();
        if self.ui_in_eyes {
            on.push("FRAMEPLAYERX_UI_IN_EYES".to_string());
        }
        if let Some(hz) = self.request_hz {
            on.push(format!("FRAMEPLAYERX_REQUEST_HZ={hz}"));
        }
        if let Some(k) = self.render_scale {
            on.push(format!("FRAMEPLAYERX_RENDER_SCALE={k}"));
        }
        if let Some(secs) = self.bare_loop {
            on.push(format!("FRAMEPLAYERX_BARE_LOOP={secs}"));
        }
        if self.xr_minimal {
            on.push("FRAMEPLAYERX_XR_MINIMAL".to_string());
        }
        if self.no_dashboard_close {
            on.push("FRAMEPLAYERX_NO_DASHBOARD_CLOSE".to_string());
        }
        if self.quality != Quality::default() {
            let off = self.quality.off_list();
            on.push(format!("FRAMEPLAYERX_QUALITY={off}"));
        }
        if on.is_empty() {
            "none".into()
        } else {
            on.join(", ")
        }
    }
}

/// What the runtime hands the app each frame, summarised every 10 s. A
/// predicted display period of twice the panel's means the compositor shows
/// every app frame twice.
#[derive(Default)]
struct Pacing {
    /// One loop iteration, from before xrWaitFrame to after xrEndFrame.
    iteration: Series,
    /// `predicted_display_period`.
    period: Series,
    /// Step of `predicted_display_time` between consecutive frames.
    step: Series,
    /// Frames the runtime said not to render.
    skipped: u32,
    last_display_ns: Option<i64>,
    /// Seconds since the loop started and the period (ms) of each of the
    /// first [`STARTUP_FRAMES`] frames.
    early: Vec<(f32, f32)>,
    /// Seconds since the loop started at the first frame the session had
    /// focus.
    focused_at: Option<f32>,
    /// The startup line has been logged.
    startup_logged: bool,
}

/// How many frames the startup line covers (8 s at 72 Hz).
const STARTUP_FRAMES: usize = 600;

/// Runs of consecutive values that stay within `tol` of the first of their
/// run, as that first value and how many there are.
fn runs(values: &[f32], tol: f32) -> Vec<(f32, u32)> {
    let mut out: Vec<(f32, u32)> = Vec::new();
    for &v in values {
        if let Some((first, count)) = out.last_mut()
            && (v - *first).abs() <= tol
        {
            *count += 1;
            continue;
        }
        out.push((v, 1));
    }
    out
}

/// The index of the first period over 1.5 times the shortest one: a frame
/// the runtime held to a slower rate than the display's. None when the
/// periods never change, including when they are slow from the first frame.
fn first_throttled(periods: &[f32]) -> Option<usize> {
    let least = periods.iter().copied().fold(f32::INFINITY, f32::min);
    periods.iter().position(|&p| p > least * 1.5)
}

/// The startup line for `frames` (seconds since the loop started, period in
/// ms): the runs of periods, and when the first throttled frame came.
fn startup_summary(frames: &[(f32, f32)], focused_at: Option<f32>) -> String {
    let periods: Vec<f32> = frames.iter().map(|f| f.1).collect();
    let shown = runs(&periods, 1.0)
        .iter()
        .map(|(ms, count)| format!("{ms:.1}x{count}"))
        .collect::<Vec<_>>()
        .join(", ");
    let throttle = match (first_throttled(&periods), focused_at) {
        (None, _) => "no throttled frame".to_string(),
        (Some(n), Some(focus)) => {
            let after = frames[n].0 - focus;
            format!("first throttled frame {n} at {after:.1} s after FOCUSED")
        }
        (Some(n), None) => {
            let at = frames[n].0;
            format!("first throttled frame {n} at {at:.1} s, before FOCUSED")
        }
    };
    let count = frames.len();
    format!("pacing startup: {count} frames, period runs [{shown}]; {throttle}")
}

impl Pacing {
    /// Notes a frame for the startup line, which comes back once the first
    /// [`STARTUP_FRAMES`] are in (or from [`Pacing::log`] if fewer are by the
    /// first 10 s summary). `secs` is the time since the loop started.
    fn startup(&mut self, secs: f32, period_ns: i64, focused: bool) -> Option<String> {
        if self.startup_logged {
            return None;
        }
        if focused && self.focused_at.is_none() {
            self.focused_at = Some(secs);
        }
        self.early.push((secs, period_ns as f32 / 1e6));
        (self.early.len() >= STARTUP_FRAMES).then(|| self.take_startup_line())
    }

    fn take_startup_line(&mut self) -> String {
        self.startup_logged = true;
        startup_summary(&self.early, self.focused_at)
    }

    fn frame(&mut self, period_ns: i64, display_ns: i64, should_render: bool) {
        self.period.push(period_ns as f32 / 1e6);
        if let Some(last) = self.last_display_ns.replace(display_ns) {
            self.step.push((display_ns - last) as f32 / 1e6);
        }
        self.skipped += u32::from(!should_render);
    }

    /// Logs and resets this and the blocking-call timings: `calls` from the
    /// session and `fence` from the renderer.
    fn log(&mut self, calls: &mut XrStats, fence: &mut Series) {
        if !self.startup_logged && !self.early.is_empty() {
            let line = self.take_startup_line();
            log::info!("{line}");
        }
        let iteration = self.iteration.take();
        let frames = iteration.map_or(0, |s| s.count);
        let iter = timing::show(iteration);
        let period = timing::show(self.period.take());
        let step = timing::show(self.step.take());
        let skipped = std::mem::take(&mut self.skipped);
        log::info!("pacing ms (median/max): loop {iter}, period {period}, step {step}");
        log::info!("pacing: should_render false on {skipped} of {frames} frames");
        let wait = timing::show(calls.wait_frame.take());
        let begin = timing::show(calls.begin_frame.take());
        let eyes = timing::show(calls.eye_acquire.take());
        log::info!("call ms (median/max): WaitFrame {wait}, BeginFrame {begin}, eyes {eyes}");
        let quads = timing::show(calls.quad_acquire.take());
        let end = timing::show(calls.end_frame.take());
        let fence = timing::show(fence.take());
        log::info!("call ms (median/max): quads {quads}, EndFrame {end}, slot fence {fence}");
    }
}

/// A frame loop that only clears the eyes, for `secs` seconds and then until
/// the session has ended: no app, panels, quad layers or input. Its pacing
/// log shows whether the runtime grants the display's full rate to an app
/// that does nothing, which tells the app's own work from the rest.
fn bare_loop(session: &mut XrSession, renderer: &mut Renderer, secs: f32) -> Result<(), Error> {
    let start = Instant::now();
    let mut pacing = Pacing::default();
    let mut logged = Instant::now();
    // When the first frame ran, and when the session was asked to end.
    let mut began = None;
    let mut asked_to_exit: Option<Instant> = None;
    loop {
        match session.poll()? {
            SessionEvent::Exit => break,
            SessionEvent::None => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            SessionEvent::Running => {}
        }
        let elapsed = start.elapsed().as_secs_f32();
        let ran = began.get_or_insert_with(Instant::now).elapsed();
        let iteration = Instant::now();
        let frame = session.begin_frame()?;
        let period = frame.state.predicted_display_period.as_nanos();
        pacing.frame(
            period,
            frame.state.predicted_display_time.as_nanos(),
            frame.state.should_render,
        );
        if let Some(line) = pacing.startup(elapsed, period, session.focused()) {
            log::info!("{line}");
        }
        if let (Some(targets), Some(eyes)) = (frame.targets, frame.eyes) {
            renderer.begin_frame()?;
            renderer.set_video(None)?;
            renderer.draw(&targets, &eyes, &VideoParams::default(), &[])?;
            renderer.end_frame()?;
        }
        session.end_frame(frame, &[])?;
        pacing.iteration.push_since(iteration);
        if logged.elapsed() > Duration::from_secs(10) {
            pacing.log(&mut session.stats, &mut renderer.fence_wait);
            logged = Instant::now();
        }
        if ran.as_secs_f32() > secs && asked_to_exit.is_none() {
            asked_to_exit = Some(Instant::now());
            session.request_exit();
        }
        if asked_to_exit.is_some_and(|t| t.elapsed() > Duration::from_secs(5)) {
            log::warn!("the runtime didn't end the session; leaving");
            break;
        }
    }
    pacing.log(&mut session.stats, &mut renderer.fence_wait);
    Ok(())
}

fn run_xr(args: &Args) -> Result<(), Error> {
    // After a restart SteamVR may still be closing the previous session:
    // keep trying for a while instead of quitting.
    let attempts = if std::env::var_os(RESTARTED).is_some() {
        20
    } else {
        1
    };
    // Found now: installing an update moves the running copy's folder aside.
    let launcher = fp_updater::current_install_dir()
        .map(|d| d.join("frameplayerx.sh"))
        .ok()
        .filter(|l| l.is_file());
    let switches = Switches::from_env();
    // Read once, here: the render scale is needed before the session exists.
    let settings = Settings::load(&Settings::path());
    let render_scale = settings.render_scale;
    let scale = switches.render_scale.unwrap_or(render_scale.factor());
    let opts = XrOptions {
        minimal_extensions: switches.xr_minimal,
    };
    let mut tries = 0;
    let ctx = loop {
        tries += 1;
        match XrContext::new("FramePlayerX", opts) {
            Ok(c) => break Arc::new(c),
            Err(e) if tries < attempts => {
                log::info!("OpenXR not ready yet ({e}); retrying");
                std::thread::sleep(Duration::from_secs(1));
            }
            Err(e) => {
                return Err(format!("{e}\nIs SteamVR running? FramePlayerX needs an OpenXR runtime (the Steam Frame provides one).").into());
            }
        }
    };
    log::info!("OpenXR runtime {} on {}", ctx.runtime, ctx.system_name);
    let gpu = Arc::new(Gpu::new(&ctx.creator(), "FramePlayerX")?);
    log::info!("GPU {} ({})", gpu.device_name, gpu.driver);
    let mut session = XrSession::new(ctx.clone(), gpu.clone(), scale)?;
    log::info!(
        "eye buffers {:?} {:?}; controller bindings {:?}",
        session.extent,
        session.format,
        session.bindings()
    );
    log::info!("A/B switches: {}", switches.describe());
    match (session.refresh_rate(), session.refresh_rates()) {
        (Some(now), Some(all)) => log::info!("display refresh rate {now} Hz; supported {all:?}"),
        _ => log::info!("display refresh rates: not available"),
    }
    let mut renderer = Renderer::new(gpu.clone(), session.format)?;
    renderer.set_quality(switches.quality);
    if let Some(secs) = switches.bare_loop {
        log::info!("bare frame loop for {secs} s: no app, panels or layers");
        bare_loop(&mut session, &mut renderer, secs)?;
        exit_watchdog(EXIT_WITHIN);
        renderer.wait_idle();
        drop(renderer);
        drop(session);
        drop(gpu);
        drop(ctx);
        log::info!("bye");
        return Ok(());
    }
    fp_media::set_hevc_10bit_hardware(renderer.shows_q10c());
    fp_media::set_shared_frames(renderer.reads_decoder_buffers());
    let mut app = make_app_with(settings)?;
    if let Ok(sizes) = ctx.eye_sizes() {
        app.set_render_info(ui::RenderInfo {
            started: render_scale,
            eye_px: RenderScale::ALL.map(|s| scaled_eye_size(&sizes, s.factor())),
        });
    }
    app.set_about(vec![
        ("OpenXR runtime".into(), ctx.runtime.clone()),
        ("Headset".into(), ctx.system_name.clone()),
        (
            "GPU".into(),
            format!("{} ({})", gpu.device_name, gpu.driver),
        ),
        (
            "Passthrough for apps".into(),
            if session.supports_passthrough_blend() {
                "yes (WebXR immersive-ar possible)"
            } else {
                "no (VR only)"
            }
            .into(),
        ),
        (
            "Eye resolution".into(),
            format!("{}×{}", session.extent.width, session.extent.height),
        ),
        (
            "Controllers".into(),
            session
                .bindings()
                .iter()
                .map(|b| b.name)
                .collect::<Vec<_>>()
                .join(", "),
        ),
    ]);
    if let Some(o) = &args.open {
        app.open(playback::OpenRequest {
            location: location(o),
            ..Default::default()
        });
    }
    let start = Instant::now();
    let mut last = Instant::now();
    let (mut quitting, mut restarting) = (false, false);
    let (mut frames, mut video_frames) = (0u64, 0u64);
    // Render-thread time per frame (after xrWaitFrame), logged every 10 s:
    // over ~9 ms the compositor starts synthesising frames.
    let mut frame_ms: Vec<f32> = Vec::new();
    let mut gpu_ms = perf::GpuWindow::default();
    let mut last_frame_ms = None;
    let mut timing_since = Instant::now();
    let layers = session.supports_quad_layers() && !switches.ui_in_eyes;
    log::info!(
        "UI as compositor quad layers: {}",
        if layers {
            "yes"
        } else if switches.ui_in_eyes {
            "no (forced into the eyes)"
        } else {
            "no (drawn into the eyes)"
        }
    );
    let mut request_hz = switches.request_hz;
    let mut pacing = Pacing::default();
    // Panel picture each quad layer holds: (panel, paint version).
    let mut copied: std::collections::HashMap<u64, (fp_render::PanelId, u64)> =
        std::collections::HashMap::new();
    let mut headset_on = false;
    // The dashboard button: at most one request in two seconds, and whether
    // the layers trimmed to the runtime's limit have been logged.
    let mut dashboard_rate = DashboardRate::default();
    let has_dashboard = ctx.has_dashboard();
    let mut trim_logged = false;
    loop {
        let event = session.poll()?;
        // Acted on here, not in a frame: no frame may be rendered once the
        // headset is off.
        if session.headset_on() != headset_on {
            app.headset_worn_changed(headset_on, session.headset_on());
            headset_on = session.headset_on();
        }
        match event {
            SessionEvent::Exit => break,
            SessionEvent::None => {
                std::thread::sleep(Duration::from_millis(20));
                continue;
            }
            SessionEvent::Running => {}
        }
        if let Some(hz) = request_hz.take() {
            match session.request_refresh_rate(hz) {
                Ok(()) => {
                    let now = session.refresh_rate();
                    log::info!("requested {hz} Hz: accepted (read back {now:?})");
                }
                Err(e) => log::warn!("requested display refresh rate {hz} Hz: {e}"),
            }
        }
        if !switches.no_dashboard_close {
            session.close_dashboard_at_launch();
        }
        let iteration = Instant::now();
        let frame = session.begin_frame()?;
        pacing.frame(
            frame.state.predicted_display_period.as_nanos(),
            frame.state.predicted_display_time.as_nanos(),
            frame.state.should_render,
        );
        if let Some(line) = pacing.startup(
            start.elapsed().as_secs_f32(),
            frame.state.predicted_display_period.as_nanos(),
            session.focused(),
        ) {
            log::info!("{line}");
        }
        let recentred = session.recentred_by(frame.state.predicted_display_time);
        let work = Instant::now();
        let input = session.input(frame.state.predicted_display_time);
        let dt = last.elapsed().as_secs_f32().min(0.1);
        last = Instant::now();
        let eye_px_per_tan = frame
            .eyes
            .as_ref()
            .map(|e| fp_render::math::pixels_per_tan(&e[0].proj, session.extent.width));
        renderer.begin_frame()?;
        gpu_ms.push(renderer.gpu_times());
        let out = app.frame(
            &mut renderer,
            FrameInput {
                time: start.elapsed().as_secs_f64(),
                dt,
                head: frame.head,
                hands: input.hands,
                passthrough_available: session.supports_passthrough_blend(),
                layers,
                frame_ms: last_frame_ms,
                display_hz: Some(frame.state.predicted_display_period.as_nanos())
                    .filter(|&ns| ns > 0)
                    .map(|ns| 1e9 / ns as f32),
                recentred,
                eye_px_per_tan,
                dashboard: has_dashboard,
            },
        );
        // Started without waiting, off this thread: the video keeps playing
        // under SteamVR's dashboard and the session stays up.
        if out.open_dashboard && dashboard_rate.ok(Instant::now()) {
            ctx.open_steamvr_dashboard();
        }
        frames += 1;
        video_frames += out.frame.is_some() as u64;
        if let Err(e) = renderer.set_video(out.frame.as_ref()) {
            log::warn!("set_video: {e}");
            let _ = renderer.set_video(None);
        }
        let mut submits = Vec::new();
        if let (Some(targets), Some(eyes)) = (frame.targets, frame.eyes) {
            renderer.draw(&targets, &eyes, &out.video, &out.quads)?;
            for l in &out.layers {
                match *l {
                    LayerDraw::Panel {
                        key,
                        panel,
                        px,
                        pose,
                        size,
                    } => {
                        let version = renderer.panel_version(panel);
                        if version == 0 {
                            continue;
                        }
                        // Only a changed picture is copied; the compositor
                        // keeps showing the last one.
                        if copied.get(&key) != Some(&(panel, version))
                            || !session.quad_layer_ready(key)
                        {
                            let image = session.quad_layer_image(key, px[0], px[1])?;
                            renderer.copy_panel_to(panel, image)?;
                            copied.insert(key, (panel, version));
                        }
                        let (_, rot, pos) = pose.to_scale_rotation_translation();
                        submits.push(QuadSubmit {
                            key,
                            pose: (pos, rot),
                            size,
                        });
                    }
                    LayerDraw::Ray { active, pose, size } => {
                        let key = RAY_LAYER + active as u64;
                        session.static_quad_layer(key, RAY_PX[0], RAY_PX[1], |image| {
                            renderer
                                .upload_rgba(image, RAY_PX[0], RAY_PX[1], &ray_picture(active))
                                .map_err(|e| e.to_string())
                        })?;
                        submits.push(QuadSubmit { key, pose, size });
                    }
                }
            }
        }
        // The projection layer takes one of the runtime's layers. If the quads
        // are more than fit, the nearest (the last) are kept.
        let keep = fit_layers(session.max_layers(), submits.len());
        if submits.len() > keep {
            if !trim_logged {
                trim_logged = true;
                log::warn!(
                    "{} quad layers over the runtime's {} per frame: showing the nearest {keep}",
                    submits.len(),
                    session.max_layers()
                );
            }
            submits.drain(..submits.len() - keep);
        }
        renderer.end_frame()?;
        session.set_passthrough(out.passthrough);
        for (hand, amp, ms) in out.buzz {
            session.buzz(hand, amp, ms);
        }
        let ms = work.elapsed().as_secs_f32() * 1000.0;
        frame_ms.push(ms);
        last_frame_ms = Some(ms);
        session.end_frame(frame, &submits)?;
        pacing.iteration.push_since(iteration);
        if timing_since.elapsed() > Duration::from_secs(10) && !frame_ms.is_empty() {
            frame_ms.sort_by(f32::total_cmp);
            let at = |q: f32| frame_ms[((frame_ms.len() - 1) as f32 * q) as usize];
            let slow = frame_ms.iter().filter(|&&t| t > 9.0).count();
            log::info!(
                "frame CPU ms: median {:.1}, 95% {:.1}, max {:.1}; {slow} of {} over 9 ms",
                at(0.5),
                at(0.95),
                at(1.0),
                frame_ms.len()
            );
            if let Some(line) = gpu_ms.take_line() {
                log::info!("{line}");
            }
            if let Some(pb) = &app.playback {
                let st = pb.player.stats();
                let hdr = renderer
                    .hdr_scene()
                    .map(|(t, peak)| format!(", {t:?} scene peak {peak:.0} nits"))
                    .unwrap_or_default();
                log::info!(
                    "video {} ({}): {} shown, {} dropped, {} stalls, {} queued{hdr}",
                    st.video_decoder,
                    if st.hardware { "hardware" } else { "software" },
                    st.frames_shown,
                    st.frames_dropped,
                    st.stalls,
                    st.video_queue
                );
            }
            pacing.log(&mut session.stats, &mut renderer.fence_wait);
            frame_ms.clear();
            timing_since = Instant::now();
        }
        let timed_out = args
            .exit_after
            .is_some_and(|t| start.elapsed().as_secs_f64() > t);
        if (out.quit || timed_out) && !quitting {
            quitting = true;
            restarting = out.restart;
            session.request_exit();
        }
    }
    log::info!("{frames} frames, {video_frames} with video");
    exit_watchdog(EXIT_WITHIN);
    let restart = launcher.filter(|_| restarting);
    if quitting && restart.is_none() {
        // SteamVR leaves its dashboard closed after an app quits itself.
        ctx.show_steamvr_dashboard();
    }
    renderer.wait_idle();
    app.shutdown();
    drop(renderer);
    drop(session);
    drop(gpu);
    drop(ctx);
    if let Some(l) = restart {
        log::info!("restarting");
        // The same process goes on, so Steam keeps it as the running game.
        let e = std::process::Command::new(&l).env(RESTARTED, "1").exec();
        log::warn!("{}: {e}", l.display());
    }
    log::info!("bye");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in for the process environment holding `vars`.
    fn env<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            let (_, value) = vars.iter().find(|(k, _)| *k == name)?;
            Some(value.to_string())
        }
    }

    /// The switches with only `name` set, to `value`.
    fn with(name: &'static str, value: &str) -> Switches {
        Switches::parse(|n| (n == name).then(|| value.to_string()))
    }

    #[test]
    fn switches_default_to_off() {
        let s = Switches::parse(env(&[]));
        assert_eq!(s, Switches::default());
        assert_eq!(s.describe(), "none");
        assert_eq!(s.quality, Quality::default());
    }

    #[test]
    fn switches_parse_the_environment_values() {
        let vars = [
            ("FRAMEPLAYERX_UI_IN_EYES", "1"),
            ("FRAMEPLAYERX_REQUEST_HZ", " 90 "),
            ("FRAMEPLAYERX_RENDER_SCALE", "1.5"),
            ("FRAMEPLAYERX_BARE_LOOP", "40"),
            ("FRAMEPLAYERX_XR_MINIMAL", "1"),
            ("FRAMEPLAYERX_NO_DASHBOARD_CLOSE", "yes"),
            ("FRAMEPLAYERX_QUALITY", "nodither, noedgeaa"),
        ];
        let s = Switches::parse(env(&vars));
        assert!(s.ui_in_eyes);
        assert_eq!(s.request_hz, Some(90.0));
        assert_eq!(s.render_scale, Some(1.5));
        assert_eq!(s.bare_loop, Some(40.0));
        assert!(s.xr_minimal);
        assert!(s.no_dashboard_close);
        assert!(!s.quality.dither && !s.quality.edge_aa && s.quality.supersample);
        let want = [
            "FRAMEPLAYERX_UI_IN_EYES",
            "FRAMEPLAYERX_REQUEST_HZ=90",
            "FRAMEPLAYERX_RENDER_SCALE=1.5",
            "FRAMEPLAYERX_BARE_LOOP=40",
            "FRAMEPLAYERX_XR_MINIMAL",
            "FRAMEPLAYERX_NO_DASHBOARD_CLOSE",
            "FRAMEPLAYERX_QUALITY=nodither,noedgeaa",
        ];
        assert_eq!(s.describe(), want.join(", "));
    }

    #[test]
    fn switches_ignore_off_and_bad_values() {
        for off in ["", "0", " 0 "] {
            assert!(!with("FRAMEPLAYERX_UI_IN_EYES", off).ui_in_eyes);
            assert!(!with("FRAMEPLAYERX_XR_MINIMAL", off).xr_minimal);
            assert!(!with("FRAMEPLAYERX_NO_DASHBOARD_CLOSE", off).no_dashboard_close);
        }
        for bad in ["", "fast", "-72", "NaN", "inf"] {
            assert_eq!(with("FRAMEPLAYERX_REQUEST_HZ", bad).request_hz, None);
        }
        for bad in ["", "fast", "dither", "nodithers"] {
            assert_eq!(with("FRAMEPLAYERX_QUALITY", bad).quality, Quality::default());
        }
        assert_eq!(with("FRAMEPLAYERX_REQUEST_HZ", "0").request_hz, Some(0.0));
        for bad in ["", "fast", "0", "-1.5", "NaN", "inf"] {
            assert_eq!(with("FRAMEPLAYERX_RENDER_SCALE", bad).render_scale, None);
            assert_eq!(with("FRAMEPLAYERX_BARE_LOOP", bad).bare_loop, None);
        }
    }

    #[test]
    fn runs_group_values_that_stay_close() {
        assert!(runs(&[], 0.5).is_empty());
        let one: [(f32, u32); 1] = [(13.9, 3)];
        assert_eq!(runs(&[13.9, 13.9, 13.8], 0.5), one);
        let alternating: [(f32, u32); 3] = [(13.9, 1), (27.8, 1), (13.9, 1)];
        assert_eq!(runs(&[13.9, 27.8, 13.9], 0.5), alternating);
        // Each value is judged against the first of its run, not the last.
        let split: [(f32, u32); 2] = [(10.0, 2), (10.9, 1)];
        assert_eq!(runs(&[10.0, 10.4, 10.9], 0.5), split);
    }

    #[test]
    fn the_first_throttled_frame_is_the_first_slow_one() {
        assert_eq!(first_throttled(&[]), None);
        assert_eq!(first_throttled(&[13.9, 13.9, 13.9]), None);
        assert_eq!(first_throttled(&[13.9, 13.9, 27.8, 13.9]), Some(2));
        // Slow from the first frame: nothing to compare against.
        assert_eq!(first_throttled(&[27.8, 27.8]), None);
    }

    #[test]
    fn the_startup_line_names_the_runs_and_the_first_throttled_frame() {
        let frames: Vec<(f32, f32)> = (0..600)
            .map(|i| (i as f32 * 0.01, if i < 42 { 13.9 } else { 27.8 }))
            .collect();
        assert_eq!(
            startup_summary(&frames, Some(0.2)),
            "pacing startup: 600 frames, period runs [13.9x42, 27.8x558]; \
             first throttled frame 42 at 0.2 s after FOCUSED"
        );
        let before = startup_summary(&frames, None);
        assert!(before.ends_with("first throttled frame 42 at 0.4 s, before FOCUSED"));
        let steady = startup_summary(&frames[..42], Some(0.0));
        assert!(
            steady.ends_with("[13.9x42]; no throttled frame"),
            "{steady}"
        );
    }

    #[test]
    fn the_startup_line_comes_once_the_first_frames_are_in() {
        let mut p = Pacing::default();
        for i in 0..STARTUP_FRAMES - 1 {
            assert_eq!(p.startup(i as f32 * 0.014, 13_900_000, i > 3), None);
        }
        let line = p.startup(10.0, 13_900_000, true).expect("the last frame");
        assert!(line.contains("600 frames"), "{line}");
        assert_eq!(p.startup(10.1, 13_900_000, true), None, "only once");
    }

    #[test]
    fn pacing_records_period_step_and_skipped_frames() {
        let mut p = Pacing::default();
        p.frame(27_800_000, 1_000_000_000, true);
        p.frame(27_800_000, 1_027_800_000, false);
        p.frame(27_800_000, 1_055_600_000, true);
        let period = p.period.take().unwrap();
        assert!((period.median - 27.8).abs() < 0.01);
        // Two steps between three frames.
        let step = p.step.take().unwrap();
        assert_eq!(step.count, 2);
        assert!((step.max - 27.8).abs() < 0.01);
        assert_eq!(p.skipped, 1);
    }
}
