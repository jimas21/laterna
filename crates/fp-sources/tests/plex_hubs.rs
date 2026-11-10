//! A Plex server's front page, library pages and collections are the server's
//! own rows in the server's order. The replies are made-up ones in the shape a
//! server sends (`fixtures/plex/hubs_*.json`, `collections.json`).

mod common;

use common::{Req, Resp, TestServer, resp};
use fp_sources::plex::PlexSource;
use fp_sources::{Catalog, CatalogKind, CatalogQuery, CatalogSort, PlexConfig, RowShape, Source};
use serde_json::{Value, json};
use std::collections::HashMap;

const PROMOTED: &str = include_str!("fixtures/plex/hubs_promoted.json");
const GLOBAL: &str = include_str!("fixtures/plex/hubs_global.json");
const SECTION: &str = include_str!("fixtures/plex/hubs_section.json");
const COLLECTIONS: &str = include_str!("fixtures/plex/collections.json");
const COLLECTION_CHILDREN: &str = include_str!("fixtures/plex/collection_children.json");

const JSON: [(&str, &str); 1] = [("Content-Type", "application/json")];

/// What the made-up server offers for the front page.
#[derive(Clone, Copy, PartialEq)]
enum Hubs {
    /// `/hubs/promoted` and `/hubs` both answer.
    Promoted,
    /// `/hubs/promoted` holds only Continue Watching; `/hubs` answers.
    OnlyContinue,
    /// `/hubs/promoted` is missing; `/hubs` answers.
    Global,
    /// Neither answers: an older server.
    Neither,
    /// `/hubs/promoted` names listings that are not the server's to give.
    Hostile,
}

/// Keys a server must not be able to send this app to.
const HOSTILE: [&str; 7] = [
    "http://evil/x",
    "//evil/x",
    "/hubs/../x",
    "/hubs/%2e%2e/x",
    "/library?X-Plex-Token=a",
    "/library/:/scrobble?key=1",
    "/hubs\\x",
];

fn film(id: u32) -> Value {
    json!({"ratingKey": id.to_string(), "type": "movie", "title": format!("Film {id}"),
        "duration": 600000, "viewCount": 0, "year": 2020})
}

fn hostile_hubs() -> Value {
    let hubs: Vec<Value> = HOSTILE
        .iter()
        .enumerate()
        .map(|(i, key)| {
            json!({"hubIdentifier": format!("hostile.{i}"), "title": format!("Row {i}"),
                "type": "movie", "more": true, "key": key, "Metadata": [film(100 + i as u32)]})
        })
        .collect();
    json!({"MediaContainer": {"Hub": hubs}})
}

fn json_reply(value: &Value) -> Resp {
    resp(200, serde_json::to_vec(value).unwrap(), &JSON)
}

fn fixture(text: &str) -> Resp {
    resp(200, text.as_bytes().to_vec(), &JSON)
}

/// A reply holding these items.
fn items(items: Vec<Value>) -> Value {
    json!({"MediaContainer": {"totalSize": items.len(), "Metadata": items}})
}

fn sections() -> Value {
    json!({"MediaContainer": {"Directory": [
        {"key": "1", "type": "movie", "title": "Films"},
        {"key": "2", "type": "show", "title": "Series"},
        {"key": "3", "type": "artist", "title": "Music"}]}})
}

/// A front page that holds only Continue Watching.
fn only_continue() -> Value {
    json!({"MediaContainer": {"Hub": [
        {"hubIdentifier": "home.continue", "title": "Continue Watching", "type": "movie",
            "context": "hub.home.continue", "Metadata": [film(10)]}]}})
}

fn made_up_show() -> Value {
    json!({"ratingKey": "20", "type": "show", "title": "Made-Up Show"})
}

fn collection_100() -> Value {
    json!({"ratingKey": "100", "type": "collection", "title": "Staff Picks",
        "childCount": "3", "thumb": "/library/collections/100/composite/1700000100"})
}

fn video_playlist() -> Value {
    json!({"ratingKey": "300", "type": "playlist", "playlistType": "video",
        "title": "Rainy Day Mix", "leafCount": 7,
        "composite": "/playlists/300/composite/1700000300"})
}

fn music_playlist() -> Value {
    json!({"ratingKey": "301", "type": "playlist", "playlistType": "audio",
        "title": "Road Songs", "leafCount": 12})
}

