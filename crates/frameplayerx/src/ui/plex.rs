//! Plex source setup: account linking, server choice and a tested manual token.

use super::sources::{address_hint, name_hint, text_field};
use super::theme::{self, Weight};
use super::widgets::{self, Kind};
use super::{Action, SourceForm};
use fp_sources::plex::auth::{LINK_URL, PlexCredentials, PlexPin, PlexResource, linked_source_id};
use fp_sources::{PlexConfig, SourceConfig};
use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Default)]
pub enum LinkState {
    #[default]
    Idle,
    Requesting,
    Waiting {
        pin: PlexPin,
        polled: Instant,
        polling: bool,
    },
    /// The account's servers, to switch on and add together. Nothing that
    /// happens to one server replaces this state, so the list and the
    /// sign-in stay for the next try.
    Servers {
        credentials: PlexCredentials,
        servers: Vec<PlexResource>,
        /// Machine identifiers of the servers switched on.
        ticked: BTreeSet<String>,
        /// What became of the servers asked for, by machine identifier; a
        /// server without an entry is `Ready`.
        rows: HashMap<String, ServerRow>,
    },
    Connecting,
    Connected,
    Failed(String),
}

/// Where one server of the list stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerRow {
    Ready,
    Checking,
    /// Now a source, under this name.
    Added(String),
    /// Could not be added; the reason is shown, and it can be tried again.
    Failed(String),
}

impl LinkState {
    /// The list for a freshly signed-in account. Servers it owns that are
    /// not sources yet start switched on; shared ones are left to choose.
    pub fn servers(
        credentials: PlexCredentials,
        servers: Vec<PlexResource>,
        configs: &[SourceConfig],
    ) -> Self {
        let ticked = servers
            .iter()
            .filter(|s| s.owned && !is_linked(configs, &credentials, &s.machine_identifier))
            .map(|s| s.machine_identifier.clone())
            .collect();
        Self::Servers {
            credentials,
            servers,
            ticked,
            rows: HashMap::new(),
        }
    }
}

/// The name of the source that already plays `machine` for the account
/// `credentials` belong to, as the account itself rather than one of its
/// profiles.
pub(crate) fn linked_name(
    configs: &[SourceConfig],
    credentials: &PlexCredentials,
    machine: &str,
) -> Option<String> {
    let key = credentials
        .account
        .as_ref()
        .map_or(&credentials.client_identifier, |a| &a.uuid);
    for config in configs {
        if let SourceConfig::Plex(p) = config
            && p.profile.is_none()
            && p.machine_identifier == machine
            && p.account_key().as_ref() == Some(key)
        {
            return Some(p.name.clone());
        }
    }
    None
}

fn is_linked(configs: &[SourceConfig], credentials: &PlexCredentials, machine: &str) -> bool {
    linked_name(configs, credentials, machine).is_some()
}

/// How many servers "Add" would check now.
pub(crate) fn pending(state: &LinkState, configs: &[SourceConfig]) -> usize {
    wanted(state, configs).len()
}

/// A server being checked, or just added, is not asked for again.
fn settled(row: Option<&ServerRow>) -> bool {
    matches!(row, Some(ServerRow::Checking | ServerRow::Added(_)))
}

/// The servers "Add" would check now: switched on, not sources yet and not
/// being checked.
fn wanted<'a>(state: &'a LinkState, configs: &[SourceConfig]) -> Vec<&'a PlexResource> {
    let LinkState::Servers {
        credentials,
        servers,
        ticked,
        rows,
    } = state
    else {
        return Vec::new();
    };
    servers
        .iter()
        .filter(|s| {
            let machine = &s.machine_identifier;
            ticked.contains(machine)
                && !is_linked(configs, credentials, machine)
                && !settled(rows.get(machine))
        })
        .collect()
}

/// The id a server gets as a new source: stable for this account and
/// server when the account is known (so adding it again, later or after
/// removing it, reaches what was watched), else made from the time. The
/// source being edited keeps its own id for its own server.
fn new_source_id(
    credentials: &PlexCredentials,
    machine: &str,
    editing: Option<&PlexConfig>,
    now_ms: u64,
    index: usize,
) -> String {
    if let Some(old) = editing
        && old.machine_identifier == machine
    {
        return old.id.clone();
    }
    match &credentials.account {
        Some(account) => linked_source_id(&account.uuid, machine, None),
        None => format!("plex-{now_ms}-{index}"),
    }
}

