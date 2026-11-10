//! The shared catalog's server rows, server order and playlists as Plex meets
//! them: a server order sends no sort, and a source with no rows of its own
//! keeps the front page it had.

mod common;

use common::{Req, Resp, TestServer, resp};
use fp_sources::plex::PlexSource;
use fp_sources::{Catalog, CatalogKind, CatalogQuery, CatalogSort, PlexConfig};
use serde_json::{Value, json};

fn film(id: &str) -> Value {
    json!({"ratingKey": id, "type": "movie", "title": format!("Film {id}"), "duration": 600000,
        "viewCount": 0, "year": 2020})
}

fn reply(request: &Req) -> Resp {
    if request.path() != "/identity" && request.header("X-Plex-Token") != Some("server-secret") {
        return resp(401, b"unauthorized".to_vec(), &[]);
    }
    let value = match request.path() {
        "/identity" => json!({"MediaContainer": {"machineIdentifier": "machine-1"}}),
        "/" => json!({"MediaContainer": {"friendlyName": "Test Plex"}}),
        "/library/sections" => {
            json!({"MediaContainer":{"Directory":[{"key":"1","type":"movie","title":"Films"}]}})
        }
        "/library/sections/1/all" => {
            json!({"MediaContainer": {"totalSize": 2, "Metadata": [film("10"), film("11")]}})
        }
        "/library/metadata/500/children" => {
            json!({"MediaContainer": {"totalSize": 2, "Metadata": [film("11"), film("10")]}})
        }
        "/hubs/continueWatching/items" => json!({"MediaContainer": {"Metadata": [film("10")]}}),
        _ => return resp(404, Vec::new(), &[]),
    };
    resp(
        200,
        serde_json::to_vec(&value).unwrap(),
        &[("Content-Type", "application/json")],
    )
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

#[test]
fn the_servers_own_order_sends_no_sort() {
    let server = TestServer::start(reply);
    let source = source(&server);
    let kept = source
        .items(&CatalogQuery {
            parent: Some("500".into()),
            sort: CatalogSort::Server,
            ..CatalogQuery::default()
        })
        .unwrap();
    let ids: Vec<&str> = kept.items.iter().map(|i| i.id.as_str()).collect();
    assert_eq!(ids, ["11", "10"], "the order the server answered in");
    assert_eq!(kept.items[0].kind, CatalogKind::Movie);
    let asked = server
        .requests()
        .into_iter()
        .find(|r| r.path() == "/library/metadata/500/children")
        .expect("the collection was listed");
    assert!(!asked.url.contains("sort="), "{}", asked.url);

    source
        .items(&CatalogQuery {
            parent: Some("section:1".into()),
            sort: CatalogSort::Name,
            ..CatalogQuery::default()
        })
        .unwrap();
    let sorted = server
        .requests()
        .into_iter()
        .find(|r| r.path() == "/library/sections/1/all")
        .expect("the library was listed");
    let url = url::Url::parse(&format!("http://test{}", sorted.url)).unwrap();
    let sort = url.query_pairs().find(|(k, _)| k == "sort");
    assert_eq!(
        sort.map(|(_, v)| v.into_owned()).as_deref(),
        Some("titleSort:asc")
    );
}

#[test]
fn a_source_without_rows_of_its_own_keeps_its_front_page() {
    let server = TestServer::start(reply);
    let source = source(&server);
    let caps = source.capabilities();
    assert!(caps.library_pages);
    let home = source.home().unwrap();
    assert!(home.rows.is_empty());
    assert_eq!(home.resume.len(), 1);
    assert_eq!(home.latest.len(), 1);

    // A server too old for library hubs opens its libraries as grids.
    assert!(source.library_rows("section:1").unwrap().is_empty());

    let before = server.requests().len();
    assert!(source.playlists().unwrap().is_empty());
    assert!(source.history(20).unwrap().is_empty());
    assert_eq!(
        server.requests().len(),
        before,
        "the defaults ask the server nothing"
    );
}