fn reply(mode: Hubs, request: &Req) -> Resp {
    if request.path() != "/identity" && request.header("X-Plex-Token") != Some("server-secret") {
        return resp(401, b"unauthorized".to_vec(), &[]);
    }
    let params = params(request);
    let start: u32 = params
        .get("X-Plex-Container-Start")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let collections = params.get("type").is_some_and(|t| t == "18");
    match request.path() {
        "/identity" => json_reply(&json!({"MediaContainer": {"machineIdentifier": "m-1"}})),
        "/" => json_reply(&json!({"MediaContainer": {"friendlyName": "Test Plex"}})),
        "/library/sections" => json_reply(&sections()),
        "/hubs/continueWatching/items" => json_reply(&items(vec![film(10)])),
        "/hubs/promoted" => match mode {
            Hubs::Promoted => fixture(PROMOTED),
            Hubs::Hostile => json_reply(&hostile_hubs()),
            Hubs::OnlyContinue => json_reply(&only_continue()),
            Hubs::Global | Hubs::Neither => resp(404, Vec::new(), &[]),
        },
        "/hubs" if mode != Hubs::Neither => fixture(GLOBAL),
        "/hubs/sections/1" => fixture(SECTION),
        "/hubs/home/recentlyAdded" => {
            let mut page = items(vec![film(120), film(121)]);
            page["MediaContainer"]["offset"] = json!(start);
            page["MediaContainer"]["totalSize"] = json!(62);
            json_reply(&page)
        }
        "/library/sections/1/all" if collections => fixture(COLLECTIONS),
        "/library/sections/1/all" => json_reply(&items(vec![film(10), film(11)])),
        "/library/sections/2/all" => json_reply(&items(vec![made_up_show()])),
        "/library/collections/100/children" => fixture(COLLECTION_CHILDREN),
        "/library/metadata/100" => json_reply(&items(vec![collection_100()])),
        "/playlists/300" => json_reply(&items(vec![video_playlist()])),
        "/playlists/301" => json_reply(&items(vec![music_playlist()])),
        "/playlists/300/items" => json_reply(&items(vec![film(105), film(103)])),
        _ => resp(404, Vec::new(), &[]),
    }
}

fn start(mode: Hubs) -> TestServer {
    TestServer::start(move |request| reply(mode, request))
}

fn source(server: &TestServer) -> PlexSource {
    let config: PlexConfig = serde_json::from_value(json!({
        "id": "plex-test",
        "name": "Test Plex",
        "url": server.base,
        "machine_identifier": "machine-1",
        "access_token": "server-secret",
    }))
    .unwrap();
    PlexSource::new(&config).unwrap()
}

/// The query arguments a request carried.
fn params(request: &Req) -> HashMap<String, String> {
    url::Url::parse(&format!("http://test{}", request.url))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

/// The first request the server saw for `path`.
fn asked(server: &TestServer, path: &str) -> Req {
    server
        .requests()
        .into_iter()
        .find(|r| r.path() == path)
        .unwrap_or_else(|| panic!("{path} was never asked for"))
}

/// The server picture a card's picture location asks for.
fn picture(source: &PlexSource, location: &str) -> String {
    let (_, url) = source.picture_request(location).unwrap();
    let url = url::Url::parse(&url).unwrap();
    let params: HashMap<String, String> = url.query_pairs().into_owned().collect();
    params["url"].clone()
}

fn lists_a_library(request: &Req) -> bool {
    request.path().starts_with("/library/sections/") && request.path().ends_with("/all")
}

fn no_token_in_any_url(server: &TestServer) {
    for request in server.requests() {
        assert!(!request.url.contains("secret"), "{}", request.url);
    }
}

#[test]
fn the_home_page_is_the_servers_own_rows_in_its_order() {
    let server = start(Hubs::Promoted);
    let source = source(&server);
    let home = source.home().unwrap();

    let titles: Vec<&str> = home.rows.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Recently Added Movies",
            "Staff Picks",
            "Recently Added Episodes",
            "Mixed Bag",
            "Collections",
        ],
        "Continue Watching, On Deck, music and empty rows are left out"
    );
    let shapes: Vec<RowShape> = home.rows.iter().map(|r| r.shape).collect();
    assert_eq!(
        shapes,
        [
            RowShape::Portrait,
            RowShape::Portrait,
            RowShape::Landscape,
            RowShape::Portrait,
            RowShape::Portrait,
        ]
    );
    assert_eq!(home.libraries.len(), 2, "the music library is not shown");
    assert_eq!(home.resume.len(), 1);
    assert!(home.next_up.is_empty());
    assert!(home.latest.is_empty(), "the server's rows replace them");

    let movies = &home.rows[0];
    assert_eq!(movies.id, "movie.recentlyadded.1");
    assert_eq!(movies.library.as_deref(), Some("section:1"));
    assert_eq!(movies.items.len(), 3);
    let more = movies.more.as_ref().expect("the server has more");
    assert_eq!(
        more.hub.as_deref(),
        Some("/hubs/home/recentlyAdded?type=1&sectionID=1")
    );
    assert_eq!(more.sort, CatalogSort::Server);
    assert_eq!(home.rows[2].library.as_deref(), Some("section:2"));
    assert_eq!(
        home.rows[1].more.as_ref().and_then(|m| m.hub.as_deref()),
        Some("/library/collections/100/children")
    );

    let mixed = &home.rows[3];
    assert_eq!(mixed.items.len(), 1, "the photo is not ours to show");
    assert!(mixed.more.is_none(), "the server has no more of it");

    let promoted = params(&asked(&server, "/hubs/promoted"));
    assert_eq!(promoted["contentDirectoryID"], "1,2");
    assert_eq!(promoted["pinnedContentDirectoryID"], "1,2");
    assert_eq!(promoted["excludeContinueWatching"], "1");
    assert_eq!(promoted["count"], "12");
    let listed = server.count(lists_a_library);
    assert_eq!(listed, 0, "no library was listed for the page");
    no_token_in_any_url(&server);
}