/// Starts checking the servers switched on: their rows say so, the list
/// stays, and the action asks the app to test them and add those that work.
fn start_batch(f: &mut SourceForm, configs: &[SourceConfig], now_ms: u64) -> Option<Action> {
    let LinkState::Servers { credentials, .. } = &f.plex.state else {
        return None;
    };
    let credentials = credentials.clone();
    let chosen = wanted(&f.plex.state, configs);
    let servers: Vec<PlexResource> = chosen.into_iter().cloned().collect();
    let editing = match &f.editing {
        Some(SourceConfig::Plex(old)) => Some(old),
        _ => None,
    };
    // A typed name is for a single server; several keep their own.
    let typed = f.name.trim();
    let name = (servers.len() == 1 && !typed.is_empty()).then_some(typed);
    let mut checks = Vec::new();
    let mut refused = Vec::new();
    for (index, server) in servers.iter().enumerate() {
        let machine = &server.machine_identifier;
        let id = new_source_id(&credentials, machine, editing, now_ms, index);
        match server_config(server, &credentials, &id, name, f) {
            Some(config) => checks.push(config),
            None => refused.push(machine.clone()),
        }
    }
    let generation = f.plex.generation;
    if let LinkState::Servers { rows, .. } = &mut f.plex.state {
        for config in &checks {
            rows.insert(config.machine_identifier.clone(), ServerRow::Checking);
        }
        for machine in refused {
            let reason = tr!("plex-server-no-address").to_string();
            rows.insert(machine, ServerRow::Failed(reason));
        }
    }
    (!checks.is_empty()).then_some(Action::PlexAddServers {
        generation,
        configs: checks,
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ConnectionMode {
    #[default]
    Local,
    Direct,
    Relay,
}

impl ConnectionMode {
    pub(super) fn from_config(config: &PlexConfig) -> Self {
        if config.local_only {
            Self::Local
        } else if config.allow_relay {
            Self::Relay
        } else {
            Self::Direct
        }
    }

    fn apply(self, config: &mut PlexConfig) {
        config.local_only = self == Self::Local;
        config.allow_relay = self == Self::Relay;
    }

    fn label(self) -> &'static str {
        match self {
            Self::Local => tr!("plex-local-only"),
            Self::Direct => tr!("plex-connection-direct"),
            Self::Relay => tr!("plex-connection-relay"),
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Local => tr!("plex-connection-local-desc"),
            Self::Direct => tr!("plex-connection-direct-desc"),
            Self::Relay => tr!("plex-connection-relay-desc"),
        }
    }
}

pub struct Connect {
    pub generation: u64,
    pub state: LinkState,
    pub manual: bool,
    pub token: String,
    pub connection: ConnectionMode,
    pub selected: Option<PlexConfig>,
    /// The code was claimed by a different account than the source being
    /// edited, so its servers are added as new sources.
    pub other_account: bool,
}

impl Default for Connect {
    fn default() -> Self {
        Self {
            generation: 0,
            state: LinkState::Idle,
            manual: false,
            token: String::new(),
            connection: ConnectionMode::default(),
            selected: None,
            other_account: false,
        }
    }
}

impl Connect {
    /// Recheck an already chosen server before saving a different policy.
    fn set_connection(&mut self, connection: ConnectionMode) -> Option<PlexConfig> {
        if self.connection == connection {
            return None;
        }
        self.connection = connection;
        let mut config = self.selected.clone()?;
        connection.apply(&mut config);
        self.state = LinkState::Connecting;
        Some(config)
    }

    /// Invalidates any outstanding job before changing how to sign in.
    fn reset(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.state = LinkState::Idle;
        self.selected = None;
        self.other_account = false;
    }

    fn use_token(&mut self, manual: bool) {
        if self.manual != manual {
            self.reset();
            self.manual = manual;
        }
    }
}

fn link_rows(r: &mut widgets::Rows, f: &mut SourceForm, actions: &mut Vec<Action>) {
    match f.plex.state.clone() {
        LinkState::Waiting { pin, .. } => {
            r.row_with(
                tr!("quick-connect-code"),
                Some(tr!("plex-link-waiting")),
                |ui| {
                    ui.label(
                        egui::RichText::new(LINK_URL.trim_start_matches("https://"))
                            .font(theme::font(Weight::Bold, 28.0))
                            .color(theme::ACCENT),
                    );
                },
                |ui| {
                    if widgets::button_sized(ui, None, tr!("common-cancel"), Kind::Secondary, 36.0)
                        .clicked()
                    {
                        f.plex.reset();
                    }
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(
                            pin.code
                                .chars()
                                .map(|c| c.to_string())
                                .collect::<Vec<_>>()
                                .join(" "),
                        )
                        .font(theme::font(Weight::Bold, 32.0))
                        .color(theme::ACCENT),
                    );
                },
            );
        }
        LinkState::Idle | LinkState::Requesting | LinkState::Failed(_) => {
            let requesting = matches!(f.plex.state, LinkState::Requesting);
            r.row(tr!("plex-sign-in"), Some(tr!("plex-link-desc")), |ui| {
                if ui
                    .add_enabled_ui(!requesting, |ui| {
                        widgets::button(
                            ui,
                            Some(super::icons::KEY_RETURN),
                            tr!("quick-connect-get-code"),
                            Kind::Primary,
                        )
                    })
                    .inner
                    .clicked()
                {
                    f.plex.selected = None;
                    f.plex.state = LinkState::Requesting;
                    actions.push(Action::PlexLinkStart);
                }
                if requesting {
                    ui.spinner();
                }
            });
        }
        LinkState::Connected | LinkState::Servers { .. } => {
            r.row(
                tr!("plex-sign-in"),
                matches!(f.plex.state, LinkState::Connected)
                    .then_some(tr!("quick-connect-authorized-desc")),
                |ui| {
                    if widgets::button_sized(ui, None, tr!("common-clear"), Kind::Secondary, 36.0)
                        .clicked()
                    {
                        f.plex.reset();
                    }
                    ui.label(egui::RichText::new(super::icons::CHECK_CIRCLE).color(theme::OK));
                },
            );
        }
        LinkState::Connecting => {}
    }
}

/// The selected config is returned only after a worker has tested access.
pub fn form(
    ui: &mut egui::Ui,
    f: &mut SourceForm,
    id: String,
    configs: &[SourceConfig],
    actions: &mut Vec<Action>,
) -> Option<fp_sources::SourceConfig> {
    if let LinkState::Waiting {
        pin,
        polled,
        polling,
    } = &mut f.plex.state
    {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
        // While a poll is out, its answer decides: a code claimed just
        // before it ran out still counts.
        if pin.expired() && !*polling {
            f.plex.state = LinkState::Failed(tr!("plex-link-expired").into());
        } else if !*polling && polled.elapsed() >= Duration::from_secs(2) {
            *polling = true;
            *polled = Instant::now();
            actions.push(Action::PlexLinkPoll {
                generation: f.plex.generation,
                pin: pin.clone(),
            });
        }
    }
    widgets::rows(ui, |r| {
        r.row(
            tr!("source-name"),
            Some(tr!("source-name-desc-server")),
            |ui| text_field(ui, &mut f.name, name_hint(f.kind), false),
        );
        r.row(tr!("source-sign-in-with"), None, |ui| {
            if widgets::chip(ui, tr!("plex-token"), f.plex.manual).clicked() {
                f.plex.use_token(true);
            }
            if widgets::chip(ui, tr!("plex-sign-in"), !f.plex.manual).clicked() {
                f.plex.use_token(false);
            }
        });
        if f.plex.manual {
            r.row(tr!("source-address"), None, |ui| {
                text_field(ui, &mut f.address, address_hint(f.kind), false);
            });
            r.row(tr!("plex-token"), None, |ui| {
                text_field(ui, &mut f.plex.token, "", true);
            });
            r.switch(
                tr!("source-self-signed"),
                Some(tr!("source-self-signed-desc")),
                &mut f.insecure_tls,
            );
        } else {
            link_rows(r, f, actions);
        }
        r.switch(
            tr!("source-parallel-downloads"),
            Some(tr!("source-parallel-downloads-desc")),
            &mut f.parallel_downloads,
        );
        r.content(|ui| {
            widgets::section_label(ui, tr!("plex-connection-mode"));
            ui.add_enabled_ui(!matches!(f.plex.state, LinkState::Connecting), |ui| {
                ui.horizontal_wrapped(|ui| {
                    for mode in [
                        ConnectionMode::Local,
                        ConnectionMode::Direct,
                        ConnectionMode::Relay,
                    ] {
                        if widgets::chip(ui, mode.label(), f.plex.connection == mode).clicked()
                            && let Some(config) = f.plex.set_connection(mode)
                        {
                            actions.push(Action::PlexConnect(config));
                        }
                    }
                });
            });
            ui.add_space(6.0);
            let mut descriptions = vec![f.plex.connection.description()];
            if f.plex.manual && f.plex.connection == ConnectionMode::Local {
                descriptions.push(tr!("plex-connection-manual-local-desc"));
            }
            descriptions.extend([
                tr!("plex-connection-pass-desc"),
                tr!("plex-connection-subnet-desc"),
            ]);
            for description in descriptions {
                ui.label(
                    egui::RichText::new(description)
                        .font(theme::font(Weight::Regular, 15.0))
                        .color(theme::TEXT_2),
                );
            }
        });
    });
    ui.add_space(12.0);
    // Signing in again to a source entered with a token makes a separate
    // source; to a linked one it signs that account in again.
    if let Some(SourceConfig::Plex(old)) = &f.editing
        && old.auth.is_none()
    {
        ui.label(tr!("plex-new-account-source"));
    }
    if f.plex.other_account {
        ui.label(tr!("plex-another-account"));
    }
    let state = f.plex.state.clone();
    match state {
        LinkState::Connecting => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr!("settings-device-connecting"));
            });
        }
        LinkState::Servers { .. } => server_list(ui, f, configs, actions),
        LinkState::Connected if f.plex.manual => {
            ui.label(tr!("quick-connect-authorized-desc"));
        }
        LinkState::Failed(error) => {
            ui.label(egui::RichText::new(error).color(theme::WARN));
        }
        _ => {}
    }
    let busy = matches!(
        f.plex.state,
        LinkState::Requesting | LinkState::Waiting { .. } | LinkState::Connecting
    );
    if !busy
        && f.plex.manual
        && widgets::button(ui, None, tr!("plex-connect"), Kind::Secondary).clicked()
    {
        if fp_sources::plex::is_web_app(&f.address) {
            f.plex.use_token(false);
            f.plex.state = LinkState::Requesting;
            actions.push(Action::PlexLinkStart);
        } else if !f.address.trim().is_empty() && !f.plex.token.trim().is_empty() {
            let id = replacement_id(f, id.clone());
            let cfg = manual_config(f, id);
            f.plex.selected = None;
            f.plex.state = LinkState::Connecting;
            actions.push(Action::PlexConnect(cfg));
        }
    }
    if f.plex.manual
        && f.plex
            .selected
            .as_ref()
            .is_some_and(|c| c.url != f.address.trim() || c.access_token != f.plex.token.trim())
    {
        f.plex.selected = None;
    }
    ui.add_space(16.0);
    if let Some(mut cfg) = f.plex.selected.clone()
        && matches!(f.plex.state, LinkState::Connected)
    {
        if !f.name.trim().is_empty() {
            cfg.name = f.name.trim().into();
        }
        f.plex.connection.apply(&mut cfg);
        cfg.insecure_tls = f.insecure_tls;
        cfg.parallel_downloads = f.parallel_downloads;
        if widgets::button(
            ui,
            Some(super::icons::CHECK),
            tr!("common-save"),
            Kind::Primary,
        )
        .clicked()
        {
            return Some(fp_sources::SourceConfig::Plex(cfg));
        }
    } else if !matches!(f.plex.state, LinkState::Servers { .. }) {
        ui.add_enabled_ui(false, |ui| {
            widgets::button(
                ui,
                Some(super::icons::CHECK),
                tr!("common-save"),
                Kind::Primary,
            )
        });
        ui.label(egui::RichText::new(tr!("plex-test-required")).color(theme::TEXT_3));
    }
    None
}

