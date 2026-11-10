//! The servers' front pages on the app's Home.
//!
//! Each media server the active profile can see is asked, in the background,
//! for the start of its own front page: what is half watched on it and its
//! first few rows. The answer is kept in memory and on disk, so Home draws
//! the rows of a server that is asleep, or not reached yet, at once, and
//! they change when it answers. A server's answer is the truth about what
//! is half watched on it: this headset's own records of that server are
//! then left off Home.

use fp_sources::{CatalogHome, CatalogRow, RowShape, SourceConfig, SourceKind, write_private};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Rows of a server Home shows.
pub const ROWS: usize = 4;

/// A copy this young is not asked for again.
const FRESH: Duration = Duration::from_secs(5 * 60);

/// A server that did not answer is asked again after this long.
const RETRY: Duration = Duration::from_secs(60);

/// The sources Home shows server rows of: media servers the active profile
/// may see, that are not locked. Their id and name.
pub fn shown(
    configs: &[SourceConfig],
    chosen: &crate::plex_profiles::Chosen,
    pins: &BTreeMap<String, String>,
    unlocked: &HashSet<String>,
) -> Vec<(String, String)> {
    let hidden = crate::plex_profiles::hidden(configs, chosen);
    configs
        .iter()
        .filter(|c| matches!(c.kind(), SourceKind::MediaBrowser | SourceKind::Plex))
        .filter(|c| !hidden.iter().any(|id| id == c.id()))
        .filter(|c| !crate::pin::locked(pins, unlocked, c.id()))
        .map(|c| (c.id().to_string(), c.name().to_string()))
        .collect()
}

/// A front page cut down to what Home shows: what is half watched, and the
/// first [`ROWS`] rows, the server's own or, for one with none, its next
/// episodes and newest titles. Its libraries are not kept.
pub fn cut(home: CatalogHome) -> CatalogHome {
    let mut rows = home.rows;
    if rows.is_empty() {
        if !home.next_up.is_empty() {
            rows.push(CatalogRow {
                id: "next-up".into(),
                title: tr!("jf-next-up").into(),
                shape: RowShape::Landscape,
                items: home.next_up,
                ..Default::default()
            });
        }
        for (library, items) in home.latest {
            rows.push(CatalogRow {
                id: format!("latest:{}", library.id),
                title: tr!("jf-latest-in", library = library.name.as_str()),
                shape: RowShape::of(&items),
                items,
                library: Some(library.id),
                ..Default::default()
            });
        }
    }
    rows.retain(|r| !r.items.is_empty());
    rows.truncate(ROWS);
    CatalogHome {
        resume: home.resume,
        rows,
        ..Default::default()
    }
}

/// The folder the copies of the front pages are kept in.
fn folder() -> PathBuf {
    fp_core::dirs::cache_dir().join("server-home")
}

/// The file holding the copy of `source`'s front page.
fn file(source: &str) -> PathBuf {
    let name: String = source
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    folder().join(format!("{name}.json"))
}

/// What is written to disk: the page and when it was asked for. It holds
/// `plex://` and server locations only, never a token.
#[derive(Serialize, Deserialize)]
struct Saved {
    /// Unix seconds.
    at: u64,
    home: CatalogHome,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The text of a copy as it is written.
fn encode(home: &CatalogHome, at: u64) -> Option<Vec<u8>> {
    serde_json::to_vec(&Saved {
        at,
        home: home.clone(),
    })
    .ok()
}

/// Keeps the copy of `source`'s front page on disk (blocking: run it on a
/// job thread).
pub fn save(source: &str, home: &CatalogHome) {
    let Some(data) = encode(home, unix_now()) else {
        return;
    };
    if let Err(e) = write_private(&file(source), &data) {
        log::info!("cannot keep the front page of {source}: {e}");
    }
}

/// The front pages last seen, and when each was asked for.
#[derive(Default)]
pub struct ServerHomes {
    /// The page of each source and when it was current, Unix seconds.
    pages: HashMap<String, (u64, CatalogHome)>,
    /// When each was last asked for.
    asked: HashMap<String, Instant>,
    /// Changes whenever a page does, so what was built from them is built
    /// again.
    generation: u64,
}

impl ServerHomes {
    /// Reads the copies kept on disk for `sources`; one that cannot be read
    /// is as if there were none.
    pub fn load(&mut self, sources: &[(String, String)]) {
        for (id, _) in sources {
            if self.pages.contains_key(id) {
                continue;
            }
            let Ok(data) = std::fs::read(file(id)) else {
                continue;
            };
            if let Ok(saved) = serde_json::from_slice::<Saved>(&data) {
                self.pages.insert(id.clone(), (saved.at, saved.home));
                self.generation += 1;
            }
        }
    }

