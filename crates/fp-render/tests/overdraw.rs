//! Overdraw of a baked place, measured with a pipeline statistics query
//! (fragment shader invocations) on a real (software) Vulkan device: how many
//! times a pixel's fragment shader runs for the place's draw, from each seat,
//! drawn nearest first and in the pack's own order.
//!
//! Runs against the real Cinema when `FRAMEPLAYERX_PLACES_DIR` points at a
//! folder with `cinema-v2/` in it, and skips otherwise (the pack is not in the
//! repository). The numbers are printed (`cargo test -- --nocapture`):
//!
//! ```text
//! FRAMEPLAYERX_PLACES_DIR=/path/to/packs-v2 \
//!   cargo test -p fp-render --test overdraw -- --nocapture
//! ```
//!
//! What this predicts for the Steam Frame, and what it does not. Lavapipe is
//! a CPU rasteriser that bins triangles into tiles and tests depth per tile
//! and per quad before it shades, which is also roughly what a tiling GPU
//! does (Adreno rejects hidden fragments early with its low resolution Z
//! pass, LRZ, when the shader does not write depth or discard). So the
//! invocation counts say how much shading the draw order saves and how many
//! texture reads the place asks for at best. They do not say what a read
//! costs (cache, bandwidth, ASTC decode, anisotropic taps), whether LRZ
//! engages for this draw, or the time of anything: that is the headset's A/B.

use fp_core::view::ViewSettings;
use fp_media::VideoFrame;
use fp_render::Renderer;
use fp_render::capture::OffscreenEyes;
use fp_render::env::{Place, SeatName};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

mod common;
use common::{cinema_params, frame, place_frame, setup_counting};

/// Pixels on a side of the eye images: big enough for the place's detail to
/// overlap as it does in the headset, small enough for a software rasteriser.
const SIZE: u32 = 512;

/// The clear colour, which no pixel of the room has.
const MAGENTA: [u8; 3] = [255, 0, 255];

fn pack_dir() -> Option<PathBuf> {
    let root = std::env::var_os(fp_render::env::pack::PLACES_DIR_ENV).filter(|v| !v.is_empty())?;
    let dir = PathBuf::from(root).join("cinema-v2");
    dir.join("manifest.json").is_file().then_some(dir)
}

/// One measurement.
#[derive(Clone, Copy, Debug)]
struct Measure {
    /// Fragment shader invocations of the place's draw, left eye.
    invocations: u64,
    /// Pixels of the left eye the place covers.
    covered: u64,
}

impl Measure {
    fn overdraw(&self) -> f64 {
        self.invocations as f64 / self.covered.max(1) as f64
    }
}

/// One frame from `seat`, the left eye's invocations and covered pixels.
fn measure(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    seat: SeatName,
    picture: &Arc<VideoFrame>,
) -> Measure {
    let env = fp_render::env::EnvFrame {
        screen_light: 1.0,
        ..place_frame(Place::Cinema, seat, 0.0)
    };
    let mut p = cinema_params(ViewSettings::default(), env);
    p.background = [1.0, 0.0, 1.0, 1.0];
    r.begin_frame().unwrap();
    r.set_video(Some(picture)).unwrap();
    r.draw(&e.targets(), &common::eyes_at(common::EYE_HEIGHT), &p, &[])
        .unwrap();
    r.end_frame().unwrap();
    let image = e.read(r, 0).unwrap();
    let covered = image
        .chunks_exact(4)
        .filter(|px| px[..3] != MAGENTA)
        .count() as u64;
    let invocations = r.place_fragment_counts()[0].expect("the place was drawn and counted");
    Measure {
        invocations,
        covered,
    }
}

/// Draws from `seat` until the sorter has answered for it (its order is for a
/// new eye), so the next frame is drawn nearest first from there.
fn wait_for_order(
    r: &mut Renderer,
    e: &mut OffscreenEyes,
    seat: SeatName,
    picture: &Arc<VideoFrame>,
) {
    let before = r.place_order_eye();
    let give_up = Instant::now() + Duration::from_secs(60);
    loop {
        measure(r, e, seat, picture);
        let now = r.place_order_eye();
        if now.is_some() && now != before {
            return;
        }
        assert!(Instant::now() < give_up, "the sorter never answered");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_cinemas_overdraw_from_each_seat_sorted_and_in_the_packs_order() {
    let Some(dir) = pack_dir() else {
        eprintln!("skipping: FRAMEPLAYERX_PLACES_DIR does not point at a folder with cinema-v2");
        return;
    };
    let Some((mut r, mut e)) = setup_counting(SIZE) else {
        return;
    };
    let pack = fp_render::env::pack::load(&dir).expect("the Cinema pack loads");
    let reads_lights_down = 1;
    let basis_reads = 8;
    r.set_place_data(Place::Cinema, pack.into_place_data());
    let picture = frame(160, 90, |_, _| [128, 128, 128]);

    let mut table = Vec::new();
    for seat in [SeatName::Front, SeatName::Middle, SeatName::Back] {
        // Pack order first (the sorter has not answered for this seat), then
        // nearest first once it has.
        r.set_place_knobs(true, false);
        let unsorted = measure(&mut r, &mut e, seat, &picture);
        r.set_place_knobs(true, true);
        wait_for_order(&mut r, &mut e, seat, &picture);
        let sorted = measure(&mut r, &mut e, seat, &picture);
        // The picture's light changes what a fragment reads, not how many
        // run: the count must not move.
        r.set_place_knobs(false, true);
        let sorted_no_basis = measure(&mut r, &mut e, seat, &picture);
        r.set_place_knobs(true, true);
        table.push((seat, unsorted, sorted, sorted_no_basis));

        assert!(
            sorted.covered * 100 >= (SIZE as u64 * SIZE as u64) * 50,
            "{seat:?}: the place covers only {} pixels",
            sorted.covered
        );
        assert!(
            sorted.invocations <= unsorted.invocations + unsorted.invocations / 50,
            "{seat:?}: nearest first shades more ({}) than the pack's order ({})",
            sorted.invocations,
            unsorted.invocations
        );
        assert_eq!(
            sorted.invocations, sorted_no_basis.invocations,
            "{seat:?}: the picture's light changed the fragment count"
        );
    }

    eprintln!(
        "overdraw of the Cinema, {SIZE}x{SIZE} left eye, lavapipe (invocations / covered pixels):"
    );
    eprintln!(
        "seat    covered   pack order            sorted               est. texture reads (sorted, lights down)"
    );
    for (seat, unsorted, sorted, no_basis) in &table {
        eprintln!(
            "{:<7} {:>7}   {:>8} = {:.2}x   {:>8} = {:.2}x   basis on {:>9}, off {:>8}",
            format!("{seat:?}"),
            sorted.covered,
            unsorted.invocations,
            unsorted.overdraw(),
            sorted.invocations,
            sorted.overdraw(),
            sorted.invocations * (reads_lights_down + basis_reads),
            no_basis.invocations * reads_lights_down,
        );
    }
    e.destroy(&r);
}
