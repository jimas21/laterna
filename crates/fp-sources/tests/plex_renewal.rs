//! A token that stopped working and a server that moved, without live
//! credentials: the account is asked once in a while, and a sign-in nothing
//! can renew is reported as signed out rather than as a failed renewal.

mod common;

use common::{Req, Resp, TestServer, resp};
use fp_sources::plex::PlexSource;
use fp_sources::plex::auth::{PlexAuthClient, PlexConnection, PlexCredentials};
use fp_sources::{Catalog, Error, PlexConfig, Source, SourceConfig};
use serde_json::{Value, json};

fn json_reply(value: &Value) -> Resp {
    resp(
        200,
        serde_json::to_vec(value).unwrap(),
        &[("Content-Type", "application/json")],
    )
}

/// A server that takes `accepted` for everything, and `leftover` for the
/// pages every sign-in can still open.
fn server_taking(accepted: &'static str, leftover: Option<&'static str>) -> TestServer {
    TestServer::start(move |request: &Req| {
        let token = request.header("X-Plex-Token");
        let body = match request.path() {
            "/identity" => json!({"MediaContainer":{"machineIdentifier":"machine-1"}}),
            "/" | "/library/sections" if token.is_some() && token == leftover => {
                json!({"MediaContainer":{"Directory":[]}})
            }
            _ if token != Some(accepted) => return resp(401, "unauthorized", &[]),
            "/" | "/library/sections" => json!({"MediaContainer":{"Directory":[]}}),
            "/library/metadata/100" => {
                json!({"MediaContainer":{"Metadata":[{"ratingKey":"100","type":"movie","title":"Film"}]}})
            }
            _ => return resp(404, "missing", &[]),
        };
        json_reply(&body)
    })
}

/// plex.tv as it answers the list of servers: `listed` for the account's
/// token, a refusal for anything else.
fn account_listing(token: &'static str, listed: Value) -> TestServer {
    TestServer::start(move |request: &Req| {
        if request.path() != "/resources" || request.header("X-Plex-Token") != Some(token) {
            return resp(401, "unauthorized", &[]);
        }
        json_reply(&listed)
    })
}

fn row(access_token: &str, uris: &[&str]) -> Value {
    let connections: Vec<Value> = uris
        .iter()
        .map(|uri| json!({"uri":uri,"local":true,"relay":false}))
        .collect();
    json!({"name":"My Plex","provides":"server","owned":true,"clientIdentifier":"machine-1",
        "accessToken":access_token,"connections":connections})
}

fn connection(uri: &str) -> PlexConnection {
    let listed = json!({"uri":uri,"local":true,"relay":false,"protocol":"http"});
    serde_json::from_value(listed).unwrap()
}

fn config(server: &TestServer, token: &str) -> PlexConfig {
    PlexConfig {
        id: "plex-one".into(),
        name: "My Plex".into(),
        url: server.base.clone(),
        machine_identifier: "machine-1".into(),
        access_token: token.into(),
        ..Default::default()
    }
}

fn linked(mut config: PlexConfig) -> PlexConfig {
    config.auth = Some(PlexCredentials::manual(
        "account-secret".into(),
        "linked-client".into(),
    ));
    config
}

fn asked_about_servers(account: &TestServer) -> usize {
    account.count(|r| r.path() == "/resources")
}

fn source(config: &PlexConfig, account: &TestServer) -> PlexSource {
    let api = PlexAuthClient::with_test_endpoint(&account.base).unwrap();
    PlexSource::new(config).unwrap().with_auth_api(api)
}

fn nothing_secret_in_a_url(servers: &[&TestServer]) {
    for server in servers {
        for request in server.requests() {
            assert!(!request.url.contains("secret"), "{}", request.url);
            assert!(!request.url.contains("server-"), "{}", request.url);
        }
    }
}

#[test]
fn a_rejected_server_token_is_replaced_from_the_account() {
    let server = server_taking("server-new", None);
    let listed = json!([row("server-new", &[&server.base])]);
    let account = account_listing("account-secret", listed);
    let config = linked(config(&server, "server-old"));
    let source = source(&config, &account);

    source.probe().unwrap();
    assert_eq!(asked_about_servers(&account), 1);
    let Some(SourceConfig::Plex(saved)) = source.updated_config() else {
        panic!("the replaced token must be saved");
    };
    assert_eq!(saved.access_token, "server-new");
    assert_eq!(saved.connections, [connection(&server.base)]);
    assert_eq!(saved.id, config.id);
    // The account's own token is what asked; the server never sees it.
    let asked = account.requests();
    assert_eq!(
        asked[0].header("X-Plex-Token"),
        Some("account-secret"),
        "the account is asked with its own token"
    );
    assert_eq!(
        asked[0].header("X-Plex-Client-Identifier"),
        Some("linked-client")
    );
    assert!(
        server
            .requests()
            .iter()
            .all(|r| r.header("X-Plex-Token") != Some("account-secret"))
    );
    assert!(source.updated_config().is_none());
    nothing_secret_in_a_url(&[&server, &account]);
}