    /// The page last seen of `source`.
    pub fn get(&self, source: &str) -> Option<&CatalogHome> {
        self.pages.get(source).map(|(_, home)| home)
    }

    /// Whether a page of `source` is known: its answer is the truth about
    /// what is half watched there.
    pub fn has(&self, source: &str) -> bool {
        self.pages.contains_key(source)
    }

    /// Counts changes to the pages.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether `source` should be asked for its front page: it has none
    /// that is young, and was not asked for a moment ago.
    pub fn due(&self, source: &str) -> bool {
        self.due_at(source, unix_now(), Instant::now())
    }

    fn due_at(&self, source: &str, now: u64, instant: Instant) -> bool {
        let young = self
            .pages
            .get(source)
            .is_some_and(|(at, _)| now.saturating_sub(*at) < FRESH.as_secs());
        let busy = self
            .asked
            .get(source)
            .is_some_and(|t| instant.saturating_duration_since(*t) < FRESH);
        !young && !busy
    }

    /// `source` is being asked for its front page now.
    pub fn asking(&mut self, source: &str) {
        self.asked.insert(source.to_string(), Instant::now());
    }

    /// `source` answered.
    pub fn put(&mut self, source: &str, home: CatalogHome) {
        self.pages.insert(source.to_string(), (unix_now(), home));
        self.generation += 1;
    }

    /// `source` did not answer: the page it had stays, and it is asked again
    /// soon.
    pub fn failed(&mut self, source: &str) {
        let early = Instant::now()
            .checked_sub(FRESH - RETRY)
            .unwrap_or_else(Instant::now);
        self.asked.insert(source.to_string(), early);
    }

    /// Everything is out of date: something was watched, and the servers
    /// have a newer story to tell.
    pub fn expire(&mut self) {
        for (at, _) in self.pages.values_mut() {
            *at = 0;
        }
        self.asked.clear();
    }

    /// Forgets `source`: its server or its profile changed, so what it showed
    /// is not for whoever opens it next. Its copy on disk goes too.
    pub fn forget(&mut self, source: &str) {
        if self.pages.remove(source).is_some() {
            self.generation += 1;
        }
        self.asked.remove(source);
        let _ = std::fs::remove_file(file(source));
    }
}

/// Where the featured video at the top of Home comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    /// What is half watched here.
    Local,
    /// The first of what a server has half watched (index into the servers).
    Server(usize),
    /// The last video watched here.
    LastWatched,
}

/// One part of Home, from the top.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    Hero(Pick),
    /// Continue watching, from this headset.
    LocalContinue,
    /// What a server has half watched.
    ServerContinue(usize),
    /// A row of a server's front page.
    ServerRow {
        server: usize,
        row: usize,
    },
    Favorites,
    Sources,
}

/// The parts of Home in the order they are drawn: the featured video,
/// Continue watching, each server's Continue watching and first rows, the
/// favourites and the sources. `servers` are the front pages of the servers
/// shown; a part with nothing in it is left out.
pub fn layout(
    local_continue: usize,
    last_watched: bool,
    favorites: usize,
    servers: &[&CatalogHome],
) -> Vec<Block> {
    let mut blocks = Vec::new();
    let first_server = servers.iter().position(|h| !h.resume.is_empty());
    let pick = if local_continue > 0 {
        Some(Pick::Local)
    } else if let Some(i) = first_server {
        Some(Pick::Server(i))
    } else {
        last_watched.then_some(Pick::LastWatched)
    };
    blocks.extend(pick.map(Block::Hero));
    if local_continue > 0 {
        blocks.push(Block::LocalContinue);
    }
    for (i, home) in servers.iter().enumerate() {
        if !home.resume.is_empty() {
            blocks.push(Block::ServerContinue(i));
        }
    }
    for (server, home) in servers.iter().enumerate() {
        for (row, r) in home.rows.iter().enumerate().take(ROWS) {
            if !r.items.is_empty() {
                blocks.push(Block::ServerRow { server, row });
            }
        }
    }
    if favorites > 0 {
        blocks.push(Block::Favorites);
    }
    blocks.push(Block::Sources);
    blocks
}

#[cfg(test)]
mod tests {
    use super::*;
    use fp_sources::plex::auth::{
        PlexAccount, PlexConnection, PlexCredentials, PlexProfile, PlexResource, linked_source_id,
    };
    use fp_sources::{CatalogItem, CatalogKind, PlexConfig};