#[test]
fn a_collections_row_shows_the_collections_own_posters() {
    let server = start(Hubs::Promoted);
    let source = source(&server);
    let home = source.home().unwrap();

    let row = home.rows.iter().find(|r| r.title == "Collections").unwrap();
    let ids: Vec<&str> = row.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["collection:100", "collection:101"]);
    assert!(row.items.iter().all(|i| i.kind == CatalogKind::BoxSet));
    let poster = row.items[0].poster.as_deref().expect("a poster");
    assert_eq!(
        picture(&source, poster),
        "/library/collections/100/composite/170000100"
    );
    assert!(
        !row.items[0].location.contains("collection:"),
        "the location holds the server's own key"
    );
}

#[test]
fn rows_fall_back_to_global_hubs_then_to_latest_rows() {
    for mode in [Hubs::Global, Hubs::OnlyContinue] {
        let server = start(mode);
        let home = source(&server).home().unwrap();
        let titles: Vec<&str> = home.rows.iter().map(|r| r.title.as_str()).collect();
        let want = ["Recently Added Movies", "Recently Added Episodes"];
        assert_eq!(titles, want);
        let global = params(&asked(&server, "/hubs"));
        assert_eq!(global["contentDirectoryID"], "1,2");
        assert_eq!(global["count"], "12");
        assert!(!global.contains_key("excludeContinueWatching"));
        assert!(home.latest.is_empty());
    }

    let server = start(Hubs::Neither);
    let home = source(&server).home().unwrap();
    assert!(home.rows.is_empty());
    assert_eq!(home.resume.len(), 1);
    let names: Vec<&str> = home.latest.iter().map(|(l, _)| l.name.as_str()).collect();
    assert_eq!(names, ["Films", "Series"], "the newest of each library");
    assert_eq!(home.latest[0].1.len(), 2);
}

#[test]
fn see_all_pages_through_the_rows_own_listing() {
    let server = start(Hubs::Promoted);
    let source = source(&server);
    let home = source.home().unwrap();
    let more = home.rows[0].more.clone().expect("the server has more");

    let mut next = more;
    next.start = 60;
    let page = source.items(&next).unwrap();
    assert_eq!(page.total, 62);
    let ids: Vec<&str> = page.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["120", "121"]);

    let listing = asked(&server, "/hubs/home/recentlyAdded");
    let sent = params(&listing);
    assert_eq!(sent["type"], "1");
    assert_eq!(sent["sectionID"], "1");
    assert_eq!(sent["X-Plex-Container-Start"], "60");
    assert_eq!(sent["X-Plex-Container-Size"], "60");
    assert!(!sent.contains_key("sort"), "{}", listing.url);
    assert_eq!(listing.url.matches("type=").count(), 1, "{}", listing.url);
    no_token_in_any_url(&server);
}

