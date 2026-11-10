//! Plex linking and connection checks run on jobs, tied to the open form.

use super::tasks::PlexListing;
use super::*;
use fp_sources::plex::PlexSource;
use fp_sources::plex::auth::{PlexAccount, PlexAuthClient, PlexCredentials, PlexPin, PlexResource};
use fp_sources::{PlexConfig, SourceConfig};
use std::collections::HashSet;
use ui::plex::{Libraries, LinkState, ListError, ServerRow};

/// A server of the Plex list, tested: its machine identifier and the source
/// to add, or why not.
type Checked = (String, Result<PlexConfig, String>);

/// The client identifier for a link. The headset has one of its own,
/// `{device}-plex`, so it is a single entry in the account's device list
/// however many servers and times it links. Linking again from a source
/// that was linked under an identifier of its own keeps that identifier, so
/// it stays the same device. Going back to an older build is safe: each
/// source signs in with the identifier its own credentials hold, so sources
/// linked meanwhile simply keep sharing this one.
fn link_client_identifier(device: &str, editing: Option<&SourceConfig>) -> String {
    if let Some(SourceConfig::Plex(old)) = editing
        && let Some(auth) = &old.auth
    {
        return auth.client_identifier.clone();
    }
    format!("{device}-plex")
}

/// One poll of a pending link. Once the code is claimed it also names the
/// account (plex.tv `/user`) and lists its servers. Naming the account is
/// a convenience: linking goes on without it.
fn poll_link(
    api: &PlexAuthClient,
    pin: &PlexPin,
) -> fp_sources::Result<Option<(PlexCredentials, Vec<PlexResource>)>> {
    let Some(mut credentials) = api.poll(pin)? else {
        return Ok(None);
    };
    match api.account(&credentials) {
        Ok(account) => credentials.account = Some(Box::new(account)),
        Err(e) => log::info!("cannot name the linked Plex account: {e}"),
    }
    let servers = api.resources(&credentials)?;
    Ok(Some((credentials, servers)))
}

/// Whether two sign-ins are for the same account. With both accounts known
/// their ids decide; a source linked before its account was recorded is
/// matched by the pairing's client identifier.
fn same_login(old: &PlexCredentials, new: &PlexCredentials) -> bool {
    match (old.account.as_deref(), new.account.as_deref()) {
        (Some(a), Some(b)) => a.uuid == b.uuid,
        (None, _) => old.client_identifier == new.client_identifier,
        (Some(_), None) => false,
    }
}

fn is_same_account(config: &PlexConfig, credentials: &PlexCredentials) -> bool {
    match &config.auth {
        Some(auth) => same_login(auth, credentials),
        None => false,
    }
}

/// Gives `config` the new sign-in. A source that is the account itself also
/// takes the server's fresh token and connections; a profile keeps its own
/// token.
fn sign_in_again(config: &mut PlexConfig, login: &PlexCredentials, servers: &[PlexResource]) {
    config.auth = Some(login.clone());
    if config.profile.is_some() {
        return;
    }
    let machine = config.machine_identifier.as_str();
    let server = servers.iter().find(|s| s.machine_identifier == machine);
    if let Some(server) = server {
        let token = server.access_token.as_ref();
        config.access_token = token.unwrap_or(&login.access_token).clone();
        config.connections = server.connections.clone();
    }
}

/// Signs every source of the account in again with one new sign-in, so none
/// is left on a token plex.tv has replaced. Their ids stay, and with them
/// what was watched. Returns the sources changed.
fn relink_fan_out(
    configs: &mut [SourceConfig],
    credentials: &PlexCredentials,
    servers: &[PlexResource],
) -> HashSet<String> {
    let mut changed = HashSet::new();
    for config in configs {
        if let SourceConfig::Plex(plex) = config
            && is_same_account(plex, credentials)
        {
            sign_in_again(plex, credentials, servers);
            changed.insert(plex.id.clone());
        }
    }
    changed
}