    fn item(id: &str) -> CatalogItem {
        CatalogItem {
            id: id.into(),
            name: format!("Title {id}"),
            kind: CatalogKind::Movie,
            location: format!("plex://server/{id}"),
            poster: Some(format!("plex://server/{id}/poster")),
            ..Default::default()
        }
    }

    fn row(id: &str, n: usize) -> CatalogRow {
        CatalogRow {
            id: id.into(),
            title: id.into(),
            items: (0..n).map(|i| item(&format!("{id}{i}"))).collect(),
            ..Default::default()
        }
    }

    fn home(resume: usize, rows: &[(&str, usize)]) -> CatalogHome {
        CatalogHome {
            resume: (0..resume).map(|i| item(&format!("r{i}"))).collect(),
            rows: rows.iter().map(|(id, n)| row(id, *n)).collect(),
            ..Default::default()
        }
    }

    fn plex(id: &str) -> SourceConfig {
        SourceConfig::Plex(PlexConfig {
            id: id.into(),
            name: format!("Server {id}"),
            url: "http://10.0.0.2:32400".into(),
            access_token: "a-secret-server-token".into(),
            ..Default::default()
        })
    }

    #[test]
    fn the_disk_copy_has_no_token() {
        let secret = "a-secret-server-token";
        let config = plex("nas");
        assert!(matches!(&config, SourceConfig::Plex(p) if p.access_token == secret));
        let page = cut(home(2, &[("recent", 3)]));
        let text = String::from_utf8(encode(&page, 7).unwrap()).unwrap();
        assert!(!text.contains(secret));
        assert!(!text.to_lowercase().contains("token"));
        assert!(text.contains("plex://server/r0"));
        // And it reads back as it was written.
        let saved: Saved = serde_json::from_str(&text).unwrap();
        assert_eq!(saved.at, 7);
        assert_eq!(saved.home, page);
    }

    #[test]
    fn a_page_is_cut_to_the_first_rows_without_libraries() {
        let mut full = home(
            1,
            &[("a", 2), ("b", 0), ("c", 1), ("d", 1), ("e", 1), ("f", 1)],
        );
        full.libraries = vec![item("lib")];
        let page = cut(full);
        assert!(page.libraries.is_empty());
        assert_eq!(page.resume.len(), 1);
        let ids: Vec<&str> = page.rows.iter().map(|r| r.id.as_str()).collect();
        // The empty row is left out; four rows remain.
        assert_eq!(ids, ["a", "c", "d", "e"]);
    }

