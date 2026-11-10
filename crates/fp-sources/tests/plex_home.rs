//! Plex Home members and the profile switch, against a local stand-in for
//! plex.tv. No Plex account, PIN or external network is used; every name,
//! id and token here is made up.

mod common;

use common::{Req, Resp, TestServer, resp};
use fp_sources::plex::PlexSource;
use fp_sources::plex::auth::{
    PlexAuthClient, PlexCredentials, PlexHomeUser, PlexProfile, is_wrong_pin,
};
use fp_sources::{Error, PlexConfig, Source, SourceConfig};
use serde_json::{Value, json};

const HOME_XML: &str = include_str!("fixtures/plex/home_users_v1.xml");
const HOME_JSON: &str = include_str!("fixtures/plex/home_users_v2.json");
const SWITCH_XML: &str = include_str!("fixtures/plex/switch.xml");
const SWITCH_JSON: &str = include_str!("fixtures/plex/switch.json");

fn credentials() -> PlexCredentials {
    PlexCredentials::manual("account-secret".into(), "installation-id".into())
}

fn account_asks(request: &Req) -> bool {
    request.header("X-Plex-Token") == Some("account-secret")
}

fn api(server: &TestServer) -> PlexAuthClient {
    PlexAuthClient::with_test_endpoint(&server.url("/api/v2")).unwrap()
}

fn people() -> Vec<PlexHomeUser> {
    vec![
        member(1, "Owner", [false, false, true]),
        member(2, "Guest", [true, false, false]),
        member(3, "Kid", [false, true, false]),
    ]
}

/// A Home member; `flags` are protected, restricted and admin. The first two
/// have a picture.
fn member(id: u64, name: &str, flags: [bool; 3]) -> PlexHomeUser {
    let picture = format!("https://example.invalid/avatar/{id}");
    PlexHomeUser {
        id,
        uuid: format!("uuid-{}", name.to_lowercase()),
        title: name.into(),
        thumb: if id < 3 { Some(picture) } else { None },
        protected: flags[0],
        restricted: flags[1],
        admin: flags[2],
    }
}

fn guest() -> PlexHomeUser {
    people().remove(1)
}

fn xml_reply(body: &str) -> Resp {
    resp(200, body, &[("Content-Type", "text/xml;charset=utf-8")])
}

fn json_reply(body: &str) -> Resp {
    resp(200, body, &[("Content-Type", "application/json")])
}

fn refused() -> Resp {
    resp(401, "unauthorized", &[])
}

fn nothing_secret_in_a_url(server: &TestServer) {
    for request in server.requests() {
        assert!(!request.url.contains("secret"), "{}", request.url);
        assert!(!request.url.contains("plextv-token"), "{}", request.url);
    }
}

#[test]
fn home_users_come_from_the_legacy_list_first() {
    let server = TestServer::start(|request| match request.path() {
        "/api/home/users" if account_asks(request) => xml_reply(HOME_XML),
        _ => refused(),
    });
    let users = api(&server).home_users(&credentials()).unwrap();
    assert_eq!(users, people());

    let requests = server.requests();
    assert_eq!(requests.len(), 1, "the newer list is not needed");
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].url, "/api/home/users");
    assert_eq!(
        requests[0].header("X-Plex-Client-Identifier"),
        Some("installation-id")
    );
    assert_eq!(requests[0].header("X-Plex-Product"), Some("Laterna"));
    assert!(requests[0].header("Accept").unwrap().contains("xml"));
    nothing_secret_in_a_url(&server);

    // Debug shows who can sign in and how, never who they are.
    let shown = format!("{users:?}");
    for private in ["Owner", "Guest", "Kid", "uuid-", "avatar", "example"] {
        assert!(!shown.contains(private), "{private} in {shown}");
    }
}

#[test]
fn and_from_the_newer_list_when_the_legacy_one_is_missing() {
    let server = TestServer::start(|request| match request.path() {
        "/api/v2/home/users" if account_asks(request) => json_reply(HOME_JSON),
        "/api/home/users" => resp(404, "missing", &[]),
        _ => refused(),
    });
    let users = api(&server).home_users(&credentials()).unwrap();
    assert_eq!(users, people(), "the same household, from the other shape");
    let paths: Vec<_> = server.requests().iter().map(|r| r.url.clone()).collect();
    assert_eq!(paths, ["/api/home/users", "/api/v2/home/users"]);
    nothing_secret_in_a_url(&server);
}