#[test]
fn a_row_path_from_the_server_must_stay_on_it() {
    let server = start(Hubs::Hostile);
    let source = source(&server);
    let home = source.home().unwrap();

    assert_eq!(home.rows.len(), HOSTILE.len(), "the rows themselves show");
    for row in &home.rows {
        assert!(row.more.is_none(), "{}: no See all", row.title);
    }

    let before = server.requests().len();
    for key in HOSTILE {
        let result = source.items(&CatalogQuery {
            hub: Some(key.into()),
            sort: CatalogSort::Server,
            ..CatalogQuery::default()
        });
        assert!(result.is_err(), "{key} was asked for");
    }
    assert_eq!(server.requests().len(), before, "nothing was sent");
}

#[test]
fn collections_open_in_their_own_order() {
    let server = start(Hubs::Neither);
    let source = source(&server);
    let collection = source.item("collection:100").unwrap();
    assert_eq!(collection.kind, CatalogKind::BoxSet);
    assert_eq!(collection.id, "collection:100");

    let query = CatalogQuery::children(&collection);
    assert_eq!(query.sort, CatalogSort::Server);
    let page = source.items(&query).unwrap();
    let ids: Vec<&str> = page.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["105", "103", "104"], "the server's order");

    let listing = asked(&server, "/library/collections/100/children");
    assert!(!listing.url.contains("sort="), "{}", listing.url);
    assert!(!listing.url.contains("type="), "{}", listing.url);
}

#[test]
fn a_librarys_recommended_rows_are_its_section_hubs() {
    let server = start(Hubs::Neither);
    let source = source(&server);
    let rows = source.library_rows("section:1").unwrap();

    let titles: Vec<&str> = rows.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Continue Watching", "Recently Released Movies", "Staff Picks"],
        "the library's own Continue Watching stays"
    );
    for row in &rows {
        assert_eq!(row.library.as_deref(), Some("section:1"));
    }
    let listing = asked(&server, "/hubs/sections/1");
    assert_eq!(params(&listing)["count"], "12");

    // A server without library hubs opens its libraries as grids.
    assert!(source.library_rows("section:9").unwrap().is_empty());
    let before = server.requests().len();
    assert!(source.library_rows("not-a-section").unwrap().is_empty());
    assert_eq!(server.requests().len(), before);
    no_token_in_any_url(&server);
}

#[test]
fn library_collections_are_a_grid_of_type_18() {
    let server = start(Hubs::Neither);
    let source = source(&server);
    let page = source
        .items(&CatalogQuery {
            parent: Some("section:1".into()),
            types: vec![CatalogKind::BoxSet],
            ..CatalogQuery::default()
        })
        .unwrap();

    assert_eq!(page.total, 2);
    let names: Vec<&str> = page.items.iter().map(|i| i.name.as_str()).collect();
    assert_eq!(names, ["Staff Picks", "Rainy Day"]);
    assert!(page.items.iter().all(|i| i.id.starts_with("collection:")));
    let listing = asked(&server, "/library/sections/1/all");
    assert_eq!(params(&listing)["type"], "18");
    let poster = page.items[1].poster.as_deref().expect("a poster");
    assert_eq!(
        picture(&source, poster),
        "/library/collections/101/composite/170000101"
    );
}

#[test]
fn a_playlist_is_a_card_of_the_servers_collage_and_lists_in_order() {
    let server = start(Hubs::Neither);
    let source = source(&server);
    let playlist = source.item("playlist:300").unwrap();
    assert_eq!(playlist.kind, CatalogKind::Playlist);
    assert_eq!(playlist.id, "playlist:300");
    assert_eq!(playlist.child_count, Some(7));
    let landscape = playlist.landscape.as_deref().expect("the collage");
    assert_eq!(
        picture(&source, landscape),
        "/playlists/300/composite/1700000300"
    );

    let page = source.items(&CatalogQuery::children(&playlist)).unwrap();
    let ids: Vec<&str> = page.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["105", "103"]);
    let listing = asked(&server, "/playlists/300/items");
    assert!(!listing.url.contains("sort="), "{}", listing.url);

    assert!(
        source.item("playlist:301").is_err(),
        "a music playlist is not ours to show"
    );
}