    #[test]
    fn a_server_with_no_rows_shows_its_next_episodes_and_newest() {
        let library = CatalogItem {
            id: "section:1".into(),
            name: "Films".into(),
            kind: CatalogKind::Library,
            ..Default::default()
        };
        let page = cut(CatalogHome {
            next_up: vec![item("n1")],
            latest: vec![(library, vec![item("l1"), item("l2")])],
            ..Default::default()
        });
        let ids: Vec<&str> = page.rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["next-up", "latest:section:1"]);
        assert_eq!(page.rows[1].items.len(), 2);
    }

    #[test]
    fn a_fresh_copy_is_not_asked_for_again_but_a_failure_is_retried() {
        let mut homes = ServerHomes::default();
        assert!(homes.due("nas"), "nothing yet");
        homes.put("nas", home(0, &[("a", 1)]));
        assert!(!homes.due("nas"));
        assert!(homes.has("nas") && homes.get("nas").is_some());
        let later = Instant::now() + Duration::from_secs(600);
        assert!(homes.due_at("nas", unix_now() + 600, later), "old by then");
        homes.expire();
        assert!(homes.due("nas"));
        homes.asking("nas");
        assert!(!homes.due("nas"), "asked a moment ago");
        homes.failed("nas");
        let soon = Instant::now() + RETRY + Duration::from_secs(1);
        assert!(
            homes.due_at("nas", unix_now(), soon),
            "tried again after a minute"
        );
    }

    #[test]
    fn forgetting_a_server_drops_its_page() {
        let mut homes = ServerHomes::default();
        homes.put("nas", home(1, &[]));
        let before = homes.generation();
        homes.forget("nas");
        assert!(!homes.has("nas"));
        assert!(homes.generation() > before);
    }

    fn account() -> PlexCredentials {
        let mut c = PlexCredentials::manual("account-token".into(), "client".into());
        c.account = Some(Box::new(PlexAccount {
            id: 1,
            uuid: "acct".into(),
            title: "Owner".into(),
            home: true,
        }));
        c
    }

    fn own(machine: &str) -> PlexConfig {
        PlexConfig {
            id: linked_source_id("acct", machine, None),
            name: format!("Server {machine}"),
            url: "http://10.0.0.2:32400".into(),
            machine_identifier: machine.into(),
            access_token: "server-token".into(),
            auth: Some(account()),
            ..Default::default()
        }
    }

    fn profile(uuid: &str) -> PlexProfile {
        PlexProfile {
            id: 2,
            uuid: uuid.into(),
            title: "Guest".into(),
            protected: false,
            restricted: false,
            token: format!("{uuid}-plextv"),
        }
    }

    fn resource(machine: &str) -> PlexResource {
        PlexResource {
            name: "Server".into(),
            machine_identifier: machine.into(),
            access_token: Some("guest-server".into()),
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

    #[test]
    fn locked_and_inactive_profile_sources_stay_off_home() {
        let base = own("m1");
        let set = crate::plex_profiles::siblings(
            std::slice::from_ref(&base),
            &profile("guest"),
            &[resource("m1")],
        );
        let mut configs: Vec<SourceConfig> =
            set.sources.into_iter().map(SourceConfig::Plex).collect();
        let own_id = base.id.clone();
        configs.push(SourceConfig::Plex(base));
        configs.push(plex("manual"));
        configs.push(SourceConfig::Local(fp_sources::LocalConfig {
            id: "videos".into(),
            name: "Videos".into(),
            root: "/home/deck/Videos".into(),
        }));
        let mut chosen = crate::plex_profiles::Chosen::new();
        let (pins, unlocked) = (BTreeMap::new(), HashSet::new());

        // The account itself is watching: its profile's source stays off, and
        // so does a folder source.
        let ids = |list: Vec<(String, String)>| -> Vec<String> {
            list.into_iter().map(|(id, _)| id).collect()
        };
        let now = ids(shown(&configs, &chosen, &pins, &unlocked));
        assert!(now.contains(&own_id) && now.contains(&"manual".to_string()));
        assert!(!now.contains(&"videos".to_string()));
        assert_eq!(now.len(), 2);

        // The profile is watching: its source shows, the account's does not.
        chosen.insert("acct".into(), "guest".into());
        let now = ids(shown(&configs, &chosen, &pins, &unlocked));
        assert!(!now.contains(&own_id));
        assert_eq!(now.len(), 2);

        // A locked source is left off until it is unlocked.
        let mut pins = BTreeMap::new();
        pins.insert("manual".to_string(), "hash".to_string());
        let now = ids(shown(&configs, &chosen, &pins, &unlocked));
        assert!(!now.contains(&"manual".to_string()));
        let unlocked: HashSet<String> = ["manual".to_string()].into();
        let now = ids(shown(&configs, &chosen, &pins, &unlocked));
        assert!(now.contains(&"manual".to_string()));
    }

    #[test]
    fn home_order_is_hero_continue_server_rows_favorites_sources() {
        let a = home(2, &[("a1", 1), ("a2", 1)]);
        let b = home(0, &[("b1", 1)]);
        let c = home(1, &[]);
        let blocks = layout(3, true, 2, &[&a, &b, &c]);
        assert_eq!(
            blocks,
            [
                Block::Hero(Pick::Local),
                Block::LocalContinue,
                Block::ServerContinue(0),
                Block::ServerContinue(2),
                Block::ServerRow { server: 0, row: 0 },
                Block::ServerRow { server: 0, row: 1 },
                Block::ServerRow { server: 1, row: 0 },
                Block::Favorites,
                Block::Sources,
            ]
        );
    }

    #[test]
    fn the_hero_is_local_then_a_server_then_the_last_watched() {
        let with = home(1, &[]);
        let without = home(0, &[("a", 1)]);
        let first = |blocks: Vec<Block>| blocks.into_iter().next();
        assert_eq!(
            first(layout(1, true, 0, &[&with])),
            Some(Block::Hero(Pick::Local))
        );
        assert_eq!(
            first(layout(0, true, 0, &[&without, &with])),
            Some(Block::Hero(Pick::Server(1)))
        );
        assert_eq!(
            first(layout(0, true, 0, &[&without])),
            Some(Block::Hero(Pick::LastWatched))
        );
        // Nothing to show but the sources.
        assert_eq!(layout(0, false, 0, &[]), [Block::Sources]);
    }
}
