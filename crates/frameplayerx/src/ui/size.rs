//! How big the screen looks, as people choose it: Home theater, Cinema or
//! Giant. One choice everywhere. Round a flat screen it is the screen's
//! width; inside a room, which fixes its own screen, it is the seat, since
//! the Back, Middle and Front seats see the picture at those same sizes.

use super::player::pick_seat;
use crate::app::places;
use crate::settings::Settings;
use fp_core::view::{SCREEN_WIDTH, ScreenSize, ViewSettings, screen_width_for};
use fp_render::env::{Place, PlaceKind, SeatName};

/// The size's name, as shown on its button.
pub fn name(size: ScreenSize) -> &'static str {
    match size {
        ScreenSize::HomeTheater => tr!("screen-size-home"),
        ScreenSize::Cinema => tr!("screen-size-cinema"),
        ScreenSize::Giant => tr!("screen-size-giant"),
    }
}

/// The degrees shown small beside a size's name.
pub fn degrees_label(size: ScreenSize) -> String {
    format!("{:.0}°", size.degrees())
}

/// The seat of a room that sees the picture at `size`.
pub fn seat_for(size: ScreenSize) -> SeatName {
    match size {
        ScreenSize::HomeTheater => SeatName::Back,
        ScreenSize::Cinema => SeatName::Middle,
        ScreenSize::Giant => SeatName::Front,
    }
}

/// The size a seat is, if it is one of the three.
pub fn size_of_seat(seat: SeatName) -> Option<ScreenSize> {
    ScreenSize::ALL.into_iter().find(|&s| seat_for(s) == seat)
}

/// The place whose room fixes the screen: the one shown, if it is a room.
pub fn room(place: Option<Place>) -> Option<Place> {
    place.filter(|p| p.kind() == PlaceKind::Room)
}

/// A screen of `degrees` at `distance` metres, as a width the sliders allow.
pub fn fit(degrees: f32, distance: f32) -> f32 {
    screen_width_for(degrees, distance).clamp(*SCREEN_WIDTH.start(), *SCREEN_WIDTH.end())
}

/// The size in use: the seat in a room, else the width. `None` when it is
/// in between (a width set with the slider, the couch).
pub fn current(
    settings: &Settings,
    view: &ViewSettings,
    place: Option<Place>,
) -> Option<ScreenSize> {
    match room(place) {
        Some(place) => {
            let layout = place.layout();
            let seat = places::seat_index(settings, place, &layout);
            size_of_seat(layout.seats.get(seat)?.name)
        }
        None => ScreenSize::of(view.screen_width, view.screen_distance),
    }
}

/// Makes the picture `size`. Returns whether anything changed.
pub fn apply(
    size: ScreenSize,
    settings: &mut Settings,
    view: &mut ViewSettings,
    place: Option<Place>,
) -> bool {
    match room(place) {
        Some(place) => {
            let layout = place.layout();
            let Some(seat) = layout.seat_index(seat_for(size)) else {
                return false;
            };
            let before = places::seat_index(settings, place, &layout);
            pick_seat(settings, place, seat);
            places::seat_index(settings, place, &layout) != before
        }
        None => {
            let width = fit(size.degrees(), view.screen_distance);
            let changed = view.screen_width != width;
            view.screen_width = width;
            changed
        }
    }
}