#[test]
fn a_token_that_dies_mid_session_is_replaced_and_the_read_retried() {
    let server = server_taking("server-new", Some("server-old"));
    let listed = json!([row("server-new", &[&server.base])]);
    let account = account_listing("account-secret", listed);
    let config = linked(config(&server, "server-old"));
    let source = source(&config, &account);

    source.probe().unwrap();
    assert_eq!(asked_about_servers(&account), 0, "still signed in");
    assert!(source.updated_config().is_none());
    assert_eq!(source.item("100").unwrap().name, "Film");
    assert_eq!(asked_about_servers(&account), 1);
    let Some(SourceConfig::Plex(saved)) = source.updated_config() else {
        panic!("the replaced token must be saved");
    };
    assert_eq!(saved.access_token, "server-new");
    nothing_secret_in_a_url(&[&server, &account]);
}

#[test]
fn the_same_token_handed_back_means_signed_out() {
    let server = server_taking("server-new", Some("server-old"));
    let listed = json!([row("server-old", &[&server.base])]);
    let account = account_listing("account-secret", listed);
    let mut config = linked(config(&server, "server-old"));
    config.connections = vec![connection(&server.base)];
    let source = source(&config, &account);

    source.probe().unwrap();
    let error = source.item("100").unwrap_err();
    assert!(matches!(error, Error::SignedOut(_)), "{error}");
    assert_eq!(asked_about_servers(&account), 1);
    assert!(source.updated_config().is_none(), "nothing was renewed");
    nothing_secret_in_a_url(&[&server, &account]);
}

#[test]
fn a_server_that_moved_is_found_again() {
    let old = TestServer::start(|_| resp(503, "gone", &[]));
    let new = server_taking("server-new", None);
    let listed = json!([row("server-new", &[&new.base])]);
    let account = account_listing("account-secret", listed);
    let mut config = linked(config(&old, "server-new"));
    config.connections = vec![connection(&old.base)];
    let source = source(&config, &account);

    source.probe().unwrap();
    assert_eq!(asked_about_servers(&account), 1);
    let Some(SourceConfig::Plex(saved)) = source.updated_config() else {
        panic!("the new address must be saved");
    };
    assert_eq!(saved.url, new.base);
    assert_eq!(saved.connections, [connection(&new.base)]);
    assert!(new.count(|r| r.path() == "/library/sections") > 0);
    nothing_secret_in_a_url(&[&old, &new, &account]);
}

#[test]
fn a_source_with_no_account_does_not_look_for_the_server() {
    let old = TestServer::start(|_| resp(503, "gone", &[]));
    let account = account_listing("account-secret", json!([]));
    let config = config(&old, "server-new");
    let source = source(&config, &account);

    let error = source.probe().unwrap_err();
    assert!(!matches!(error, Error::SignedOut(_)), "{error}");
    assert_eq!(asked_about_servers(&account), 0);
}

#[test]
fn rediscovery_is_rate_limited_and_offline_is_not_signed_out() {
    let server = server_taking("server-new", None);
    // plex.tv is out of reach; the server itself refuses the token.
    let account = TestServer::start(|_| resp(503, "unavailable", &[]));
    let config = linked(config(&server, "server-old"));
    let source = source(&config, &account);

    let first = source.probe().unwrap_err();
    assert!(!matches!(first, Error::SignedOut(_)), "{first}");
    let asked = asked_about_servers(&account);
    assert!(asked >= 1);
    for _ in 0..5 {
        let error = source.probe().unwrap_err();
        assert!(!matches!(error, Error::SignedOut(_)), "{error}");
    }
    let again = asked_about_servers(&account);
    assert_eq!(again, asked, "asked once a minute");
    assert!(source.updated_config().is_none());
    nothing_secret_in_a_url(&[&server, &account]);
}

#[test]
fn the_account_saying_no_means_signed_out() {
    let server = server_taking("server-new", None);
    let account = TestServer::start(|_| resp(401, "unauthorized", &[]));
    let config = linked(config(&server, "server-old"));
    let source = source(&config, &account);

    for _ in 0..3 {
        let error = source.probe().unwrap_err();
        assert!(matches!(error, Error::SignedOut(_)), "{error}");
        let shown = format!("{error} {error:?}");
        assert!(!shown.contains("secret"), "{shown}");
    }
    assert_eq!(asked_about_servers(&account), 1, "the answer is kept");
    assert!(source.updated_config().is_none());
    nothing_secret_in_a_url(&[&server, &account]);
}

#[test]
fn a_pasted_token_that_stopped_working_says_signed_out() {
    for status in [401, 498] {
        let server = TestServer::start(move |request: &Req| {
            let body = match request.path() {
                "/identity" => json!({"MediaContainer":{"machineIdentifier":"machine-1"}}),
                "/library/metadata/100" => return resp(status, "token rejected", &[]),
                _ => json!({"MediaContainer":{"Directory":[]}}),
            };
            json_reply(&body)
        });
        let account = account_listing("account-secret", json!([]));
        let config = config(&server, "typed-token");
        let source = source(&config, &account);

        source.probe().unwrap();
        let error = source.item("100").unwrap_err();
        assert!(matches!(error, Error::SignedOut(_)), "{error}");
        let asked = asked_about_servers(&account);
        assert_eq!(asked, 0, "a typed token has no account");
        nothing_secret_in_a_url(&[&server]);
    }
}