/// One sign-in for each pairing whose sources do not name their account yet.
fn accounts_to_name(configs: &[SourceConfig]) -> Vec<PlexCredentials> {
    let mut pending: Vec<PlexCredentials> = Vec::new();
    for config in configs {
        if let SourceConfig::Plex(plex) = config
            && let Some(auth) = &plex.auth
            && auth.account.is_none()
            && !pending
                .iter()
                .any(|p| p.client_identifier == auth.client_identifier)
        {
            pending.push(auth.clone());
        }
    }
    pending
}

/// Records the accounts found for pairings (by client identifier) on their
/// sources. Returns the sources changed.
fn name_accounts(
    configs: &mut [SourceConfig],
    accounts: &[(String, PlexAccount)],
) -> HashSet<String> {
    let mut changed = HashSet::new();
    for config in configs {
        if let SourceConfig::Plex(plex) = config
            && let Some(auth) = &mut plex.auth
            && auth.account.is_none()
            && let Some((_, account)) = accounts
                .iter()
                .find(|(client, _)| *client == auth.client_identifier)
        {
            auth.account = Some(Box::new(account.clone()));
            changed.insert(plex.id.clone());
        }
    }
    changed
}

/// Whether plex.tv refused the account's sign-in.
fn account_refused(error: &fp_sources::Error) -> bool {
    use fp_sources::Error;
    match error {
        Error::Auth(_) | Error::SignedOut(_) => true,
        Error::HttpStatus { status, .. } => matches!(status, 401 | 498),
        _ => false,
    }
}

/// The servers of an account, for the list that manages them. An account
/// that its sources were linked to before accounts were recorded is named
/// on the way, so they are found by it.
fn list_servers(api: &PlexAuthClient, mut credentials: PlexCredentials) -> PlexListing {
    if credentials.account.is_none() {
        match api.account(&credentials) {
            Ok(account) => credentials.account = Some(Box::new(account)),
            Err(e) if account_refused(&e) => return Err(ListError::SignedOut),
            Err(e) => log::info!("cannot name the linked Plex account: {e}"),
        }
    }
    match api.resources(&credentials) {
        Ok(servers) => Ok((credentials, servers)),
        Err(e) if account_refused(&e) => Err(ListError::SignedOut),
        Err(e) => Err(ListError::Other(ui::errors::source(&e))),
    }
}

/// Tests one server and returns the source to add.
fn check_server(config: PlexConfig) -> Result<PlexConfig, String> {
    PlexSource::new(&config)
        .and_then(|source| source.probe())
        .map_err(|e| ui::errors::source(&e))
}

/// Tests the servers together, so one that cannot be reached does not hold
/// up the others.
fn check_servers(configs: Vec<PlexConfig>) -> Vec<Checked> {
    std::thread::scope(|scope| {
        let mut workers = Vec::new();
        for config in configs {
            let machine = config.machine_identifier.clone();
            workers.push((machine, scope.spawn(move || check_server(config))));
        }
        let mut checks = Vec::new();
        for (machine, worker) in workers {
            let result = worker
                .join()
                .unwrap_or_else(|_| Err(tr!("plex-server-failed").to_string()));
            checks.push((machine, result));
        }
        checks
    })
}

impl App {
    pub(super) fn plex_link_start(&mut self) {
        let generation = self.ui.next_seq();
        let Some(f) = self
            .ui
            .source_form
            .as_mut()
            .filter(|f| f.kind == fp_sources::SourceKind::Plex && !f.plex.manual)
        else {
            return;
        };
        f.plex.generation = generation;
        f.plex.other_account = false;
        f.plex.state = LinkState::Requesting;
        // A linked source being edited stays: signing in again is for it.
        // One entered with a token is another identity, so its servers are
        // added as new sources and it is kept as it is.
        if matches!(&f.editing, Some(SourceConfig::Plex(old)) if old.auth.is_none()) {
            f.editing = None;
        }
        let device = self.services.device_identifier();
        let client = link_client_identifier(device, f.editing.as_ref());
        self.jobs.spawn("plex-link", move || {
            let result = PlexAuthClient::new().initiate(&client);
            Job::PlexInitiated(generation, result.map_err(|e| ui::errors::source(&e)))
        });
    }