/// Steps to the next size (a size in between goes to Cinema) and returns
/// the one now in use.
pub fn cycle(settings: &mut Settings, view: &mut ViewSettings, place: Option<Place>) -> ScreenSize {
    let next = current(settings, view, place).map_or(ScreenSize::Cinema, ScreenSize::next);
    apply(next, settings, view, place);
    next
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> (Settings, ViewSettings) {
        (Settings::default(), ViewSettings::default())
    }

    #[test]
    fn the_names_are_plain_and_different() {
        let names: Vec<_> = ScreenSize::ALL.into_iter().map(name).collect();
        assert_eq!(names, ["Home theater", "Cinema", "Giant"]);
        let degrees: Vec<_> = ScreenSize::ALL.into_iter().map(degrees_label).collect();
        assert_eq!(degrees, ["58°", "72°", "90°"]);
    }

    #[test]
    fn every_size_has_its_own_seat_and_back_again() {
        for size in ScreenSize::ALL {
            assert_eq!(size_of_seat(seat_for(size)), Some(size));
        }
        assert_eq!(seat_for(ScreenSize::Giant), SeatName::Front);
        assert_eq!(seat_for(ScreenSize::Cinema), SeatName::Middle);
        assert_eq!(seat_for(ScreenSize::HomeTheater), SeatName::Back);
        assert_eq!(size_of_seat(SeatName::Couch), None);
    }

    #[test]
    fn a_flat_screen_starts_as_cinema_and_cycles_round() {
        use ScreenSize::*;
        let (mut s, mut v) = defaults();
        assert_eq!(current(&s, &v, None), Some(Cinema));
        let seen: Vec<_> = (0..4).map(|_| cycle(&mut s, &mut v, None)).collect();
        assert_eq!(seen, [Giant, HomeTheater, Cinema, Giant]);
        // Each step lands on a real preset, kept at the same distance.
        assert_eq!(current(&s, &v, None), Some(Giant));
        assert_eq!(v.screen_distance, ViewSettings::default().screen_distance);
        // A sky is no room: the width still decides.
        assert_eq!(current(&s, &v, Some(Place::Studio)), Some(Giant));
    }

    #[test]
    fn an_in_between_width_goes_to_cinema() {
        let (mut s, mut v) = defaults();
        v.screen_width = 6.5;
        assert_eq!(current(&s, &v, None), None);
        assert_eq!(cycle(&mut s, &mut v, None), ScreenSize::Cinema);
        assert_eq!(current(&s, &v, None), Some(ScreenSize::Cinema));
    }

    #[test]
    fn in_the_cinema_the_size_is_the_seat_and_the_screen_stays() {
        let place = Some(Place::Cinema);
        let (mut s, mut v) = defaults();
        let width = v.screen_width;
        let layout = Place::Cinema.layout();
        let seat = |s: &Settings| places::seat_index(s, Place::Cinema, &layout);
        // The room starts at its middle seat.
        assert_eq!(current(&s, &v, place), Some(ScreenSize::Cinema));
        assert_eq!(cycle(&mut s, &mut v, place), ScreenSize::Giant);
        assert_eq!(seat(&s), layout.seat_index(SeatName::Front).unwrap());
        assert_eq!(cycle(&mut s, &mut v, place), ScreenSize::HomeTheater);
        assert_eq!(seat(&s), layout.seat_index(SeatName::Back).unwrap());
        assert_eq!(cycle(&mut s, &mut v, place), ScreenSize::Cinema);
        assert_eq!(seat(&s), layout.seat_index(SeatName::Middle).unwrap());
        // The video's own width was never touched.
        assert_eq!(v.screen_width, width);
    }

    #[test]
    fn applying_the_size_in_use_changes_nothing() {
        let (mut s, mut v) = defaults();
        assert!(!apply(ScreenSize::Cinema, &mut s, &mut v, None));
        assert!(apply(ScreenSize::Giant, &mut s, &mut v, None));
        assert!(!apply(ScreenSize::Giant, &mut s, &mut v, None));
        let room = Some(Place::Cinema);
        assert!(!apply(ScreenSize::Cinema, &mut s, &mut v, room));
        assert!(apply(ScreenSize::Giant, &mut s, &mut v, room));
    }

    #[test]
    fn the_cinema_seats_see_the_sizes_the_names_say() {
        // The point of the mapping: each seat's view of the 16:9 picture
        // filling the room's screen height is about the size's angle.
        let layout = Place::Cinema.layout();
        let screen = layout.screen.expect("the cinema has a screen");
        let width = screen.max.y * 16.0 / 9.0;
        for size in ScreenSize::ALL {
            let seat = layout.seat(layout.seat_index(seat_for(size)).unwrap());
            let d = (screen.centre - seat.eye).length();
            let seen = fp_core::view::screen_degrees(width, d);
            assert!(
                (seen - size.degrees()).abs() < 6.0,
                "{size:?}: {seen} degrees"
            );
        }
    }
}
