//! Plex account linking and refresh against a local account API. No Plex
//! account, credentials or external network are needed.

mod common;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use common::{TestServer, resp};
use fp_sources::plex::auth::{LINK_URL, PlexAuthClient, PlexConnection, PlexCredentials};
use ring::rand::SystemRandom;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn assert_assertion(assertion: &str, jwk: &Value, client: &str) -> Value {
    let parts: Vec<_> = assertion.split('.').collect();
    assert_eq!(parts.len(), 3);
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(header["alg"], "EdDSA");
    assert_eq!(header["kid"], jwk["kid"]);
    assert_eq!(claims["aud"], "plex.tv");
    assert_eq!(claims["iss"], client);
    assert!(claims["exp"].as_u64().unwrap() > claims["iat"].as_u64().unwrap());
    let public = URL_SAFE_NO_PAD.decode(jwk["x"].as_str().unwrap()).unwrap();
    let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
    UnparsedPublicKey::new(&ED25519, public)
        .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
        .expect("the registered public key verifies the device assertion");
    claims
}

#[test]
fn code_link_uses_traditional_tokens_and_preserves_its_client_identity() {
    let polls = Arc::new(AtomicUsize::new(0));
    let count = polls.clone();
    let server = TestServer::start(move |request| {
        let body = match (request.method.as_str(), request.path()) {
            ("POST", "/api/v2/pins") => json!({"id":42,"code":"aB12","expiresIn":600}),
            ("GET", "/api/v2/pins/42") => {
                if count.fetch_add(1, Ordering::SeqCst) == 0 {
                    json!({"authToken":null})
                } else {
                    json!({"authToken":"linked-account-token"})
                }
            }
            _ => return resp(404, "missing", &[]),
        };
        resp(
            200,
            body.to_string(),
            &[("Content-Type", "application/json")],
        )
    });
    let api = PlexAuthClient::with_test_endpoint(&server.url("/api/v2")).unwrap();
    let pin = api.initiate("installation-id").unwrap();
    assert!(!pin.expired());
    assert_eq!(pin.id, 42);
    assert_eq!(
        pin.code, "aB12",
        "display the four-character code without changing its case"
    );
    let link = url::Url::parse(LINK_URL).unwrap();
    assert_eq!(link.scheme(), "https");
    assert_eq!(link.host_str(), Some("plex.tv"));
    assert_eq!(link.path(), "/link");
    assert!(link.query().is_none());
    assert!(link.fragment().is_none());
    assert!(!format!("{pin:?}").contains("aB12"));
    assert!(api.poll(&pin).unwrap().is_none());
    let credentials = api.poll(&pin).unwrap().unwrap();
    assert_eq!(credentials.access_token, "linked-account-token");
    assert!(credentials.device_key.is_none());
    assert!(credentials.expires_at.is_none());
    assert!(!credentials.needs_refresh());
    assert!(api.refresh(&credentials).is_err());

    let requests = server.requests();
    let registration: Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(
        registration["strong"], false,
        "request the short device code"
    );
    assert!(registration.get("jwk").is_none());
    let client = &credentials.client_identifier;
    assert_eq!(client, "installation-id", "used as given");
    assert!(
        requests
            .iter()
            .all(|r| r.header("x-plex-client-identifier") == Some(client.as_str()))
    );
    assert!(requests.iter().all(|r| {
        r.header("x-plex-product") == Some("Laterna")
            && r.header("accept") == Some("application/json")
            && r.header("x-plex-token").is_none()
    }));
    for request in requests.iter().filter(|r| r.path() == "/api/v2/pins/42") {
        assert_eq!(request.url, "/api/v2/pins/42");
    }
    assert_eq!(requests.len(), 3);
    let serialized = serde_json::to_value(&credentials).unwrap();
    assert!(serialized.get("device_key").is_none());
    assert!(serialized.get("expires_at").is_none());
    assert!(!format!("{credentials:?}").contains("linked-account-token"));
    let restored: PlexCredentials = serde_json::from_value(serialized).unwrap();
    assert_eq!(restored, credentials);
    // The caller decides whether links share an identity: another link
    // that chooses another identifier is sent under that one.
    api.initiate("second-link-7").unwrap();
    let requests = server.requests();
    assert_eq!(
        requests[3].header("x-plex-client-identifier"),
        Some("second-link-7")
    );
    assert_ne!(
        requests[0].header("x-plex-client-identifier"),
        requests[3].header("x-plex-client-identifier")
    );
}