    pub(super) fn plex_link_poll(&mut self, generation: u64, pin: PlexPin) {
        let current = self.ui.source_form.as_ref().is_some_and(|f| f.kind == fp_sources::SourceKind::Plex
            && !f.plex.manual
            && f.plex.generation == generation
            && matches!(&f.plex.state, LinkState::Waiting { pin: current, .. } if current.id == pin.id));
        if !current {
            return;
        }
        self.jobs.spawn("plex-poll", move || {
            let api = PlexAuthClient::new();
            let result = poll_link(&api, &pin).map_err(|e| ui::errors::source(&e));
            Job::PlexPolled(generation, Box::new(result))
        });
    }

    pub(super) fn plex_connect(&mut self, config: PlexConfig) {
        let generation = self.ui.next_seq();
        let Some(f) = self
            .ui
            .source_form
            .as_mut()
            .filter(|f| f.kind == fp_sources::SourceKind::Plex)
        else {
            return;
        };
        f.plex.generation = generation;
        f.plex.state = LinkState::Connecting;
        self.jobs.spawn("plex-connect", move || {
            Job::PlexConnected(
                generation,
                Box::new(
                    PlexSource::new(&config)
                        .and_then(|s| s.probe())
                        .map_err(|e| ui::errors::source(&e)),
                ),
            )
        });
    }

    fn plex_form(&mut self, generation: u64) -> Option<&mut ui::SourceForm> {
        self.ui
            .source_form
            .as_mut()
            .filter(|f| f.kind == fp_sources::SourceKind::Plex && f.plex.generation == generation)
    }

    pub(super) fn plex_initiated(&mut self, generation: u64, result: Result<PlexPin, String>) {
        let Some(f) = self.plex_form(generation) else {
            return;
        };
        if f.plex.manual || !matches!(f.plex.state, LinkState::Requesting) {
            return;
        }
        f.plex.state = match result {
            Ok(pin) => LinkState::Waiting {
                pin,
                polled: Instant::now() - Duration::from_secs(2),
                polling: false,
            },
            Err(e) => LinkState::Failed(e),
        };
        self.panels[MAIN].request_repaint();
    }

    pub(super) fn plex_polled(
        &mut self,
        generation: u64,
        result: Result<Option<(PlexCredentials, Vec<PlexResource>)>, String>,
    ) {
        let Some(f) = self.plex_form(generation) else {
            return;
        };
        let LinkState::Waiting { pin, .. } = &f.plex.state else {
            return;
        };
        if f.plex.manual {
            return;
        }
        // A code claimed just before it ran out still counts: only an
        // answer that it is not claimed yet ends the wait.
        let expired = pin.expired();
        match result {
            Ok(Some((credentials, servers))) => {
                self.plex_linked(generation, credentials, servers);
                return;
            }
            Ok(None) if expired => {
                f.plex.state = LinkState::Failed(tr!("plex-link-expired").into());
            }
            Ok(None) => {
                if let LinkState::Waiting { polling, .. } = &mut f.plex.state {
                    *polling = false;
                }
            }
            Err(e) => f.plex.state = LinkState::Failed(e),
        }
        self.panels[MAIN].request_repaint();
    }

    /// The code was claimed: the account's other sources are signed in again
    /// with the new sign-in, and its servers are listed to choose from.
    fn plex_linked(
        &mut self,
        generation: u64,
        credentials: PlexCredentials,
        servers: Vec<PlexResource>,
    ) {
        let Some(f) = self.plex_form(generation) else {
            return;
        };
        // Signing in again as another account is not for the source being
        // edited: its servers are added as new sources.
        if let Some(SourceConfig::Plex(old)) = &f.editing
            && old.auth.is_some()
            && !is_same_account(old, &credentials)
        {
            f.editing = None;
            f.plex.other_account = true;
        }
        let known = |c: &SourceConfig| match c {
            SourceConfig::Plex(plex) => is_same_account(plex, &credentials),
            _ => false,
        };
        if self.services.source_configs.iter().any(known) {
            let change = self
                .services
                .replace_sources(|list| relink_fan_out(list, &credentials, &servers));
            if let Some(error) = change.errors.first() {
                self.ui.toast(error.to_string());
            } else if let Some(error) = change.save_error {
                let text = tr!("source-changed-unsaved", name = "Plex", error = error);
                self.ui.toast(text);
            } else {
                let title = credentials.account.as_ref().map(|a| a.title.as_str());
                let account = title.filter(|t| !t.is_empty()).unwrap_or("Plex");
                let text = tr!("plex-signed-in-again", account = account);
                self.ui.toast(text);
            }
            self.ui.invalidate();
        }
        let configs = self.services.source_configs.clone();
        if let Some(f) = self.plex_form(generation) {
            f.plex.state = LinkState::servers(credentials, servers, &configs);
        }
        self.panels[MAIN].request_repaint();
    }

