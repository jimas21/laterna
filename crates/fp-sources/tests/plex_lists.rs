//! A Plex server's video playlists and its watch history, as the catalog
//! lists them. The replies are made-up ones in the shape a server sends.

mod common;

use common::{Req, Resp, TestServer, resp};
use fp_sources::plex::PlexSource;
use fp_sources::{Catalog, CatalogItem, CatalogKind, CatalogQuery, CatalogSort, PlexConfig};
use serde_json::{Value, json};
use std::collections::HashMap;

const JSON: [(&str, &str); 1] = [("Content-Type", "application/json")];

fn film(id: u32, section: u32, viewed: Option<i64>) -> Value {
    let mut v = json!({"ratingKey": id.to_string(), "type": "movie",
        "title": format!("Film {id}"), "duration": 600000, "librarySectionID": section});
    if let Some(at) = viewed {
        v["lastViewedAt"] = json!(at);
        v["viewCount"] = json!(1);
    }
    v
}

fn episode(id: u32, section: u32, viewed: Option<i64>) -> Value {
    let mut v = json!({"ratingKey": id.to_string(), "type": "episode",
        "title": format!("Episode {id}"), "grandparentTitle": "Made-Up Show",
        "grandparentRatingKey": "900", "parentIndex": 1, "index": 1,
        "duration": 600000, "librarySectionID": section});
    if let Some(at) = viewed {
        v["lastViewedAt"] = json!(at);
        v["viewCount"] = json!(1);
    }
    v
}

fn playlist(id: u32, kind: &str) -> Value {
    json!({"ratingKey": id.to_string(), "type": "playlist", "title": format!("List {id}"),
        "playlistType": kind, "leafCount": 3, "composite": format!("/playlists/{id}/composite/1")})
}

fn json_reply(value: &Value) -> Resp {
    resp(200, serde_json::to_vec(value).unwrap(), &JSON)
}

fn items(items: Vec<Value>) -> Resp {
    json_reply(&json!({"MediaContainer": {"totalSize": items.len(), "Metadata": items}}))
}

fn params(request: &Req) -> HashMap<String, String> {
    url::Url::parse(&format!("http://test{}", request.url))
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect()
}

fn reply(request: &Req) -> Resp {
    if request.path() != "/identity" && request.header("X-Plex-Token") != Some("server-secret") {
        return resp(401, b"unauthorized".to_vec(), &[]);
    }
    let asked = params(request);
    match request.path() {
        "/identity" => json_reply(&json!({"MediaContainer": {"machineIdentifier": "machine-1"}})),
        "/" => json_reply(&json!({"MediaContainer": {"friendlyName": "Test Plex"}})),
        "/playlists" => items(vec![
            playlist(50, "video"),
            playlist(51, "audio"),
            playlist(52, "video"),
        ]),
        // In the playlist's own order, which is not the title order.
        "/playlists/50/items" => items(vec![
            film(33, 1, None),
            episode(21, 2, Some(10)),
            film(11, 1, None),
            film(44, 4, None),
        ]),
        "/library/all" => match asked.get("type").map(String::as_str) {
            Some("1") => items(vec![
                film(12, 1, Some(500)),
                film(13, 1, Some(300)),
                film(41, 4, Some(250)),
                film(14, 1, None),
                film(15, 1, None),
            ]),
            Some("4") => items(vec![
                episode(22, 2, Some(400)),
                episode(23, 2, Some(100)),
                episode(24, 2, None),
            ]),
            _ => resp(400, Vec::new(), &[]),
        },
        _ => resp(404, Vec::new(), &[]),
    }
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

#[test]
fn video_playlists_are_listed_with_their_collage() {
    let server = TestServer::start(reply);
    let lists = plex(&server, &[]).playlists().unwrap();
    assert_eq!(
        ids(&lists),
        ["playlist:50", "playlist:52"],
        "the audio playlist is not ours to show"
    );
    assert!(lists.iter().all(|l| l.kind == CatalogKind::Playlist));
    assert_eq!(lists[0].name, "List 50");
    assert_eq!(lists[0].child_count, Some(3));
    assert!(
        lists[0].landscape.is_some() && lists[0].poster.is_some(),
        "the server's collage stands in for a poster"
    );
    let asked = server
        .requests()
        .into_iter()
        .find(|r| r.path() == "/playlists")
        .unwrap();
    assert_eq!(params(&asked)["playlistType"], "video");
    for request in server.requests() {
        assert!(!request.url.contains("secret"), "{}", request.url);
    }
}

#[test]
fn a_playlist_pages_in_its_own_order() {
    let server = TestServer::start(reply);
    let source = plex(&server, &[]);
    let page = source
        .items(&CatalogQuery {
            parent: Some("playlist:50".into()),
            sort: CatalogSort::Server,
            limit: 48,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(ids(&page.items), ["33", "21", "11", "44"]);
    let asked = server
        .requests()
        .into_iter()
        .find(|r| r.path() == "/playlists/50/items")
        .expect("the playlist's own listing was asked for");
    assert!(
        !params(&asked).contains_key("sort"),
        "no sort: the playlist keeps its order"
    );
}

#[test]
fn server_history_merges_films_and_episodes_by_last_watched() {
    let server = TestServer::start(reply);
    let history = plex(&server, &[]).history(60).unwrap();
    assert_eq!(ids(&history), ["12", "22", "13", "41", "23"]);
    assert_eq!(history[0].last_viewed, Some(500));
    let asked: Vec<_> = server
        .requests()
        .into_iter()
        .filter(|r| r.path() == "/library/all")
        .map(|r| params(&r))
        .collect();
    assert_eq!(asked.len(), 2, "films and episodes, each asked for");
    for p in &asked {
        assert_eq!(p["sort"], "lastViewedAt:desc");
        assert_eq!(p["X-Plex-Container-Size"], "60");
    }
    let kinds: Vec<&str> = asked.iter().map(|p| p["type"].as_str()).collect();
    assert_eq!(kinds, ["1", "4"]);
}

#[test]
fn history_takes_only_as_many_as_asked_newest_first() {
    let server = TestServer::start(reply);
    let history = plex(&server, &[]).history(3).unwrap();
    assert_eq!(ids(&history), ["12", "22", "13"]);
}

#[test]
fn history_stops_at_what_was_never_watched() {
    let server = TestServer::start(reply);
    let history = plex(&server, &[]).history(60).unwrap();
    for never in ["14", "15", "24"] {
        assert!(!ids(&history).contains(&never), "{never} was never watched");
    }
}

#[test]
fn history_leaves_out_hidden_libraries() {
    let server = TestServer::start(reply);
    let history = plex(&server, &["4", "2"]).history(60).unwrap();
    assert_eq!(ids(&history), ["12", "13"]);
}
