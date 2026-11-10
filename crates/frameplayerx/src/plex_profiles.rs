//! Which Plex Home profile each linked account plays as.
//!
//! One profile is active per account, chosen in the source's settings and
//! remembered (`Settings::plex_profiles`: account key to profile uuid, absent
//! for the account itself). Each (server, profile) is a source of its own with
//! the profile's token; the ones that are not active are hidden, so the
//! sources list, Home, History, search and the remotes show only what the
//! active profile may see. These are the rules, without any network.

use fp_sources::plex::auth::{PlexProfile, PlexResource, linked_source_id};
use fp_sources::{PlexConfig, SourceConfig};
use std::collections::BTreeMap;

/// Account key to the uuid of the profile it plays as.
pub type Chosen = BTreeMap<String, String>;

/// The profile uuid a source plays as; empty for the account itself.
fn plays_as(source: &PlexConfig) -> &str {
    source.profile.as_deref().map_or("", |p| p.uuid.as_str())
}

/// The profile uuid `account` plays as now, empty for the account itself. A
/// profile none of whose sources is left counts as the account.
pub fn active(configs: &[SourceConfig], chosen: &Chosen, account: &str) -> String {
    let wanted = chosen.get(account).map_or("", String::as_str);
    let known = wanted.is_empty()
        || configs.iter().any(|c| {
            matches!(c, SourceConfig::Plex(p)
                if p.account_key().as_deref() == Some(account) && plays_as(p) == wanted)
        });
    if known { wanted } else { "" }.to_string()
}

/// Whether any source plays as a profile.
pub fn any(configs: &[SourceConfig]) -> bool {
    configs
        .iter()
        .any(|c| matches!(c, SourceConfig::Plex(p) if p.profile.is_some()))
}

/// The ids of the Plex sources that are not their account's active profile.
/// Nothing is hidden until a profile has been set up.
pub fn hidden(configs: &[SourceConfig], chosen: &Chosen) -> Vec<String> {
    if !any(configs) {
        return Vec::new();
    }
    configs
        .iter()
        .filter_map(|c| match c {
            SourceConfig::Plex(p) => {
                let key = p.account_key()?;
                (plays_as(p) != active(configs, chosen, &key)).then(|| p.id.clone())
            }
            _ => None,
        })
        .collect()
}

/// For a source that is hidden, the active profile's source of the same
/// server. `None` when `id` is not hidden or the profile has no source of
/// that server.
pub fn counterpart(configs: &[SourceConfig], chosen: &Chosen, id: &str) -> Option<String> {
    let SourceConfig::Plex(shown) = configs.iter().find(|c| c.id() == id)? else {
        return None;
    };
    let key = shown.account_key()?;
    let active = active(configs, chosen, &key);
    if plays_as(shown) == active {
        return None;
    }
    configs.iter().find_map(|c| match c {
        SourceConfig::Plex(p)
            if p.account_key().as_deref() == Some(&key)
                && p.machine_identifier == shown.machine_identifier
                && plays_as(p) == active =>
        {
            Some(p.id.clone())
        }
        _ => None,
    })
}

/// The Plex sources that are the account itself (not a profile) and linked,
/// which a profile gets a copy of each.
pub fn own_sources(configs: &[SourceConfig], account: &str) -> Vec<PlexConfig> {
    configs
        .iter()
        .filter_map(|c| match c {
            SourceConfig::Plex(p)
                if p.profile.is_none()
                    && p.auth.is_some()
                    && p.account_key().as_deref() == Some(account) =>
            {
                Some(p.clone())
            }
            _ => None,
        })
        .collect()
}

/// The sources of every profile of `account`: what "forget" removes.
pub fn profile_sources(configs: &[SourceConfig], account: &str) -> Vec<String> {
    configs
        .iter()
        .filter_map(|c| match c {
            SourceConfig::Plex(p)
                if p.profile.is_some() && p.account_key().as_deref() == Some(account) =>
            {
                Some(p.id.clone())
            }
            _ => None,
        })
        .collect()
}

/// Whether `account` has a source for the profile `uuid` already.
pub fn is_set_up(configs: &[SourceConfig], account: &str, uuid: &str) -> bool {
    configs.iter().any(|c| {
        matches!(c, SourceConfig::Plex(p)
            if p.account_key().as_deref() == Some(account) && plays_as(p) == uuid)
    })
}