    pub(super) fn plex_connected(&mut self, generation: u64, result: Result<PlexConfig, String>) {
        let Some(f) = self.plex_form(generation) else {
            return;
        };
        if !matches!(f.plex.state, LinkState::Connecting) {
            return;
        }
        match result {
            Ok(cfg) => {
                f.address = cfg.url.clone();
                if f.name.trim().is_empty() {
                    f.name = cfg.name.clone();
                }
                f.plex.selected = Some(cfg);
                f.plex.state = LinkState::Connected;
            }
            Err(e) => f.plex.state = LinkState::Failed(e),
        }
        self.panels[MAIN].request_repaint();
    }

    /// Tests the servers switched on in the list, all at once.
    pub(super) fn plex_add_servers(&mut self, generation: u64, configs: Vec<PlexConfig>) {
        if self.plex_form(generation).is_none() {
            return;
        }
        self.jobs.spawn("plex-add", move || {
            Job::PlexServersChecked(generation, check_servers(configs))
        });
    }

    /// The servers were tested: those that answered become sources in one
    /// save; the others say why and can be tried again. The form closes
    /// when nothing is left to add.
    pub(super) fn plex_servers_checked(&mut self, generation: u64, checks: Vec<Checked>) {
        let Some(f) = self.plex_form(generation) else {
            return;
        };
        let managing = f.plex.managing;
        let LinkState::Servers { rows, .. } = &mut f.plex.state else {
            return;
        };
        let mut working = Vec::new();
        for (machine, check) in checks {
            match check {
                Ok(config) => working.push((machine, config)),
                Err(reason) => {
                    rows.insert(machine, ServerRow::Failed(reason));
                }
            }
        }
        let sources = working
            .iter()
            .map(|(_, config)| SourceConfig::Plex(config.clone()))
            .collect();
        let change = self.add_sources(sources);
        let mut added = Vec::new();
        let configs = self.services.source_configs.clone();
        let mut closing = false;
        if let Some(f) = self.plex_form(generation)
            && let LinkState::Servers { rows, .. } = &mut f.plex.state
        {
            for (machine, config) in &working {
                let failed = change.errors.iter().find(|e| e.id == config.id);
                let row = match failed {
                    Some(e) => ServerRow::Failed(ui::errors::source(&e.error)),
                    None => {
                        added.push((config.id.clone(), config.name.clone()));
                        ServerRow::Added(config.name.clone())
                    }
                };
                rows.insert(machine.clone(), row);
            }
            // Managing stays on the list, whatever was added.
            closing = !managing && ui::plex::pending(&f.plex.state, &configs) == 0;
        }
        if closing {
            self.ui.source_form = None;
        }
        if let [(id, name)] = added.as_slice()
            && closing
            && change.save_error.is_none()
        {
            self.ui.toast(tr!("source-added", name = name.as_str()));
            self.open_source(id.clone(), Screen::Sources);
        } else if !added.is_empty() {
            let names: Vec<&str> = added.iter().map(|(_, name)| name.as_str()).collect();
            let names = names.join(", ");
            self.ui.toast(match change.save_error {
                Some(error) => tr!("source-added-unsaved", name = names, error = error),
                None => tr!("plex-servers-added", names = names),
            });
        }
        self.panels[MAIN].request_repaint();
    }