#[test]
fn an_invalid_client_identifier_is_refused_before_any_request() {
    let server = TestServer::start(|_| {
        resp(
            200,
            json!({"id":42,"code":"aB12","expiresIn":600}).to_string(),
            &[("Content-Type", "application/json")],
        )
    });
    let api = PlexAuthClient::with_test_endpoint(&server.url("/api/v2")).unwrap();
    for bad in ["", "has space", "slash/inside", "accent\u{e9}"] {
        assert!(api.initiate(bad).is_err(), "refuse {bad:?}");
    }
    assert!(api.initiate(&"a".repeat(129)).is_err());
    assert!(server.requests().is_empty());
    let longest = "a".repeat(128);
    api.initiate(&longest).unwrap();
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header("x-plex-client-identifier"),
        Some(longest.as_str())
    );
}

#[test]
fn the_linked_account_is_named_without_keeping_its_email() {
    let server = TestServer::start(|request| {
        if request.path() != "/api/v2/user" {
            return resp(404, "missing", &[]);
        }
        if request.header("x-plex-token") != Some("linked-account-token") {
            return resp(401, "unauthorized", &[]);
        }
        let body = json!({
            "id": 5, "uuid": "made-up-account-uuid", "title": "Account Owner",
            "username": "owner-login", "email": "owner@example.test",
            "authToken": "leaked-user-token", "home": true, "homeAdmin": true,
        });
        resp(
            200,
            body.to_string(),
            &[("Content-Type", "application/json")],
        )
    });
    let api = PlexAuthClient::with_test_endpoint(&server.url("/api/v2")).unwrap();
    let mut credentials = PlexCredentials::manual("linked-account-token".into(), "dev-1".into());
    let account = api.account(&credentials).unwrap();
    assert_eq!(account.id, 5);
    assert_eq!(account.uuid, "made-up-account-uuid");
    assert_eq!(account.title, "Account Owner");
    assert!(account.home);
    credentials.account = Some(account.clone());

    let saved = serde_json::to_string(&credentials).unwrap();
    let restored: PlexCredentials = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored, credentials);
    for kept in [&saved, &serde_json::to_string(&account).unwrap()] {
        assert!(!kept.contains("owner@example.test"), "{kept}");
        assert!(!kept.contains("leaked-user-token"), "{kept}");
        assert!(!kept.contains("owner-login"), "{kept}");
    }
    let shown = format!("{credentials:?} {account:?}");
    for private in [
        "Account Owner",
        "made-up-account-uuid",
        "linked-account-token",
        "owner@example.test",
    ] {
        assert!(!shown.contains(private), "{private} in {shown}");
    }
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "/api/v2/user");
    assert_eq!(
        requests[0].header("x-plex-client-identifier"),
        Some("dev-1")
    );
}

#[test]
fn an_account_without_a_title_uses_its_sign_in_name_and_a_bad_reply_names_nothing() {
    let server = TestServer::start(|request| {
        if request.header("x-plex-token") == Some("dead-token") {
            return resp(401, "unauthorized", &[]);
        }
        let body = json!({"id": "6", "uuid": "another-uuid", "username": "second"});
        resp(
            200,
            body.to_string(),
            &[("Content-Type", "application/json")],
        )
    });
    let api = PlexAuthClient::with_test_endpoint(&server.url("/api/v2")).unwrap();
    let account = api
        .account(&PlexCredentials::manual("token".into(), "device-2".into()))
        .unwrap();
    assert_eq!((account.id, account.title.as_str()), (6, "second"));
    assert!(!account.home);
    let dead = PlexCredentials::manual("dead-token".into(), "device-2".into());
    let error = api.account(&dead).unwrap_err().to_string();
    assert!(!error.contains("dead-token"), "{error}");

    let nameless = TestServer::start(|_| {
        resp(
            200,
            json!({"id": 1}).to_string(),
            &[("Content-Type", "application/json")],
        )
    });
    let api = PlexAuthClient::with_test_endpoint(&nameless.url("/api/v2")).unwrap();
    assert!(api.account(&dead).is_err(), "no uuid, no account");
}