/// Why switching to a profile failed.
#[derive(Debug, PartialEq, Eq)]
pub enum SwitchError {
    /// plex.tv refused the PIN.
    WrongPin,
    /// plex.tv no longer accepts the account's sign-in.
    SignedOut,
    Other(String),
}

/// The sources a profile plays the account's servers as, and the names of
/// the servers that are not shared with it.
#[derive(Debug)]
pub struct Siblings {
    pub sources: Vec<PlexConfig>,
    pub missing: Vec<String>,
}

/// A source for each of the account's servers that `servers` (the list the
/// profile's own token got from plex.tv) holds. It has its own id, so what
/// the profile watched stays apart from the account's, the profile's token
/// for the server, and the name, connection choices and hidden libraries of
/// the account's source.
pub fn siblings(own: &[PlexConfig], profile: &PlexProfile, servers: &[PlexResource]) -> Siblings {
    let mut found = Siblings {
        sources: Vec::new(),
        missing: Vec::new(),
    };
    for base in own {
        let server = servers
            .iter()
            .find(|s| s.machine_identifier == base.machine_identifier);
        // The server's own token for the profile: plex.tv's token is refused
        // by the server, so a server that gave none is not shared.
        let (Some(server), Some(key)) = (server, base.account_key()) else {
            found.missing.push(base.name.clone());
            continue;
        };
        let Some(token) = server.access_token.clone() else {
            found.missing.push(base.name.clone());
            continue;
        };
        let mut source = base.clone();
        source.id = linked_source_id(&key, &base.machine_identifier, Some(&profile.uuid));
        source.access_token = token;
        source.connections = server.connections.clone();
        source.profile = Some(Box::new(profile.clone()));
        found.sources.push(source);
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_sources::plex::auth::{PlexAccount, PlexConnection, PlexCredentials};

    fn auth(account: &str) -> PlexCredentials {
        let mut credentials = PlexCredentials::manual("account-token".into(), "client".into());
        credentials.account = Some(Box::new(PlexAccount {
            id: 1,
            uuid: account.into(),
            title: "Owner".into(),
            home: true,
        }));
        credentials
    }

    fn own(account: &str, machine: &str) -> PlexConfig {
        PlexConfig {
            id: linked_source_id(account, machine, None),
            name: format!("Server {machine}"),
            url: "http://10.0.0.2:32400".into(),
            machine_identifier: machine.into(),
            access_token: "server-token".into(),
            auth: Some(auth(account)),
            hidden_sections: vec!["3".into()],
            ..Default::default()
        }
    }

    fn profile(uuid: &str) -> PlexProfile {
        PlexProfile {
            id: 2,
            uuid: uuid.into(),
            title: "Guest".into(),
            protected: true,
            restricted: false,
            token: format!("{uuid}-plextv"),
        }
    }

    fn resource(machine: &str, token: &str) -> PlexResource {
        PlexResource {
            name: "Server".into(),
            machine_identifier: machine.into(),
            access_token: Some(token.into()),
            connections: vec![PlexConnection {
                protocol: "http".into(),
                uri: "http://10.0.0.2:32400".into(),
                address: None,
                port: Some(32400),
                local: true,
                relay: false,
            }],
            owned: false,
            provides: "server".into(),
        }
    }

    /// Three profiles of one account, each with a source of two servers.
    fn household() -> Vec<SourceConfig> {
        let mut all = Vec::new();
        for machine in ["m1", "m2"] {
            let base = own("acct", machine);
            for uuid in ["guest", "kid"] {
                let set = siblings(
                    std::slice::from_ref(&base),
                    &profile(uuid),
                    &[resource(machine, &format!("{uuid}-server"))],
                );
                all.extend(set.sources.into_iter().map(SourceConfig::Plex));
            }
            all.push(SourceConfig::Plex(base));
        }
        all
    }

    fn ids(configs: &[SourceConfig], keep: impl Fn(&PlexConfig) -> bool) -> Vec<String> {
        configs
            .iter()
            .filter_map(|c| match c {
                SourceConfig::Plex(p) if keep(p) => Some(p.id.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn only_the_active_profiles_sources_are_shown() {
        let configs = household();
        assert_eq!(configs.len(), 6);
        let mut chosen = Chosen::new();
        // The account itself by default: both profiles' sources are hidden.
        let hidden_now = hidden(&configs, &chosen);
        let profiles = ids(&configs, |p| p.profile.is_some());
        assert_eq!(hidden_now.len(), 4);
        assert!(profiles.iter().all(|id| hidden_now.contains(id)));

        for uuid in ["guest", "kid"] {
            chosen.insert("acct".into(), uuid.into());
            let hidden_now = hidden(&configs, &chosen);
            let shown = ids(&configs, |p| !hidden_now.contains(&p.id));
            assert_eq!(shown.len(), 2, "one source per server for {uuid}");
            for id in shown {
                let Some(SourceConfig::Plex(p)) = configs.iter().find(|c| c.id() == id) else {
                    panic!("no source {id}");
                };
                assert_eq!(plays_as(p), uuid);
            }
        }
    }

    #[test]
    fn other_accounts_and_manual_sources_are_never_hidden() {
        let mut configs = household();
        configs.push(SourceConfig::Plex(own("another", "m1")));
        configs.push(SourceConfig::Plex(PlexConfig {
            id: "manual".into(),
            machine_identifier: "m1".into(),
            ..Default::default()
        }));
        let mut chosen = Chosen::new();
        chosen.insert("acct".into(), "guest".into());
        let hidden_now = hidden(&configs, &chosen);
        assert!(!hidden_now.contains(&linked_source_id("another", "m1", None)));
        assert!(!hidden_now.contains(&"manual".to_string()));
    }

    #[test]
    fn nothing_is_hidden_before_a_profile_is_set_up() {
        let configs = vec![SourceConfig::Plex(own("acct", "m1"))];
        let mut chosen = Chosen::new();
        chosen.insert("acct".into(), "guest".into());
        assert!(hidden(&configs, &chosen).is_empty());
        // A chosen profile that is gone leaves the account itself playing.
        assert_eq!(active(&configs, &chosen, "acct"), "");
    }

    #[test]
    fn a_hidden_source_leads_to_the_active_profiles_source_of_its_server() {
        let configs = household();
        let mut chosen = Chosen::new();
        chosen.insert("acct".into(), "kid".into());
        let account_m2 = linked_source_id("acct", "m2", None);
        let kid_m2 = linked_source_id("acct", "m2", Some("kid"));
        assert_eq!(
            counterpart(&configs, &chosen, &account_m2),
            Some(kid_m2.clone())
        );
        // A shown source leads nowhere.
        assert_eq!(counterpart(&configs, &chosen, &kid_m2), None);
        assert_eq!(counterpart(&configs, &chosen, "unknown"), None);
    }

    #[test]
    fn profile_sources_get_their_own_ids_and_server_tokens() {
        let base = own("acct", "m1");
        let set = siblings(
            std::slice::from_ref(&base),
            &profile("guest"),
            &[resource("m1", "guest-server-token")],
        );
        assert!(set.missing.is_empty());
        let [source] = set.sources.as_slice() else {
            panic!("expected one source");
        };
        assert_ne!(source.id, base.id);
        assert_eq!(source.id, linked_source_id("acct", "m1", Some("guest")));
        assert_eq!(source.access_token, "guest-server-token");
        assert_eq!(source.profile.as_ref().unwrap().token, "guest-plextv");
        // Name, libraries left out and the account's sign-in carry over.
        assert_eq!(source.name, base.name);
        assert_eq!(source.hidden_sections, base.hidden_sections);
        assert_eq!(source.auth, base.auth);
    }

    #[test]
    fn a_server_the_profile_cannot_reach_is_named_not_added() {
        let bases = [own("acct", "m1"), own("acct", "m2")];
        let set = siblings(&bases, &profile("kid"), &[resource("m2", "kid-server")]);
        assert_eq!(set.missing, vec!["Server m1".to_string()]);
        assert_eq!(set.sources.len(), 1);
        assert_eq!(set.sources[0].machine_identifier, "m2");
    }

    #[test]
    fn a_profile_can_be_forgotten_and_found() {
        let configs = household();
        assert_eq!(profile_sources(&configs, "acct").len(), 4);
        assert!(is_set_up(&configs, "acct", "kid"));
        assert!(!is_set_up(&configs, "acct", "other"));
        assert_eq!(own_sources(&configs, "acct").len(), 2);
    }
}
