//! A library the user left out of a Plex source is gone from its tiles, rows,
//! search and Continue Watching, but not from the settings that choose it.
//! The replies are made-up ones in the shape a server sends.

mod common;

use common::{Req, Resp, TestServer, resp};
use fp_sources::plex::PlexSource;
use fp_sources::{Catalog, CatalogItem, CatalogQuery, CatalogSort, PlexConfig, Source};
use serde_json::{Value, json};
use std::collections::HashMap;

const JSON: [(&str, &str); 1] = [("Content-Type", "application/json")];

fn film(id: u32, section: u32) -> Value {
    json!({"ratingKey": id.to_string(), "type": "movie", "title": format!("Film {id}"),
        "duration": 600000, "viewCount": 0, "librarySectionID": section})
}

/// An episode whose library is named the way some servers name it: as text.
fn episode(id: u32, section: u32) -> Value {
    json!({"ratingKey": id.to_string(), "type": "episode", "title": format!("Episode {id}"),
        "grandparentTitle": "Made-Up Show", "grandparentRatingKey": "900",
        "parentIndex": 1, "index": 1, "duration": 600000, "viewCount": 0,
        "librarySectionID": section.to_string()})
}

fn show(id: u32, section: u32) -> Value {
    json!({"ratingKey": id.to_string(), "type": "show", "title": "Made-Up Show",
        "librarySectionID": section})
}

fn collection(id: u32, section: u32) -> Value {
    json!({"ratingKey": id.to_string(), "type": "collection", "title": format!("Set {id}"),
        "childCount": "3", "librarySectionID": section})
}

fn json_reply(value: &Value) -> Resp {
    resp(200, serde_json::to_vec(value).unwrap(), &JSON)
}

fn items(items: Vec<Value>) -> Resp {
    json_reply(&json!({"MediaContainer": {"totalSize": items.len(), "Metadata": items}}))
}

fn hub(id: &str, title: &str, kind: &str, section: Option<Value>, metadata: Vec<Value>) -> Value {
    let mut hub = json!({"hubIdentifier": id, "title": title, "type": kind,
        "Metadata": metadata});
    if let Some(section) = section {
        hub["librarySectionID"] = section;
    }
    hub
}

fn reply(request: &Req) -> Resp {
    if request.path() != "/identity" && request.header("X-Plex-Token") != Some("server-secret") {
        return resp(401, b"unauthorized".to_vec(), &[]);
    }
    match request.path() {
        "/identity" => json_reply(&json!({"MediaContainer": {"machineIdentifier": "machine-1"}})),
        "/" => json_reply(&json!({"MediaContainer": {"friendlyName": "Test Plex"}})),
        "/library/sections" => json_reply(&json!({"MediaContainer": {"Directory": [
            {"key": "1", "type": "movie", "title": "Films"},
            {"key": "2", "type": "show", "title": "Series"},
            {"key": "3", "type": "artist", "title": "Music"},
            {"key": "4", "type": "movie", "title": "Cartoons"}]}})),
        "/hubs/continueWatching/items" => items(vec![film(10, 1), episode(20, 2), film(40, 4)]),
        "/hubs/promoted" => json_reply(&json!({"MediaContainer": {"Hub": [
            hub("movie.recent.1", "Recently Added Movies", "movie", Some(json!(1)),
                vec![film(11, 1), film(12, 1)]),
            hub("tv.recent.2", "Recently Added Episodes", "episode", Some(json!(2)),
                vec![episode(21, 2), episode(22, 2)]),
            hub("mixed", "Mixed Bag", "movie", None,
                vec![film(13, 1), film(41, 4), episode(23, 2)]),
            hub("set.series", "Series Picks", "collection", Some(json!("2")),
                vec![collection(101, 2)]),
            hub("set.cartoons", "Collections", "collection", Some(json!(4)),
                vec![collection(100, 4)])]}})),
        "/hubs/search" => json_reply(&json!({"MediaContainer": {"Hub": [
            hub("search.movie", "Movies", "movie", None, vec![film(14, 1), film(42, 4)]),
            hub("search.show", "Shows", "show", None, vec![show(24, 2)])]}})),
        "/hubs/home/recentlyAdded" => items(vec![film(15, 1), film(43, 4)]),
        "/library/collections/100/children" => items(vec![film(44, 4), film(45, 2)]),
        "/library/sections/2/all" => items(vec![show(24, 2)]),
        _ => resp(404, Vec::new(), &[]),
    }
}

fn start() -> TestServer {
    TestServer::start(reply)
}

fn plex(server: &TestServer, hidden: &[&str]) -> PlexSource {
    let config: PlexConfig = serde_json::from_value(json!({
        "id": "plex-test",
        "name": "Test Plex",
        "url": server.base,
        "machine_identifier": "machine-1",
        "access_token": "server-secret",
        "hidden_sections": hidden,
    }))
    .unwrap();
    PlexSource::new(&config).unwrap()
}

fn ids(items: &[CatalogItem]) -> Vec<&str> {
    items.iter().map(|i| i.id.as_str()).collect()
}