    /// Lists the servers of the account of the linked source being edited,
    /// to add and to remove.
    pub(super) fn plex_list_servers(&mut self, credentials: PlexCredentials) {
        let generation = self.ui.next_seq();
        let Some(f) = self
            .ui
            .source_form
            .as_mut()
            .filter(|f| f.kind == fp_sources::SourceKind::Plex && !f.plex.manual)
        else {
            return;
        };
        f.plex.generation = generation;
        f.plex.managing = true;
        f.plex.manage_error = None;
        f.plex.removing = None;
        f.plex.state = LinkState::Connecting;
        self.jobs.spawn("plex-servers", move || {
            let result = list_servers(&PlexAuthClient::new(), credentials);
            Job::PlexServersListed(generation, Box::new(result))
        });
    }

    /// The account's servers were listed: the list replaces the source's
    /// form, or the source stays as it is and says why not. An account
    /// plex.tv no longer accepts is signed in again in place.
    pub(super) fn plex_servers_listed(&mut self, generation: u64, result: PlexListing) {
        let Some(f) = self.plex_form(generation) else {
            return;
        };
        if !f.plex.managing || !matches!(f.plex.state, LinkState::Connecting) {
            return;
        }
        match result {
            Ok((credentials, servers)) => {
                // Sources linked before accounts were recorded are named
                // now, so none of them is offered to be added a second time.
                let unnamed = accounts_to_name(&self.services.source_configs);
                if let Some(account) = credentials.account.as_deref()
                    && unnamed
                        .iter()
                        .any(|p| p.client_identifier == credentials.client_identifier)
                {
                    let named = vec![(credentials.client_identifier.clone(), account.clone())];
                    self.plex_accounts_named(named);
                }
                let configs = self.services.source_configs.clone();
                if let Some(f) = self.plex_form(generation) {
                    ui::plex::refresh_edited(f, &configs);
                    f.plex.state = LinkState::managing(credentials, servers, &configs);
                }
            }
            Err(ListError::SignedOut) => {
                f.plex.managing = false;
                f.plex.state = LinkState::Failed(tr!("plex-signed-out").into());
            }
            Err(ListError::Other(error)) => {
                f.plex.managing = false;
                f.plex.manage_error = Some(error);
                f.plex.state = LinkState::Connected;
            }
        }
        self.panels[MAIN].request_repaint();
    }

    /// Removes the sources of a server switched off in the list, together.
    pub(super) fn plex_remove_servers(&mut self, ids: Vec<String>) {
        let mut names: Vec<String> = Vec::new();
        for id in &ids {
            let name = self.source_name(id);
            if !names.contains(&name) {
                names.push(name);
            }
        }
        if self.remove_sources(&ids) {
            self.ui
                .toast(tr!("plex-servers-removed", names = names.join(", ")));
        }
    }

    /// Lists the libraries of a source, to choose those it leaves out.
    pub(super) fn catalog_libraries(&mut self, source: String) {
        let found = self.services.opener.source(&source);
        self.jobs.spawn("catalog-libraries", move || {
            let result: Result<Vec<(String, String)>, String> = found
                .as_ref()
                .and_then(|src| src.as_catalog())
                .ok_or_else(|| tr!("source-not-available").to_string())
                .and_then(|catalog| catalog.all_libraries().map_err(|e| ui::errors::source(&e)))
                .map(|libraries| {
                    libraries
                        .into_iter()
                        .map(|library| {
                            let key = library.id.strip_prefix("section:").unwrap_or(&library.id);
                            (key.to_string(), library.name)
                        })
                        .collect()
                });
            Job::CatalogLibraries { source, result }
        });
    }

    pub(super) fn libraries_listed(
        &mut self,
        source: &str,
        result: Result<Vec<(String, String)>, String>,
    ) {
        let Some(f) = self
            .ui
            .source_form
            .as_mut()
            .filter(|f| f.editing.as_ref().is_some_and(|c| c.id() == source))
        else {
            return;
        };
        f.plex.libraries = match result {
            Ok(libraries) => Libraries::Listed(libraries),
            Err(error) => Libraries::Failed(error),
        };
        self.panels[MAIN].request_repaint();
    }