static READY: ServerRow = ServerRow::Ready;

/// The account's servers, each with a switch and where it stands, and the
/// button that adds every one switched on.
fn server_list(
    ui: &mut egui::Ui,
    f: &mut SourceForm,
    configs: &[SourceConfig],
    actions: &mut Vec<Action>,
) {
    widgets::section_label(ui, tr!("plex-server-select"));
    if let LinkState::Servers {
        credentials,
        servers,
        ticked,
        rows,
    } = &mut f.plex.state
    {
        if servers.is_empty() {
            ui.label(tr!("plex-no-servers"));
        }
        widgets::rows(ui, |r| {
            for server in servers.iter() {
                let machine = &server.machine_identifier;
                let owner = if server.owned {
                    tr!("plex-server-owned")
                } else {
                    tr!("plex-server-shared")
                };
                let row = rows.get(machine).unwrap_or(&READY);
                let linked = is_linked(configs, credentials, machine);
                let added = linked || matches!(row, ServerRow::Added(_));
                if added {
                    r.row(&server.name, Some(owner), |ui| {
                        let text = egui::RichText::new(tr!("plex-server-added"));
                        ui.label(text.color(theme::OK));
                    });
                    continue;
                }
                if *row == ServerRow::Checking {
                    r.row(&server.name, Some(owner), |ui| {
                        ui.spinner();
                        ui.label(tr!("plex-server-checking"));
                    });
                    continue;
                }
                let desc = match row {
                    ServerRow::Failed(reason) => format!("{owner} \u{b7} {reason}"),
                    _ => owner.to_string(),
                };
                let mut on = ticked.contains(machine);
                r.switch(&server.name, Some(&desc), &mut on);
                set_ticked(ticked, machine, on);
            }
        });
    }
    ui.add_space(12.0);
    let count = pending(&f.plex.state, configs);
    let checking = match &f.plex.state {
        LinkState::Servers { rows, .. } => rows.values().any(|r| *r == ServerRow::Checking),
        _ => false,
    };
    let label = tr!("plex-servers-add", count = count);
    let add = ui
        .add_enabled_ui(count > 0 && !checking, |ui| {
            widgets::button(ui, Some(super::icons::CHECK), &label, Kind::Primary)
        })
        .inner
        .clicked();
    if add
        && let Some(action) = start_batch(f, configs, fp_core::playback::now_ms())
    {
        actions.push(action);
    }
}