#[test]
fn saved_device_credentials_still_refresh_with_the_registered_signing_identity() {
    let server = TestServer::start(|request| {
        let body = match (request.method.as_str(), request.path()) {
            ("GET", "/api/v2/auth/nonce") => json!({"nonce":"fresh-nonce"}),
            ("POST", "/api/v2/auth/token") => json!({"auth_token":"refreshed-account-token"}),
            _ => return resp(404, "missing", &[]),
        };
        resp(
            200,
            body.to_string(),
            &[("Content-Type", "application/json")],
        )
    });
    // Model credentials saved by the earlier device-key linking flow.
    let private = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
    let pair = Ed25519KeyPair::from_pkcs8(private.as_ref()).unwrap();
    let jwk = json!({
        "kid":"saved-key", "x":URL_SAFE_NO_PAD.encode(pair.public_key().as_ref()),
    });
    let private = URL_SAFE_NO_PAD.encode(private.as_ref());
    let serialized = json!({
        "access_token":"saved-account-token", "client_identifier":"saved-client",
        "device_key":{"id":"saved-key", "pkcs8":private}, "expires_at":1,
    });
    let credentials: PlexCredentials = serde_json::from_value(serialized.clone()).unwrap();
    assert!(credentials.needs_refresh());
    let api = PlexAuthClient::with_test_endpoint(&server.url("/api/v2")).unwrap();
    let refreshed = api.refresh(&credentials).unwrap();
    assert_eq!(refreshed.access_token, "refreshed-account-token");
    assert_eq!(refreshed.client_identifier, credentials.client_identifier);
    assert_eq!(refreshed.device_key, credentials.device_key);
    assert!(!refreshed.needs_refresh());
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| {
        r.header("x-plex-client-identifier") == Some("saved-client")
            && r.header("x-plex-product") == Some("Laterna")
            && r.header("x-plex-token").is_none()
    }));
    let refresh = requests
        .iter()
        .find(|r| r.path() == "/api/v2/auth/token")
        .unwrap();
    let refresh_body: Value = serde_json::from_str(&refresh.body).unwrap();
    let claims = assert_assertion(
        refresh_body["jwt"].as_str().unwrap(),
        &jwk,
        &credentials.client_identifier,
    );
    assert_eq!(claims["nonce"], "fresh-nonce");
    assert_eq!(serde_json::to_value(&credentials).unwrap(), serialized);
    let debug = format!("{credentials:?} {refreshed:?}");
    assert!(!debug.contains(&private));
    assert!(!debug.contains("account-token"));
    let restored: PlexCredentials = serde_json::from_value(serialized).unwrap();
    assert_eq!(restored, credentials);
}

#[test]
fn resources_skip_players_and_keep_shared_server_credentials_and_connections() {
    let server = TestServer::start(|_| {
        resp(200, json!([
        {"name":"TV player","provides":"player","accessToken":null},
        {"name":"Shared server","provides":"server","clientIdentifier":"machine","owned":"0","accessToken":"shared-token","connections":[
            {"uri":"https://192-168-1-2.machine.plex.direct:32400","local":true,"relay":false},
            {"uri":"https://relay.plex.direct:443","local":"0","relay":"1"},
            {"uri":"file:///etc/passwd"}
        ]},
        {"name":"Server without token","provides":"server","clientIdentifier":"other","accessToken":null}
    ]).to_string(), &[("Content-Type","application/json")])
    });
    let credentials = PlexCredentials::manual("account-token".into(), "client".into());
    let api = PlexAuthClient::with_test_endpoint(&server.url("/api/v2")).unwrap();
    let resources = api.resources(&credentials).unwrap();
    assert_eq!(resources.len(), 2);
    let shared = &resources[0];
    assert_eq!(shared.machine_identifier, "machine");
    assert_eq!(shared.access_token.as_deref(), Some("shared-token"));
    assert!(!shared.owned);
    assert_eq!(shared.connections.len(), 2);
    assert!(shared.connections[0].local);
    assert!(shared.connections[1].relay);
    assert_eq!(shared.connections[0].protocol, "https");
    assert!(!format!("{shared:?}").contains("shared-token"));
    assert!(resources[1].access_token.is_none());
    let requests = server.requests();
    assert_eq!(requests[0].header("x-plex-token"), Some("account-token"));
    assert!(requests[0].url.contains("includeRelay=1"));
    assert!(requests[0].url.contains("includeIPv6=1"));
    assert!(
        !credentials.needs_refresh(),
        "manual credentials cannot be refreshed"
    );
    assert!(api.refresh(&credentials).is_err());
}

