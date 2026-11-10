//! A Plex library's tile shows the library's own picture, never one of the
//! server's stock glyphs. The section list is a made-up reply in the shape a
//! server sends (`fixtures/plex/sections.json`).

mod common;

use common::{Req, Resp, TestServer, resp};
use fp_sources::plex::PlexSource;
use fp_sources::{Catalog, PlexConfig, Source};
use std::collections::HashMap;

const SECTIONS: &str = include_str!("fixtures/plex/sections.json");

const FILMS_COLLAGE: &str = "/library/sections/1/composite/1700000000";
const SERIES_ART: &str = "/library/sections/2/art/1700000001";
const SERIES_POSTER: &str = "/library/sections/2/thumb/1700000003";
const CARTOONS_POSTER: &str = "/library/sections/4/thumb/1700000004";

fn reply(request: &Req) -> Resp {
    if request.path() != "/identity" && request.header("X-Plex-Token") != Some("server-secret") {
        return resp(401, b"unauthorized".to_vec(), &[]);
    }
    let json = [("Content-Type", "application/json")];
    let image = [("Content-Type", "image/jpeg")];
    match request.path() {
        "/identity" => {
            let body = br#"{"MediaContainer":{"machineIdentifier":"machine-1"}}"#;
            resp(200, body.to_vec(), &json)
        }
        "/" => {
            let body = br#"{"MediaContainer":{"friendlyName":"My Plex"}}"#;
            resp(200, body.to_vec(), &json)
        }
        "/library/sections" => resp(200, SECTIONS.as_bytes().to_vec(), &json),
        "/photo/:/transcode" => resp(200, b"picture".to_vec(), &image),
        _ => resp(404, Vec::new(), &[]),
    }
}

fn source(server: &TestServer) -> PlexSource {
    PlexSource::new(&PlexConfig {
        id: "plex-one".into(),
        name: "My Plex".into(),
        url: server.base.clone(),
        machine_identifier: "machine-1".into(),
        access_token: "server-secret".into(),
        connections: Vec::new(),
        auth: None,
        insecure_tls: false,
        parallel_downloads: false,
        max_height: None,
        local_only: false,
        allow_relay: true,
        ..Default::default()
    })
    .unwrap()
}

/// What a picture location asks the server for: the picture's own path and
/// the box the server scales it into (`640x360`).
struct Asked {
    picture: String,
    size: String,
}

fn asked(source: &PlexSource, location: &str) -> Asked {
    assert!(location.starts_with("plex://"), "{location}");
    let (_, url) = source.picture_request(location).unwrap();
    assert!(!url.contains("secret"), "the token goes in a header");
    // The picture's path is itself a query value, so it arrives encoded.
    let url = url::Url::parse(&url).unwrap();
    assert_eq!(url.path(), "/photo/:/transcode");
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(params["upscale"], "0");
    let (width, height) = (&params["width"], &params["height"]);
    Asked {
        picture: params["url"].clone(),
        size: format!("{width}x{height}"),
    }
}

#[test]
fn library_tiles_use_the_librarys_own_picture_not_a_stock_glyph() {
    let server = TestServer::start(reply);
    let source = source(&server);
    // The poster and the art the server offers are stock glyphs; only the
    // collage is the library's own.
    let library = source.item("section:1").unwrap();
    assert_eq!(library.name, "Films");

    let tile = asked(&source, library.landscape.as_deref().unwrap());
    assert_eq!(tile.picture, FILMS_COLLAGE);
    assert_eq!(tile.size, "640x360", "a 16:9 tile");

    let backdrop = asked(&source, library.backdrop.as_deref().unwrap());
    assert_eq!(backdrop.picture, FILMS_COLLAGE);
    assert_eq!(backdrop.size, "1280x720");

    assert!(library.poster.is_none(), "the poster is a stock glyph");
}

#[test]
fn custom_art_wins_over_the_collage() {
    let server = TestServer::start(reply);
    let source = source(&server);
    let library = source.item("section:2").unwrap();

    let tile = asked(&source, library.landscape.as_deref().unwrap());
    assert_eq!(tile.picture, SERIES_ART);
    let backdrop = asked(&source, library.backdrop.as_deref().unwrap());
    assert_eq!(backdrop.picture, SERIES_ART);

    // The poster keeps its own box and its own picture.
    let poster = asked(&source, library.poster.as_deref().unwrap());
    assert_eq!(poster.picture, SERIES_POSTER);
    assert_eq!(poster.size, "480x480");
}

#[test]
fn a_library_with_only_a_poster_of_its_own_uses_it_for_the_tile() {
    let server = TestServer::start(reply);
    let source = source(&server);
    let library = source.item("section:4").unwrap();

    let tile = asked(&source, library.landscape.as_deref().unwrap());
    assert_eq!(tile.picture, CARTOONS_POSTER);
    assert_eq!(tile.size, "640x360");
    assert!(library.backdrop.is_none(), "its art is a stock glyph");
}

#[test]
fn a_library_with_only_glyphs_shows_its_placeholder() {
    let server = TestServer::start(reply);
    let source = source(&server);
    let library = source.item("section:3").unwrap();
    assert!(library.landscape.is_none());
    assert!(library.poster.is_none());
    assert!(library.backdrop.is_none());
}

#[test]
fn only_video_libraries_are_listed_in_the_servers_order() {
    let server = TestServer::start(reply);
    let source = source(&server);
    let entries = source.list(None).unwrap();
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Films", "Series", "Clips", "Cartoons"]);
}

#[test]
fn a_picture_is_fetched_with_the_token_in_a_header() {
    let server = TestServer::start(reply);
    let source = source(&server);
    let library = source.item("section:1").unwrap();
    let (client, url) = source
        .picture_request(library.landscape.as_deref().unwrap())
        .unwrap();
    assert!(!url.contains("secret"), "the token goes in a header");
    let answer = client.request("GET", &url, &[], None, 1024).unwrap();
    assert_eq!(answer.body, b"picture");

    let requests = server.requests();
    let fetch = requests
        .iter()
        .find(|r| r.path() == "/photo/:/transcode")
        .unwrap();
    assert_eq!(fetch.header("X-Plex-Token"), Some("server-secret"));
    assert!(!fetch.url.contains("secret"));
}