fn set_ticked(ticked: &mut BTreeSet<String>, machine: &str, on: bool) {
    if on {
        ticked.insert(machine.to_string());
    } else {
        ticked.remove(machine);
    }
}

fn replacement_id(f: &mut SourceForm, current: String) -> String {
    if let Some(fp_sources::SourceConfig::Plex(old)) = &f.editing
        && old.access_token != f.plex.token.trim()
    {
        f.editing = None;
        format!("plex-{}", fp_core::playback::now_ms())
    } else {
        current
    }
}

fn manual_config(f: &SourceForm, id: String) -> PlexConfig {
    let mut cfg = match &f.editing {
        Some(fp_sources::SourceConfig::Plex(c)) => c.clone(),
        _ => PlexConfig {
            id,
            name: "Plex".into(),
            url: String::new(),
            machine_identifier: String::new(),
            access_token: String::new(),
            connections: Vec::new(),
            auth: None,
            insecure_tls: false,
            parallel_downloads: false,
            max_height: None,
            local_only: true,
            allow_relay: false,
            ..Default::default()
        },
    };
    cfg.url = fp_sources::urlutil::with_http_scheme(f.address.trim());
    cfg.name = if f.name.trim().is_empty() {
        "Plex".into()
    } else {
        f.name.trim().to_string()
    };
    cfg.access_token = f.plex.token.trim().to_string();
    cfg.auth = None;
    cfg.connections.clear();
    cfg.insecure_tls = f.insecure_tls;
    cfg.parallel_downloads = f.parallel_downloads;
    f.plex.connection.apply(&mut cfg);
    cfg
}