#[test]
fn discovery_retains_only_literal_addresses_and_valid_optional_ports() {
    let server = TestServer::start(|_| {
        resp(
            200,
            json!([{
                "name":"Server", "provides":"server", "clientIdentifier":"machine",
                "connections":[
                    {"uri":"https://server.plex.direct:32400", "address":"192.168.1.2", "port":32400},
                    {"uri":"https://server.plex.direct:32400", "address":"[2001:db8::2]", "port":"32400"},
                    {"uri":"https://server.plex.direct:32400", "address":"2001:db8::3", "port":65535},
                    {"uri":"https://server.plex.direct:32400"},
                    {"uri":"https://server.plex.direct:32400", "address":null, "port":null},
                    {"uri":"https://server.plex.direct:32400", "address":"some-host.example", "port":443},
                    {"uri":"https://server.plex.direct:32400", "address":"192.168.1.2:32400", "port":"invalid"},
                    {"uri":"https://server.plex.direct:32400", "address":42, "port":0},
                    {"uri":"https://server.plex.direct:32400", "address":{}, "port":-1},
                    {"uri":"https://server.plex.direct:32400", "address":"[::1]:32400", "port":65536},
                    {"uri":"https://server.plex.direct:32400", "address":"not an IP", "port":32400.5},
                    {"uri":"https://server.plex.direct:32400", "address":"127.0.0.1", "port":"18446744073709551616"},
                    {"uri":"file:///private", "address":"127.0.0.1", "port":32400},
                    {"uri":"https://user:password@server.plex.direct", "address":"127.0.0.1", "port":32400}
                ]
            }])
            .to_string(),
            &[("Content-Type", "application/json")],
        )
    });
    let api = PlexAuthClient::with_test_endpoint(&server.base).unwrap();
    let resources = api
        .resources(&PlexCredentials::manual(
            "account-token".into(),
            "client".into(),
        ))
        .unwrap();
    assert_eq!(
        resources.len(),
        1,
        "invalid optional fields do not discard the server"
    );
    let connections = &resources[0].connections;
    let expected = [
        (Some("192.168.1.2"), Some(32400)),
        (Some("2001:db8::2"), Some(32400)),
        (Some("2001:db8::3"), Some(65535)),
        (None, None),
        (None, None),
        (None, Some(443)),
        (None, None),
        (None, None),
        (None, None),
        (None, None),
        (None, None),
        (Some("127.0.0.1"), None),
    ];
    assert_eq!(
        connections.len(),
        expected.len(),
        "URI checks still reject unsafe connections"
    );
    for (connection, (address, port)) in connections.iter().zip(expected) {
        assert_eq!(
            connection.uri, "https://server.plex.direct:32400",
            "retain the original TLS/SNI hostname"
        );
        assert_eq!(connection.address, address.map(|a| a.parse().unwrap()));
        assert_eq!(connection.port, port);
    }
    let stored = serde_json::to_value(&connections[1]).unwrap();
    let restored: PlexConnection = serde_json::from_value(stored).unwrap();
    assert_eq!(restored, connections[1]);
    let old: PlexConnection =
        serde_json::from_value(json!({"uri":"https://server.plex.direct:32400"})).unwrap();
    assert!(old.address.is_none() && old.port.is_none());
    let stored = serde_json::to_value(old).unwrap();
    assert!(stored.get("address").is_none() && stored.get("port").is_none());
}

#[test]
fn expired_pin_is_never_polled_and_missing_expiry_is_rejected() {
    let server = TestServer::start(|request| {
        let body = if request.url.contains("missing") {
            json!({"id":42,"code":"ABCD"})
        } else {
            json!({"id":42,"code":"ABCD","expiresIn":0})
        };
        resp(
            200,
            body.to_string(),
            &[("Content-Type", "application/json")],
        )
    });
    let api = PlexAuthClient::with_test_endpoint(&server.base).unwrap();
    let pin = api.initiate("device").unwrap();
    assert!(pin.expired());
    assert!(api.poll(&pin).is_err());
    assert_eq!(server.requests().len(), 1);
    let api = PlexAuthClient::with_test_endpoint(&server.url("/missing")).unwrap();
    assert!(api.initiate("device").is_err());
    assert!(PlexAuthClient::with_test_endpoint("http://example.com").is_err());
    assert!(PlexAuthClient::with_test_endpoint("https://clients.plex.tv/api/v2").is_err());
}

#[test]
fn linking_rejects_long_or_invalid_codes_instead_of_displaying_an_unusable_code() {
    for code in ["long-browser-authorization-code", "12345", "A B1", "💯", ""] {
        let server = TestServer::start(move |_| {
            resp(
                200,
                json!({"id":42,"code":code,"expiresIn":600}).to_string(),
                &[("Content-Type", "application/json")],
            )
        });
        let api = PlexAuthClient::with_test_endpoint(&server.base).unwrap();
        assert!(api.initiate("device").is_err(), "reject {code:?}");
        assert_eq!(server.requests().len(), 1);
    }
}