#[test]
fn a_reply_of_an_unknown_shape_is_not_an_empty_home() {
    for odd in [
        r#"<MediaContainer size="0"/>"#,
        "<html><body>hello</body></html>",
        r#"{"errors":[]}"#,
        "[]",
        "not either",
    ] {
        let server = TestServer::start(move |request| match request.path() {
            "/api/home/users" => xml_reply(odd),
            "/api/v2/home/users" => json_reply(HOME_JSON),
            _ => refused(),
        });
        let users = api(&server).home_users(&credentials()).unwrap();
        assert_eq!(users.len(), 3, "fell back from {odd:?}");
    }
}

#[test]
fn a_home_user_without_an_id_or_uuid_is_left_out() {
    let body = r#"<MediaContainer>
        <User id="1" uuid="uuid-owner" title="Owner" admin="1"/>
        <User uuid="uuid-nobody" title="No id"/>
        <User id="9" title="No uuid"/>
        <User id="x" uuid="uuid-bad" title="Bad id"/>
    </MediaContainer>"#;
    let server = TestServer::start(move |_| xml_reply(body));
    let users = api(&server).home_users(&credentials()).unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].id, 1);
    assert!(users[0].admin);
    assert!(!users[0].protected);
}

#[test]
fn a_refused_account_does_not_try_the_other_list() {
    let server = TestServer::start(|_| refused());
    let error = api(&server).home_users(&credentials()).unwrap_err();
    assert!(matches!(error, Error::Auth(_)), "{error:?}");
    assert_eq!(server.requests().len(), 1);
    assert!(!error.to_string().contains("secret"));
}

#[test]
fn when_neither_list_answers_the_first_error_is_kept() {
    let server = TestServer::start(|request| match request.path() {
        "/api/home/users" => resp(404, "missing", &[]),
        _ => resp(500, "broken", &[]),
    });
    let error = api(&server).home_users(&credentials()).unwrap_err();
    assert!(matches!(error, Error::NotFound(_)), "{error:?}");
    assert_eq!(server.requests().len(), 2);
}

#[test]
fn switching_sends_the_pin_only_to_plex_tv_and_returns_the_profiles_token() {
    let server = TestServer::start(|request| {
        if request.method != "POST" || !account_asks(request) {
            return refused();
        }
        match request.url.as_str() {
            "/api/home/users/2/switch?pin=1234" => xml_reply(SWITCH_XML),
            // A refusal that repeats what it was given, as a careless server might.
            "/api/home/users/2/switch?pin=0000" => resp(403, "bad pin 0000", &[]),
            _ => resp(404, "missing", &[]),
        }
    });
    let api = api(&server);
    let account = credentials();

    let wrong = api
        .switch_profile(&account, &guest(), Some("0000"))
        .unwrap_err();
    assert!(is_wrong_pin(&wrong), "{wrong:?}");
    for shown in [wrong.to_string(), format!("{wrong:?}")] {
        assert!(!shown.contains("0000"), "{shown}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    let profile = api
        .switch_profile(&account, &guest(), Some("1234"))
        .unwrap();
    assert_eq!(profile.id, 2);
    assert_eq!(profile.uuid, "uuid-guest");
    assert_eq!(profile.title, "Guest");
    assert!(profile.protected);
    assert!(!profile.restricted);
    assert_eq!(profile.token, "guest-plextv-token");
    // The profile hides its token and its name from logs.
    let shown = format!("{profile:?}");
    assert!(!shown.contains("plextv-token"), "{shown}");
    assert!(!shown.contains("Guest"), "{shown}");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.method, "POST");
        assert!(request.body.is_empty());
        assert_eq!(request.header("X-Plex-Token"), Some("account-secret"));
        assert_eq!(
            request.header("X-Plex-Client-Identifier"),
            Some("installation-id")
        );
    }
    assert_eq!(requests[1].url, "/api/home/users/2/switch?pin=1234");
    // The PIN is in no header, and the account's token is in no URL.
    for request in &requests {
        // (Host holds the test server's random port, which may hold digits.)
        for (name, value) in &request.headers {
            let is_host = name.eq_ignore_ascii_case("host");
            assert!(is_host || !value.contains("1234"));
        }
        assert!(!request.url.contains("secret"));
    }
}

#[test]
fn an_open_profile_switches_without_a_pin() {
    let server = TestServer::start(|request| match request.path() {
        "/api/home/users/3/switch" if account_asks(request) => json_reply(SWITCH_JSON),
        _ => refused(),
    });
    let kid = people().remove(2);
    let profile = api(&server)
        .switch_profile(&credentials(), &kid, None)
        .unwrap();
    assert_eq!(profile.id, 3);
    assert_eq!(profile.title, "Kid");
    assert!(profile.restricted);
    assert!(!profile.protected);
    assert_eq!(server.requests()[0].url, "/api/home/users/3/switch");
}

