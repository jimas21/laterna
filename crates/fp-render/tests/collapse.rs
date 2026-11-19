//! The picture's light on the real Cinema, read from the one collapsed
//! texture and from the eight basis textures it replaces: how far the two
//! pictures are apart (PSNR over both eyes) for every seat, a handful of
//! pictures, and each size of the collapsed texture, on a real (software)
//! Vulkan device, plus how long a frame takes each way.
//!
//! Runs against the real Cinema when `FRAMEPLAYERX_PLACES_DIR` points at a
//! folder with `cinema-v2/` in it (the per-seat pack), and skips otherwise
//! (the pack is not in the repository):
//!
//! ```text
//! FRAMEPLAYERX_PLACES_DIR=/path/to/packs-seats \
//!   cargo test -p fp-render --test collapse -- --nocapture
//! ```

use fp_core::view::ViewSettings;
use fp_media::VideoFrame;
use fp_render::Renderer;
use fp_render::capture::OffscreenEyes;
use fp_render::env::{EnvFrame, Place, SeatName};
use std::sync::Arc;
use std::time::Instant;

mod common;
use common::{cinema_params, eyes_at, frame, place_frame, setup_sized};

/// Pixels on a side of the eye images: `FRAMEPLAYERX_COLLAPSE_TEST_SIZE`, or
/// small enough for a software rasteriser.
const DEFAULT_SIZE: u32 = 384;

/// The least agreement between the two ways to read the light that is
/// called visually identical.
const PSNR_FLOOR: f64 = 45.0;

fn size() -> u32 {
    std::env::var("FRAMEPLAYERX_COLLAPSE_TEST_SIZE")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_SIZE)
}

fn pack_present() -> bool {
    std::env::var_os(fp_render::env::pack::PLACES_DIR_ENV)
        .filter(|v| !v.is_empty())
        .is_some_and(|root| {
            std::path::Path::new(&root)
                .join("cinema-v2")
                .join("manifest.json")
                .is_file()
        })
}

/// One frame of `picture` from `seat`, both eyes, at `time` seconds.
fn shoot(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    seat: SeatName,
    picture: &Arc<VideoFrame>,
    house: f32,
    time: f64,
) -> [Vec<u8>; 2] {
    let env = EnvFrame {
        screen_light: 1.0,
        time,
        ..place_frame(Place::Cinema, seat, house)
    };
    let p = cinema_params(ViewSettings::default(), env);
    r.begin_frame().unwrap();
    r.set_video(Some(picture)).unwrap();
    r.draw(&e.targets(), &eyes_at(common::EYE_HEIGHT), &p, &[])
        .unwrap();
    r.end_frame().unwrap();
    [e.read(r, 0).unwrap(), e.read(r, 1).unwrap()]
}

/// PSNR in dB of two RGBA8 images over their colour channels, the peak
/// 255; infinite when they are the same.
fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let (mut sum, mut n) = (0.0f64, 0u64);
    for (x, y) in a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0) {
        for k in 0..3 {
            let d = f64::from(x[k]) - f64::from(y[k]);
            sum += d * d;
            n += 1;
        }
    }
    let mse = sum / n.max(1) as f64;
    if mse == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (255.0f64 * 255.0 / mse).log10()
    }
}

fn pictures() -> Vec<(&'static str, Arc<VideoFrame>)> {
    vec![
        (
            "gradient",
            frame(160, 90, |u, v| {
                [
                    (u * 255.0) as u8,
                    (v * 255.0) as u8,
                    ((1.0 - u) * 200.0) as u8,
                ]
            }),
        ),
        ("white", frame(160, 90, |_, _| [255, 255, 255])),
        ("dark", frame(160, 90, |_, _| [12, 10, 16])),
        (
            "teal and orange",
            frame(160, 90, |u, _| {
                if u < 0.5 {
                    [20, 190, 170]
                } else {
                    [240, 120, 20]
                }
            }),
        ),
    ]
}

/// Both ways of reading one picture's light from `seat`, once the eased
/// colours have settled: the image collapsed and the image from eight reads,
/// from the same state (the same time, so nothing eases between them).
fn both_ways(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    seat: SeatName,
    picture: &Arc<VideoFrame>,
    house: f32,
    time: &mut f64,
) -> ([Vec<u8>; 2], [Vec<u8>; 2]) {
    r.set_place_collapse(true);
    let mut collapsed = None;
    for _ in 0..6 {
        *time += 1.0;
        collapsed = Some(shoot(r, e, seat, picture, house, *time));
    }
    r.set_place_collapse(false);
    let eight = shoot(r, e, seat, picture, house, *time);
    (collapsed.expect("six frames"), eight)
}