    /// Names the account of sources linked before accounts were recorded,
    /// so they are grouped with the rest of their account. Nothing happens
    /// offline: it is tried again at the next start.
    pub(super) fn plex_name_accounts(&mut self) {
        let pending = accounts_to_name(&self.services.source_configs);
        if pending.is_empty() {
            return;
        }
        self.jobs.spawn("plex-accounts", move || {
            let api = PlexAuthClient::new();
            let mut named = Vec::new();
            for auth in &pending {
                match api.account(auth) {
                    Ok(account) => named.push((auth.client_identifier.clone(), account)),
                    Err(e) => log::info!("cannot name a linked Plex account: {e}"),
                }
            }
            Job::PlexAccounts(named)
        });
    }

    pub(super) fn plex_accounts_named(&mut self, accounts: Vec<(String, PlexAccount)>) {
        if accounts.is_empty() {
            return;
        }
        let change = self
            .services
            .replace_sources(|list| name_accounts(list, &accounts));
        if let Some(error) = change.errors.first() {
            log::warn!("source unavailable: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_sources::plex::auth::{PlexConnection, PlexProfile};

    fn account(uuid: &str) -> PlexAccount {
        PlexAccount {
            id: 1,
            uuid: uuid.into(),
            title: "Owner".into(),
            home: false,
        }
    }

    fn sign_in(token: &str, client: &str, uuid: Option<&str>) -> PlexCredentials {
        let mut credentials = PlexCredentials::manual(token.into(), client.into());
        credentials.account = uuid.map(|uuid| Box::new(account(uuid)));
        credentials
    }

    fn linked(id: &str, machine: &str, auth: PlexCredentials) -> PlexConfig {
        PlexConfig {
            id: id.into(),
            name: id.into(),
            machine_identifier: machine.into(),
            access_token: format!("{id}-token"),
            auth: Some(auth),
            ..Default::default()
        }
    }

    fn server(machine: &str, token: Option<&str>) -> PlexResource {
        PlexResource {
            name: machine.into(),
            machine_identifier: machine.into(),
            access_token: token.map(str::to_string),
            connections: vec![PlexConnection {
                uri: format!("https://{machine}.plex.direct:32400"),
                address: None,
                port: Some(32400),
                local: true,
                relay: false,
                protocol: "https".into(),
            }],
            owned: true,
            provides: "server".into(),
        }
    }

    #[test]
    fn the_headset_links_under_one_identifier_of_its_own() {
        assert_eq!(
            link_client_identifier("frameplayerx-abc", None),
            "frameplayerx-abc-plex"
        );
        let id = link_client_identifier("frameplayerx-abc", None);
        assert!(id.len() <= 128);
        assert!(id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
        // A source linked under an identifier of its own keeps it.
        let old = linked("old", "m1", sign_in("t", "legacy-link-1", None));
        let editing = SourceConfig::Plex(old);
        assert_eq!(
            link_client_identifier("frameplayerx-abc", Some(&editing)),
            "legacy-link-1"
        );
        // A source entered with a token has none to keep.
        let manual = SourceConfig::Plex(PlexConfig::default());
        assert_eq!(
            link_client_identifier("frameplayerx-abc", Some(&manual)),
            "frameplayerx-abc-plex"
        );
    }

    #[test]
    fn relinking_an_account_updates_all_its_sources_and_keeps_their_ids() {
        let old = sign_in("old-token", "client-1", Some("account-1"));
        let profile = PlexProfile {
            id: 7,
            uuid: "profile-1".into(),
            title: "Kid".into(),
            protected: false,
            restricted: true,
            token: "profile-plex-tv-token".into(),
        };
        let mut for_profile = linked("kid", "m1", old.clone());
        for_profile.profile = Some(Box::new(profile));
        let mut configs = vec![
            SourceConfig::Plex(linked("home", "m1", old.clone())),
            SourceConfig::Plex(linked("tv", "m2", old.clone())),
            SourceConfig::Plex(for_profile),
        ];
        let fresh = sign_in("fresh-token", "client-1", Some("account-1"));
        let servers = [server("m1", Some("m1-server-token")), server("m2", None)];
        let changed = relink_fan_out(&mut configs, &fresh, &servers);
        assert_eq!(changed.len(), 3);
        let plex: Vec<&PlexConfig> = configs
            .iter()
            .map(|c| match c {
                SourceConfig::Plex(p) => p,
                _ => unreachable!(),
            })
            .collect();
        let ids: Vec<&str> = plex.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["home", "tv", "kid"]);
        for p in &plex {
            assert_eq!(p.auth.as_ref(), Some(&fresh));
        }
        assert_eq!(plex[0].access_token, "m1-server-token");
        assert_eq!(plex[0].connections.len(), 1);
        // A server without a token of its own takes the account's.
        assert_eq!(plex[1].access_token, "fresh-token");
        // A profile keeps the token it plays with.
        assert_eq!(plex[2].access_token, "kid-token");
        assert!(plex[2].profile.is_some());
    }

    #[test]
    fn another_accounts_link_changes_nothing_of_this_one() {
        let mine = sign_in("my-token", "client-1", Some("account-1"));
        let before = vec![
            SourceConfig::Plex(linked("home", "m1", mine)),
            SourceConfig::Plex(PlexConfig::default()),
        ];
        let mut configs = before.clone();
        let other = sign_in("other-token", "client-1", Some("account-2"));
        let servers = [server("m1", Some("other-server-token"))];
        assert!(relink_fan_out(&mut configs, &other, &servers).is_empty());
        assert_eq!(configs, before);
        // Neither does a sign-in plex.tv would not name, when this one is.
        let unnamed = sign_in("other-token", "client-1", None);
        assert!(relink_fan_out(&mut configs, &unnamed, &servers).is_empty());
        assert_eq!(configs, before);
    }

    #[test]
    fn only_the_accounts_refusal_signs_it_out() {
        use fp_sources::Error;
        let status = |status| Error::HttpStatus {
            status,
            url: "plex.tv".into(),
        };
        assert!(account_refused(&Error::Auth("plex.tv".into())));
        assert!(account_refused(&status(401)));
        assert!(account_refused(&status(498)));
        // A plex.tv that is down, or an answer nobody can read, is not a sign-out.
        assert!(!account_refused(&status(503)));
        assert!(!account_refused(&Error::NotFound("plex.tv".into())));
    }

    #[test]
    fn a_source_linked_before_accounts_were_recorded_is_found_by_its_pairing() {
        let old = linked("home", "m1", sign_in("old-token", "client-1", None));
        let named = sign_in("fresh-token", "client-1", Some("account-1"));
        let elsewhere = sign_in("fresh-token", "client-2", Some("account-1"));
        assert!(is_same_account(&old, &named));
        assert!(!is_same_account(&old, &elsewhere));
        assert!(!is_same_account(&PlexConfig::default(), &named));
    }

    #[test]
    fn older_sources_get_their_account_named_once_per_pairing() {
        let one = sign_in("t1", "client-1", None);
        let two = sign_in("t2", "client-2", None);
        let known = sign_in("t3", "client-3", Some("account-3"));
        let mut configs = vec![
            SourceConfig::Plex(linked("a", "m1", one.clone())),
            SourceConfig::Plex(linked("b", "m2", one)),
            SourceConfig::Plex(linked("c", "m3", two)),
            SourceConfig::Plex(linked("d", "m4", known)),
            SourceConfig::Plex(PlexConfig::default()),
        ];
        let pending = accounts_to_name(&configs);
        let clients: Vec<_> = pending.iter().map(|p| &p.client_identifier).collect();
        assert_eq!(clients, ["client-1", "client-2"]);
        let found = [("client-1".to_string(), account("account-1"))];
        let changed = name_accounts(&mut configs, &found);
        assert_eq!(changed, HashSet::from(["a".to_string(), "b".to_string()]));
        let keys: Vec<Option<String>> = configs
            .iter()
            .map(|c| match c {
                SourceConfig::Plex(p) => p.account_key(),
                _ => None,
            })
            .collect();
        assert_eq!(keys[0].as_deref(), Some("account-1"));
        assert_eq!(keys[1].as_deref(), Some("account-1"));
        assert_eq!(keys[2].as_deref(), Some("client-2"));
        assert_eq!(keys[3].as_deref(), Some("account-3"));
    }
}