#[test]
fn the_switch_reply_may_name_its_token_in_any_known_way() {
    let bodies = [
        SWITCH_JSON,
        SWITCH_XML,
        r#"{"authenticationToken":"guest-plextv-token"}"#,
        r#"{"user":{"authToken":"guest-plextv-token"}}"#,
        r#"<user authenticationToken="guest-plextv-token"/>"#,
    ];
    for body in bodies {
        let server = TestServer::start(move |_| resp(200, body, &[]));
        let profile = api(&server)
            .switch_profile(&credentials(), &guest(), Some("1234"))
            .unwrap();
        assert_eq!(profile.token, "guest-plextv-token", "from {body}");
    }
    for body in [r#"{"id":2}"#, r#"<user id="2"/>"#, "", "no token here"] {
        let server = TestServer::start(move |_| resp(200, body, &[]));
        let error = api(&server)
            .switch_profile(&credentials(), &guest(), Some("1234"))
            .unwrap_err();
        assert!(matches!(error, Error::Parse { .. }), "{error:?}");
        assert!(!error.to_string().contains("1234"));
    }
}

#[test]
fn a_dead_account_token_is_signed_out_not_a_wrong_pin() {
    let server = TestServer::start(|_| refused());
    let error = api(&server)
        .switch_profile(&credentials(), &guest(), Some("1234"))
        .unwrap_err();
    assert!(matches!(error, Error::SignedOut(_)), "{error:?}");
    assert!(!is_wrong_pin(&error));
    for shown in [error.to_string(), format!("{error:?}")] {
        assert!(!shown.contains("1234"), "{shown}");
        assert!(!shown.contains("secret"), "{shown}");
    }
}

#[test]
fn a_refusal_of_an_open_profile_is_not_blamed_on_a_pin() {
    let server = TestServer::start(|_| resp(403, "no", &[]));
    let kid = people().remove(2);
    let error = api(&server)
        .switch_profile(&credentials(), &kid, None)
        .unwrap_err();
    assert!(matches!(error, Error::Auth(_)), "{error:?}");
    assert!(!is_wrong_pin(&error));
    // A protected profile asked without its PIN is refused as a PIN.
    let error = api(&server)
        .switch_profile(&credentials(), &guest(), None)
        .unwrap_err();
    assert!(is_wrong_pin(&error), "{error:?}");
}

#[test]
fn other_failures_are_plain_http_errors() {
    let server = TestServer::start(|_| resp(500, "broken", &[]));
    let error = api(&server)
        .switch_profile(&credentials(), &guest(), Some("1234"))
        .unwrap_err();
    assert!(matches!(error, Error::HttpStatus { status: 500, .. }));
    assert!(!is_wrong_pin(&error));
    assert!(!error.to_string().contains("1234"));
}

#[test]
fn a_malformed_pin_is_refused_before_any_request() {
    let server = TestServer::start(|_| xml_reply(SWITCH_XML));
    let api = api(&server);
    for bad in ["", "123", "12345", "12a4", " 123", "１２３４"] {
        let error = api
            .switch_profile(&credentials(), &guest(), Some(bad))
            .unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{bad:?}: {error:?}");
        assert!(!is_wrong_pin(&error));
    }
    assert!(server.requests().is_empty());
}

fn profile() -> PlexProfile {
    PlexProfile {
        id: 2,
        uuid: "uuid-guest".into(),
        title: "Guest".into(),
        protected: true,
        restricted: false,
        token: "guest-plextv-token".into(),
    }
}

fn machine_row(access_token: &str, uri: &str) -> Value {
    json!({"name":"Test Plex","provides":"server","owned":true,"clientIdentifier":"machine-1",
        "accessToken":access_token,
        "connections":[{"uri":uri,"local":true,"relay":false}]})
}