/// The query arguments a request carried.
fn params(request: &Req) -> HashMap<String, String> {
    url::Url::parse(&format!("http://test{}", request.url))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

#[test]
fn a_hidden_library_leaves_tiles_rows_and_search() {
    let server = start();
    let source = plex(&server, &["2"]);
    let home = source.home().unwrap();

    assert_eq!(ids(&home.libraries), ["section:1", "section:4"]);
    let titles: Vec<&str> = home.rows.iter().map(|r| r.title.as_str()).collect();
    assert_eq!(
        titles,
        ["Recently Added Movies", "Mixed Bag", "Collections"],
        "the rows of the hidden library are gone, the others stay in order"
    );
    let mixed = home.rows.iter().find(|r| r.title == "Mixed Bag").unwrap();
    assert_eq!(ids(&mixed.items), ["13", "41"], "a row of many libraries");

    let found = source.search("film", 10).unwrap();
    let kinds: Vec<&str> = found.iter().map(|(kind, _)| *kind).collect();
    assert_eq!(
        kinds,
        ["Movies"],
        "no Shows: all of them are in the hidden library"
    );
    assert_eq!(ids(&found[0].1), ["14", "42"]);

    // The library's folder list is the same as its tiles.
    let listed = source.list(None).unwrap();
    let names: Vec<&str> = listed.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["Films", "Cartoons"]);

    // What the source's settings choose from, and an id that was kept.
    let all = source.all_libraries().unwrap();
    assert_eq!(ids(&all), ["section:1", "section:2", "section:4"]);
    assert_eq!(source.item("section:2").unwrap().name, "Series");
    let opened = source
        .items(&CatalogQuery {
            parent: Some("section:2".into()),
            sort: CatalogSort::Server,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        ids(&opened.items),
        ["24"],
        "a library opened by id still lists"
    );

    for request in server.requests() {
        assert!(!request.url.contains("secret"), "{}", request.url);
    }
}

#[test]
fn nothing_is_hidden_by_default() {
    let server = start();
    let home = plex(&server, &[]).home().unwrap();
    assert_eq!(
        ids(&home.libraries),
        ["section:1", "section:2", "section:4"]
    );
    assert_eq!(home.rows.len(), 5);
    let all = plex(&server, &[]).all_libraries().unwrap();
    assert_eq!(all.len(), 3);
}

#[test]
fn home_asks_hubs_only_for_shown_libraries() {
    let server = start();
    plex(&server, &["2"]).home().unwrap();
    let promoted = server
        .requests()
        .into_iter()
        .find(|r| r.path() == "/hubs/promoted")
        .expect("the front page was asked for");
    let asked = params(&promoted);
    assert_eq!(asked["contentDirectoryID"], "1,4");
    assert_eq!(asked["pinnedContentDirectoryID"], "1,4");

    let server = start();
    plex(&server, &[]).home().unwrap();
    let promoted = server
        .requests()
        .into_iter()
        .find(|r| r.path() == "/hubs/promoted")
        .unwrap();
    assert_eq!(params(&promoted)["contentDirectoryID"], "1,2,4");
}

#[test]
fn continue_watching_leaves_out_hidden_libraries() {
    let server = start();
    let home = plex(&server, &["1", "4"]).home().unwrap();
    assert_eq!(ids(&home.resume), ["20"]);

    let home = plex(&server, &["2"]).home().unwrap();
    assert_eq!(ids(&home.resume), ["10", "40"]);

    let up = plex(&server, &["2"]).next_up(None, 10).unwrap();
    assert!(up.is_empty(), "the only episode is in the hidden library");
    let up = plex(&server, &["1"]).next_up(None, 10).unwrap();
    assert_eq!(ids(&up), ["20"]);
}

#[test]
fn a_rows_see_all_and_a_collection_leave_out_hidden_libraries() {
    let server = start();
    let source = plex(&server, &["2"]);
    let more = source
        .items(&CatalogQuery {
            hub: Some("/hubs/home/recentlyAdded?type=1".into()),
            sort: CatalogSort::Server,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ids(&more.items), ["15", "43"]);

    let hidden_both = plex(&server, &["4"])
        .items(&CatalogQuery {
            hub: Some("/hubs/home/recentlyAdded?type=1".into()),
            sort: CatalogSort::Server,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ids(&hidden_both.items), ["15"]);

    let set = source
        .items(&CatalogQuery {
            parent: Some("collection:100".into()),
            sort: CatalogSort::Server,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(
        ids(&set.items),
        ["44"],
        "a collection that holds another library's item"
    );
}

#[test]
fn hiding_every_library_still_connects() {
    let server = start();
    let source = plex(&server, &["1", "2", "4"]);
    let config = source
        .probe()
        .expect("the connection test goes by every library");
    assert_eq!(config.hidden_sections, ["1", "2", "4"]);

    let home = source.home().unwrap();
    assert!(home.libraries.is_empty());
    assert!(home.rows.is_empty());
    assert!(home.resume.is_empty());
    assert!(home.latest.is_empty());
    assert_eq!(
        server.count(|r| r.path().starts_with("/hubs/promoted") || r.path() == "/hubs"),
        0,
        "no row is asked for when none can be shown"
    );
    assert_eq!(source.all_libraries().unwrap().len(), 3);
    assert!(source.list(None).unwrap().is_empty());
}