/// `name` replaces the server's own name; the connection mode and the other
/// switches of the form apply.
fn server_config(
    server: &PlexResource,
    credentials: &PlexCredentials,
    id: &str,
    name: Option<&str>,
    f: &SourceForm,
) -> Option<PlexConfig> {
    let connection = server
        .connections
        .iter()
        .find(|c| c.local && c.protocol == "https")
        .or_else(|| server.connections.iter().find(|c| c.local))
        .or_else(|| server.connections.iter().find(|c| !c.relay))
        .or_else(|| server.connections.first())?;
    let mut config = PlexConfig {
        id: id.to_string(),
        name: name.map_or_else(|| server.name.clone(), str::to_string),
        url: connection.uri.clone(),
        machine_identifier: server.machine_identifier.clone(),
        access_token: server
            .access_token
            .clone()
            .unwrap_or_else(|| credentials.access_token.clone()),
        connections: server.connections.clone(),
        auth: Some(credentials.clone()),
        insecure_tls: f.insecure_tls,
        parallel_downloads: f.parallel_downloads,
        max_height: None,
        local_only: true,
        allow_relay: false,
        ..Default::default()
    };
    f.plex.connection.apply(&mut config);
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_sources::plex::auth::{PlexAccount, PlexConnection, PlexProfile};

    #[test]
    fn connection_modes_default_to_local_and_reach_both_setup_methods() {
        let mut form = SourceForm::default();
        assert_eq!(form.plex.connection, ConnectionMode::Local);
        let server = PlexResource {
            name: "Plex".into(),
            machine_identifier: "machine".into(),
            access_token: Some("server-token".into()),
            connections: vec![fp_sources::plex::auth::PlexConnection {
                uri: "https://server.plex.direct:32400".into(),
                address: Some("192.168.1.10".parse().unwrap()),
                port: Some(32400),
                local: true,
                relay: false,
                protocol: "https".into(),
            }],
            owned: true,
            provides: "server".into(),
        };
        let auth = PlexCredentials::manual("account-token".into(), "client".into());
        for (mode, local_only, allow_relay) in [
            (ConnectionMode::Local, true, false),
            (ConnectionMode::Direct, false, false),
            (ConnectionMode::Relay, false, true),
        ] {
            assert!(form.plex.set_connection(mode).is_none());
            assert_eq!(form.plex.connection, mode);
            assert!(matches!(form.plex.state, LinkState::Idle));
            let manual = manual_config(&form, "manual".into());
            let linked = server_config(&server, &auth, "id", None, &form).unwrap();
            for config in [manual, linked] {
                assert_eq!(
                    (config.local_only, config.allow_relay),
                    (local_only, allow_relay)
                );
                assert_eq!(ConnectionMode::from_config(&config), mode);
            }
        }
    }

    #[test]
    fn editing_preserves_saved_modes_and_rechecks_a_changed_connection_policy() {
        for (local_only, allow_relay, mode) in [
            (true, false, ConnectionMode::Local),
            (true, true, ConnectionMode::Local),
            (false, false, ConnectionMode::Direct),
            (false, true, ConnectionMode::Relay),
        ] {
            let mut saved = manual_config(&SourceForm::default(), "saved-source".into());
            saved.url = "https://server.plex.direct:32400".into();
            saved.access_token = "server-token".into();
            saved.auth = Some(PlexCredentials::manual(
                "account-token".into(),
                "client".into(),
            ));
            saved.local_only = local_only;
            saved.allow_relay = allow_relay;
            let mut form =
                super::super::sources::edit_form(&fp_sources::SourceConfig::Plex(saved.clone()));
            assert_eq!(form.plex.connection, mode);
            assert_eq!(form.plex.selected.as_ref(), Some(&saved));
            assert!(form.plex.set_connection(mode).is_none());
            assert!(matches!(form.plex.state, LinkState::Connected));
            let next = if mode == ConnectionMode::Local {
                ConnectionMode::Direct
            } else {
                ConnectionMode::Local
            };
            let checked = form.plex.set_connection(next).unwrap();
            assert!(matches!(form.plex.state, LinkState::Connecting));
            assert_eq!(checked.id, saved.id);
            assert_eq!(checked.auth, saved.auth);
            assert_eq!(checked.access_token, saved.access_token);
            assert_eq!(ConnectionMode::from_config(&checked), next);
        }
    }

    #[test]
    fn code_linking_is_default_and_switching_methods_discards_pending_work() {
        let mut form = SourceForm::default();
        assert!(!form.plex.manual);
        form.plex.generation = 17;
        form.plex.state = LinkState::Connecting;
        form.plex.selected = Some(manual_config(&form, "source".into()));
        form.plex.use_token(true);
        assert!(form.plex.manual);
        assert_ne!(
            form.plex.generation, 17,
            "a pending result must no longer match"
        );
        assert!(form.plex.selected.is_none());
        assert!(matches!(form.plex.state, LinkState::Idle));
        form.plex.use_token(false);
        assert!(!form.plex.manual);
        form.plex.state = LinkState::Requesting;
        let generation = form.plex.generation;
        form.plex.reset();
        assert_ne!(form.plex.generation, generation);
        assert!(matches!(form.plex.state, LinkState::Idle));
    }

    #[test]
    fn replacing_manual_credentials_gets_a_separate_source_identity() {
        let mut form = SourceForm {
            address: "http://127.0.0.1:32400".into(),
            plex: Connect {
                token: "old-token".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let original = manual_config(&form, "old-source".into());
        form.editing = Some(fp_sources::SourceConfig::Plex(original));
        assert_eq!(replacement_id(&mut form, "old-source".into()), "old-source");
        form.plex.token = "another-account".into();
        let id = replacement_id(&mut form, "old-source".into());
        assert_ne!(id, "old-source");
        assert!(form.editing.is_none());
        let new = manual_config(&form, id.clone());
        assert_eq!(new.id, id);
        assert_eq!(new.access_token, "another-account");
        assert!(new.machine_identifier.is_empty());
    }

    fn resource(name: &str, machine: &str, owned: bool) -> PlexResource {
        PlexResource {
            name: name.into(),
            machine_identifier: machine.into(),
            access_token: Some(format!("{machine}-token")),
            connections: vec![PlexConnection {
                uri: format!("https://{machine}.plex.direct:32400"),
                address: None,
                port: Some(32400),
                local: true,
                relay: false,
                protocol: "https".into(),
            }],
            owned,
            provides: "server".into(),
        }
    }

    /// A server of the account's own, named after its identifier.
    fn own(machine: &str) -> PlexResource {
        resource(machine, machine, true)
    }

    fn signed_in(uuid: &str) -> PlexCredentials {
        let mut credentials = PlexCredentials::manual("account-token".into(), "client".into());
        credentials.account = Some(Box::new(PlexAccount {
            id: 1,
            uuid: uuid.into(),
            title: "Owner".into(),
            home: false,
        }));
        credentials
    }

    fn list_form(servers: Vec<PlexResource>, configs: &[SourceConfig]) -> SourceForm {
        let mut form = SourceForm::default();
        form.plex.state = LinkState::servers(signed_in("account-uuid-1"), servers, configs);
        form
    }

    fn ticked(form: &SourceForm) -> Vec<String> {
        match &form.plex.state {
            LinkState::Servers { ticked, .. } => ticked.iter().cloned().collect(),
            _ => Vec::new(),
        }
    }

    fn row(form: &SourceForm, machine: &str) -> Option<ServerRow> {
        match &form.plex.state {
            LinkState::Servers { rows, .. } => rows.get(machine).cloned(),
            _ => None,
        }
    }

    fn asked(action: Option<Action>) -> Vec<PlexConfig> {
        let Some(Action::PlexAddServers { configs, .. }) = action else {
            panic!("no servers were asked for");
        };
        configs
    }

    /// A source already playing `machine` as the account of `signed_in`.
    fn source_of(machine: &str, name: &str) -> SourceConfig {
        SourceConfig::Plex(PlexConfig {
            id: format!("existing-{machine}"),
            name: name.into(),
            machine_identifier: machine.into(),
            auth: Some(signed_in("account-uuid-1")),
            ..Default::default()
        })
    }

    #[test]
    fn ticking_two_servers_asks_for_both_and_keeps_the_list() {
        let servers = vec![
            resource("Home", "m1", true),
            resource("Home TV", "m2", true),
            resource("Friend", "m3", false),
        ];
        let mut form = list_form(servers, &[]);
        // Your own servers start switched on; shared ones are yours to choose.
        assert_eq!(ticked(&form), ["m1", "m2"]);
        form.plex.connection = ConnectionMode::Direct;
        let configs = asked(start_batch(&mut form, &[], 1000));
        assert_eq!(configs.len(), 2);
        let names: Vec<&str> = configs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["Home", "Home TV"]);
        assert_ne!(configs[0].id, configs[1].id);
        let expected = linked_source_id("account-uuid-1", "m1", None);
        assert_eq!(configs[0].id, expected);
        assert!(configs.iter().all(|c| !c.local_only && !c.allow_relay));
        // The list and the sign-in stay; both rows say they are checked.
        assert!(matches!(form.plex.state, LinkState::Servers { .. }));
        assert_eq!(row(&form, "m1"), Some(ServerRow::Checking));
        assert_eq!(row(&form, "m2"), Some(ServerRow::Checking));
        assert_eq!(row(&form, "m3"), None);
        // Pressing again while they are checked asks for nothing.
        assert!(start_batch(&mut form, &[], 1001).is_none());
    }

    #[test]
    fn servers_of_an_account_plex_would_not_name_get_distinct_ids() {
        let unnamed = PlexCredentials::manual("account-token".into(), "client".into());
        let mut form = SourceForm::default();
        form.plex.state = LinkState::servers(unnamed, vec![own("m1"), own("m2")], &[]);
        let configs = asked(start_batch(&mut form, &[], 42));
        assert_eq!(configs[0].id, "plex-42-0");
        assert_eq!(configs[1].id, "plex-42-1");
    }

    #[test]
    fn a_server_already_linked_shows_added_and_is_not_asked_again() {
        let configs = [source_of("m1", "Home")];
        let credentials = signed_in("account-uuid-1");
        let found = linked_name(&configs, &credentials, "m1");
        assert_eq!(found.as_deref(), Some("Home"));
        assert!(!is_linked(&configs, &credentials, "m2"));
        // Another account's source for the same server is not this one.
        assert!(!is_linked(&configs, &signed_in("account-uuid-2"), "m1"));
        // Nor is a profile's.
        let profile = PlexConfig {
            machine_identifier: "m2".into(),
            auth: Some(credentials.clone()),
            profile: Some(Box::new(PlexProfile {
                id: 7,
                uuid: "profile-1".into(),
                title: "Kid".into(),
                protected: false,
                restricted: true,
                token: "profile-token".into(),
            })),
            ..Default::default()
        };
        let only = [SourceConfig::Plex(profile)];
        assert!(!is_linked(&only, &credentials, "m2"));
        let mut form = list_form(vec![own("m1"), own("m2")], &configs);
        assert_eq!(ticked(&form), ["m2"]);
        // Even switched on by hand, it is not asked for again.
        if let LinkState::Servers { ticked, .. } = &mut form.plex.state {
            ticked.insert("m1".into());
        }
        let checks = asked(start_batch(&mut form, &configs, 1));
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].machine_identifier, "m2");
        assert_eq!(row(&form, "m1"), None);
    }

    #[test]
    fn a_failed_server_keeps_the_list_and_can_be_retried() {
        let mut form = list_form(vec![own("m1"), own("m2")], &[]);
        assert_eq!(asked(start_batch(&mut form, &[], 1)).len(), 2);
        // The app reports one server added and the other not answering.
        if let LinkState::Servers { rows, .. } = &mut form.plex.state {
            rows.insert("m1".into(), ServerRow::Failed("timed out".into()));
            rows.insert("m2".into(), ServerRow::Added("m2".into()));
        }
        // Still the list, with the sign-in: nothing starts over.
        assert!(matches!(form.plex.state, LinkState::Servers { .. }));
        let again = asked(start_batch(&mut form, &[], 2));
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].machine_identifier, "m1");
        assert_eq!(row(&form, "m1"), Some(ServerRow::Checking));
        // A server Plex gives no address for says so instead of being asked.
        let mut lost = own("m3");
        lost.connections.clear();
        let mut form = list_form(vec![lost], &[]);
        assert!(start_batch(&mut form, &[], 3).is_none());
        assert!(matches!(row(&form, "m3"), Some(ServerRow::Failed(_))));
    }

    #[test]
    fn one_ticked_server_takes_the_typed_name() {
        let mut form = list_form(vec![own("m1"), own("m2")], &[]);
        form.name = "  Living room ".into();
        // Two servers keep their own names.
        let both = asked(start_batch(&mut form, &[], 1));
        let names: Vec<&str> = both.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["m1", "m2"]);
        let mut form = list_form(vec![own("m1"), own("m2")], &[]);
        form.name = "  Living room ".into();
        if let LinkState::Servers { ticked, .. } = &mut form.plex.state {
            ticked.remove("m2");
        }
        let one = asked(start_batch(&mut form, &[], 1));
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "Living room");
    }

    #[test]
    fn the_source_being_edited_keeps_its_id_for_its_own_server() {
        let credentials = signed_in("account-uuid-1");
        let old = PlexConfig {
            id: "old-id".into(),
            machine_identifier: "m1".into(),
            ..Default::default()
        };
        let kept = new_source_id(&credentials, "m1", Some(&old), 1, 0);
        assert_eq!(kept, "old-id");
        let other = new_source_id(&credentials, "m2", Some(&old), 1, 0);
        let expected = linked_source_id("account-uuid-1", "m2", None);
        assert_eq!(other, expected);
    }
}