/// A server that takes `accepted` for everything but its identity.
fn server_taking(accepted: &'static str) -> TestServer {
    TestServer::start(move |request: &Req| {
        if request.path() == "/identity" {
            let found = json!({"MediaContainer":{"machineIdentifier":"machine-1"}});
            return json_reply(&found.to_string());
        }
        if request.header("X-Plex-Token") != Some(accepted) {
            return refused();
        }
        match request.path() {
            "/" | "/library/sections" => json_reply(r#"{"MediaContainer":{"Directory":[]}}"#),
            _ => resp(404, "missing", &[]),
        }
    })
}

/// plex.tv's list of servers: the owner's token sees one server token, the
/// switched profile's token another, and anything else is refused.
fn account_listing(server: &TestServer) -> TestServer {
    let for_owner = json!([machine_row("owner-server-token", &server.base)]);
    let for_guest = json!([machine_row("guest-server-token", &server.base)]);
    TestServer::start(move |request: &Req| {
        if request.path() != "/resources" {
            return resp(404, "missing", &[]);
        }
        match request.header("X-Plex-Token") {
            Some("account-secret") => json_reply(&for_owner.to_string()),
            Some("guest-plextv-token") => json_reply(&for_guest.to_string()),
            _ => refused(),
        }
    })
}

fn config(server: &TestServer, token: &str) -> PlexConfig {
    PlexConfig {
        id: "plex-guest".into(),
        name: "Test Plex".into(),
        url: server.base.clone(),
        machine_identifier: "machine-1".into(),
        access_token: token.into(),
        auth: Some(credentials()),
        profile: Some(Box::new(profile())),
        ..Default::default()
    }
}

#[test]
fn a_profiles_server_token_comes_from_its_own_resource_list() {
    let server = server_taking("guest-server-token");
    let account = account_listing(&server);
    let api = PlexAuthClient::with_test_endpoint(&account.base).unwrap();

    // The owner's list and the profile's name the same server, each with
    // its own token; the profile plays with its own, never with the
    // plex.tv token it switched with.
    let owned = api
        .resources_with("installation-id", "account-secret")
        .unwrap();
    let owner_token = owned[0].access_token.as_deref();
    assert_eq!(owner_token, Some("owner-server-token"));
    let listed = api
        .resources_with("installation-id", &profile().token)
        .unwrap();
    let access = listed
        .iter()
        .find(|r| r.machine_identifier == "machine-1")
        .and_then(|r| r.access_token.clone())
        .unwrap();
    assert_eq!(access, "guest-server-token");

    let source = PlexSource::new(&config(&server, &access)).unwrap();
    source.probe().unwrap();
    let on_the_server = server.requests();
    assert!(
        on_the_server
            .iter()
            .any(|r| r.path() == "/library/sections")
    );
    assert!(
        on_the_server
            .iter()
            .filter(|r| r.path() != "/identity")
            .all(|r| r.header("X-Plex-Token") == Some("guest-server-token"))
    );
    assert!(on_the_server.iter().all(|r| {
        r.header("X-Plex-Token") != Some("guest-plextv-token")
            && r.header("X-Plex-Token") != Some("account-secret")
    }));
    nothing_secret_in_a_url(&server);
    nothing_secret_in_a_url(&account);
}

#[test]
fn a_profile_source_renews_with_the_profiles_token() {
    let server = server_taking("guest-server-token");
    let account = account_listing(&server);
    let api = PlexAuthClient::with_test_endpoint(&account.base).unwrap();
    let source = PlexSource::new(&config(&server, "guest-server-stale"))
        .unwrap()
        .with_auth_api(api);

    source.probe().unwrap();
    let Some(SourceConfig::Plex(saved)) = source.updated_config() else {
        panic!("the replaced token must be saved");
    };
    assert_eq!(saved.access_token, "guest-server-token");
    assert_eq!(saved.id, "plex-guest");
    assert_eq!(saved.profile.as_deref(), Some(&profile()));
    let asked = account.requests();
    assert_eq!(asked.len(), 1);
    assert_eq!(
        asked[0].header("X-Plex-Token"),
        Some("guest-plextv-token"),
        "the profile asks with its own token, not the account's"
    );
    nothing_secret_in_a_url(&server);
    nothing_secret_in_a_url(&account);
}

#[test]
fn a_profile_is_saved_with_its_source_and_kept_out_of_logs() {
    let server = server_taking("guest-server-token");
    let saved = config(&server, "guest-server-token");
    let text = serde_json::to_string(&SourceConfig::Plex(saved.clone())).unwrap();
    let Ok(SourceConfig::Plex(restored)) = serde_json::from_str::<SourceConfig>(&text) else {
        panic!("a saved Plex source must load");
    };
    assert_eq!(restored, saved);
    assert_eq!(restored.profile.as_deref(), Some(&profile()));
    let shown = format!("{saved:?}");
    for private in ["plextv-token", "guest-server-token", "account-secret"] {
        assert!(!shown.contains(private), "{private} in {shown}");
    }
    assert!(!shown.contains("Guest"), "{shown}");
}