#[test]
fn the_collapsed_light_is_visually_the_eight_reads_on_the_real_cinema() {
    if !pack_present() {
        eprintln!("skipping: FRAMEPLAYERX_PLACES_DIR does not point at a folder with cinema-v2");
        return;
    }
    let Some((mut r, mut e)) = setup_sized(size()) else {
        return;
    };
    // Pack order, so that no frame waits on the sorter.
    r.set_place_knobs(true, false);
    let seats = [SeatName::Front, SeatName::Middle, SeatName::Back];
    let mut time = 0.0;
    let side = size();
    eprintln!(
        "PSNR (dB, both eyes) of the collapsed light against the eight reads, {side}x{side}:"
    );
    eprintln!("shift  seat    picture          house   psnr");
    // The size the app starts with first, then the others. Every pass goes
    // front, middle, back, so each starts on a seat that is not the one the
    // last pass ended on and loads every set afresh (a seat's set is loaded
    // when the viewer sits there).
    let shifts: [Option<u32>; 3] = [None, Some(1), Some(2)];
    let mut floor_default = f64::INFINITY;
    for shift in shifts {
        if let Some(n) = shift {
            r.set_place_collapse_shift(n);
        }
        let order = seats;
        let mut worst = f64::INFINITY;
        for seat in order {
            for (name, picture) in pictures() {
                for house in [0.0, 0.5] {
                    let (a, b) = both_ways(&mut r, &mut e, seat, &picture, house, &mut time);
                    let db = psnr(&a[0], &b[0]).min(psnr(&a[1], &b[1]));
                    worst = worst.min(db);
                    eprintln!(
                        "{:<6} {:<7} {:<16} {house:<6} {db:>7.2}",
                        shift.map_or("auto".to_string(), |n| n.to_string()),
                        format!("{seat:?}"),
                        name
                    );
                }
            }
        }
        eprintln!(
            "shift {}: worst {worst:.2} dB",
            shift.map_or("auto".to_string(), |n| n.to_string())
        );
        if shift.is_none() {
            floor_default = worst;
        }
    }
    assert!(
        floor_default > PSNR_FLOOR,
        "the default collapsed light is {floor_default:.2} dB from the eight reads"
    );
    e.destroy(&r);
}

#[test]
fn a_frame_with_the_collapsed_light_costs_no_more_than_eight_reads_on_the_real_cinema() {
    if !pack_present() {
        eprintln!("skipping: FRAMEPLAYERX_PLACES_DIR does not point at a folder with cinema-v2");
        return;
    }
    let Some((mut r, mut e)) = setup_sized(size()) else {
        return;
    };
    r.set_place_knobs(true, false);
    let picture = frame(160, 90, |u, v| [(u * 255.0) as u8, (v * 255.0) as u8, 128]);
    // The wall clock of a frame on a CPU rasteriser is only a proxy for the
    // texture reads the draw makes: it counts them, not what a mobile GPU's
    // caches make of them.
    let mut time = 0.0;
    let mut per_frame = |r: &mut Renderer, e: &mut OffscreenEyes, collapse: bool| {
        r.set_place_collapse(collapse);
        // Settled, and the collapsed texture drawn.
        for _ in 0..6 {
            time += 1.0;
            shoot(r, e, SeatName::Middle, &picture, 0.0, time);
        }
        let start = Instant::now();
        let frames = 8;
        for _ in 0..frames {
            shoot(r, e, SeatName::Middle, &picture, 0.0, time);
        }
        start.elapsed().as_secs_f64() * 1000.0 / f64::from(frames)
    };
    let (mut eight, mut one) = (f64::MAX, f64::MAX);
    // Best of three each way, alternating, so that load on the machine
    // counts against neither.
    for _ in 0..3 {
        eight = eight.min(per_frame(&mut r, &mut e, false));
        one = one.min(per_frame(&mut r, &mut e, true));
    }
    let side = size();
    eprintln!(
        "lavapipe, {side}x{side}, a settled picture, a frame with readback: eight reads {eight:.1} ms, collapsed {one:.1} ms"
    );
    assert!(
        one < eight * 1.25,
        "collapsed {one:.1} ms against eight reads {eight:.1} ms"
    );
    e.destroy(&r);
}
